//! Parsing `#[pii(...)]` annotations on enum variants and their fields into the
//! validated [`PiiVariantModel`] / [`PiiFieldModel`] intermediate representation.
//!
//! The entry point is [`parse_pii_variants`].  It walks every variant in the
//! enum, skips variants that carry no `#[pii(...)]` attribute (they are
//! non-PII and will be passed through unchanged by the generated codec), and
//! converts the annotated ones into a [`PiiVariantModel`] after validating that
//! the field annotations satisfy all invariants.
//!
//! All errors are returned as [`syn::Error`] values so the proc-macro entry
//! point can convert them to `compile_error!` tokens with accurate source spans.

use zyn::syn::{
    self as syn, Attribute, Fields, Ident, LitStr, Variant, punctuated::Punctuated, token::Comma,
};

use crate::model::{PiiFieldModel, PiiFieldRole, PiiVariantModel, RedactValue};

// ── Raw (unparsed) attribute data ─────────────────────────────────────────────

/// Raw values parsed from `#[pii(event_type = "...", sentinel = "...")]` on a
/// variant.
struct RawVariantAttr {
    event_type: String,
    /// Defaults to `"encrypted_pii"` when not specified.
    sentinel: String,
}

/// Raw flags parsed from `#[pii(subject)]` / `#[pii(plaintext)]` /
/// `#[pii(secret)]` on a field, plus an optional explicit redaction
/// override `#[pii(secret, redact = "...")]`.
struct RawFieldAttr {
    subject: bool,
    plaintext: bool,
    secret: bool,
    redact: Option<String>,
}

// ── Attribute helpers ─────────────────────────────────────────────────────────

/// Returns the first `#[pii(...)]` attribute in `attrs`, or `None`.
fn find_pii_attr(attrs: &[Attribute]) -> Option<&Attribute> {
    attrs.iter().find(|a| a.path().is_ident("pii"))
}

/// Parse the key-value contents of a variant-level `#[pii(...)]` attribute.
///
/// Accepted keys:
/// - `event_type = "<string>"` (required)
/// - `sentinel = "<string>"` (optional, default `"encrypted_pii"`)
fn parse_variant_pii_attr(attr: &Attribute) -> syn::Result<RawVariantAttr> {
    let mut event_type: Option<String> = None;
    let mut sentinel = "encrypted_pii".to_string();

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("event_type") {
            let value: LitStr = meta.value()?.parse()?;
            event_type = Some(value.value());
            Ok(())
        } else if meta.path.is_ident("sentinel") {
            let value: LitStr = meta.value()?.parse()?;
            sentinel = value.value();
            Ok(())
        } else {
            Err(meta.error("unknown `#[pii]` option; expected `event_type` or `sentinel`"))
        }
    })?;

    let event_type = event_type.ok_or_else(|| {
        syn::Error::new_spanned(attr, "`event_type` is required on `#[pii(...)]`")
    })?;

    Ok(RawVariantAttr {
        event_type,
        sentinel,
    })
}

/// Parse the contents of a field-level `#[pii(...)]` attribute.
///
/// Accepted forms:
/// - bare flags: `subject`, `plaintext`, `secret`
/// - key-value: `redact = "<string>"` (only meaningful alongside `secret`)
fn parse_field_pii_attr(attr: &Attribute) -> syn::Result<RawFieldAttr> {
    let mut result = RawFieldAttr {
        subject: false,
        plaintext: false,
        secret: false,
        redact: None,
    };

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("subject") {
            result.subject = true;
            Ok(())
        } else if meta.path.is_ident("plaintext") {
            result.plaintext = true;
            Ok(())
        } else if meta.path.is_ident("secret") {
            result.secret = true;
            Ok(())
        } else if meta.path.is_ident("redact") {
            let value: LitStr = meta.value()?.parse()?;
            result.redact = Some(value.value());
            Ok(())
        } else {
            Err(meta.error(
                "unknown `#[pii]` option on field; expected `subject`, `plaintext`, `secret`, or `redact = \"...\"`",
            ))
        }
    })?;

    Ok(result)
}

// ── Field parsing ────────────────────────────────────────────────────────────

/// Returns `true` if the type's inferred redaction value is part of the
/// crate's contract and may not be overridden by `#[pii(secret, redact = "...")]`.
///
/// Currently: `String`, `Option<_>`, and `serde_json::Value`.
///
/// **Maintenance contract:** this list is hand-maintained alongside
/// [`infer_redact`]. If you add a new arm to `infer_redact` whose
/// redaction value is intended to be fixed (i.e. callers must not
/// override it), you must also add the type to this function. Types
/// that are intended to be overridable (e.g. `NaiveDate` under the
/// `chrono` feature) do **not** belong here.
fn has_fixed_default(ty: &syn::Type) -> bool {
    let last = match ty {
        syn::Type::Path(tp) => tp.path.segments.last().map(|seg| seg.ident.to_string()),
        _ => None,
    };
    matches!(
        last.as_deref(),
        Some("String") | Some("Option") | Some("Value")
    )
}

/// Infer the [`RedactValue`] for a secret field by inspecting the last path
/// segment of its type.
///
/// Returns an error for types that cannot be mapped automatically.
///
/// With the `chrono` feature enabled, `NaiveDate` is also recognized and
/// redacts to the default sentinel `"0000-01-01"`. Callers can still
/// override the sentinel per-field via `#[pii(secret, redact = "...")]`.
fn infer_redact(ty: &syn::Type) -> syn::Result<RedactValue> {
    let last_ident = match ty {
        syn::Type::Path(tp) => tp.path.segments.last().map(|seg| seg.ident.to_string()),
        _ => None,
    };

    match last_ident.as_deref() {
        Some("String") => Ok(RedactValue::Literal("[redacted]".to_string())),
        Some("Option") => Ok(RedactValue::Null),
        Some("Value") => Ok(RedactValue::EmptyObject),
        #[cfg(feature = "chrono")]
        Some("NaiveDate") => Ok(RedactValue::Literal("0000-01-01".to_string())),
        _ => Err(syn::Error::new_spanned(
            ty,
            "cannot infer redaction value for this type; \
             only `String`, `Option<_>`, and `serde_json::Value` are supported \
             out of the box. Enable the `chrono` feature for `NaiveDate`, \
             or specify `#[pii(secret, redact = \"...\")]` explicitly.",
        )),
    }
}

/// Convert a single named `syn::Field` into a [`PiiFieldModel`].
///
/// Errors if:
/// - the field has no `#[pii(...)]` attribute
/// - more than one role flag is set
fn parse_field(field: &syn::Field, variant_ident: &Ident) -> syn::Result<PiiFieldModel> {
    let field_ident = field
        .ident
        .as_ref()
        .expect("named field always has an ident");

    let pii_attr = find_pii_attr(&field.attrs).ok_or_else(|| {
        syn::Error::new_spanned(
            field_ident,
            format!(
                "field `{field_ident}` on PII variant `{variant_ident}` must be annotated \
                 with `#[pii(subject)]`, `#[pii(plaintext)]`, or `#[pii(secret)]`"
            ),
        )
    })?;

    let raw = parse_field_pii_attr(pii_attr)?;

    let role_count = [raw.subject, raw.plaintext, raw.secret]
        .iter()
        .filter(|&&b| b)
        .count();

    if role_count != 1 {
        return Err(syn::Error::new_spanned(
            field_ident,
            format!(
                "field `{field_ident}` must have exactly one `#[pii]` role: \
                 `subject`, `plaintext`, or `secret`"
            ),
        ));
    }

    let role = if raw.subject {
        PiiFieldRole::Subject
    } else if raw.plaintext {
        PiiFieldRole::Plaintext
    } else {
        PiiFieldRole::Secret
    };

    let redact = if matches!(role, PiiFieldRole::Secret) {
        match raw.redact {
            Some(override_val) => {
                if has_fixed_default(&field.ty) {
                    return Err(syn::Error::new_spanned(
                        field_ident,
                        "redact override is not allowed on `String`, `Option<_>`, \
                         or `serde_json::Value` fields — their redaction values \
                         are part of the crate's contract. Remove the \
                         `redact = \"...\"` argument, or wrap the field in a \
                         newtype if you need a different sentinel.",
                    ));
                }
                Some(RedactValue::Literal(override_val))
            }
            None => Some(infer_redact(&field.ty)?),
        }
    } else {
        if raw.redact.is_some() {
            return Err(syn::Error::new_spanned(
                field_ident,
                "`redact` only applies to `#[pii(secret)]` fields",
            ));
        }
        None
    };

    Ok(PiiFieldModel {
        ident: field_ident.clone(),
        role,
        redact,
    })
}

// ── Variant parsing ───────────────────────────────────────────────────────────

/// Convert a single `#[pii(...)]`-annotated `syn::Variant` into a
/// [`PiiVariantModel`].
///
/// Errors if:
/// - the variant does not have named fields
/// - any field parsing error occurs (see [`parse_field`])
/// - there is not exactly one `#[pii(subject)]` field
/// - there are no `#[pii(secret)]` fields
fn parse_pii_variant(variant: &Variant, raw: RawVariantAttr) -> syn::Result<PiiVariantModel> {
    let named = match &variant.fields {
        Fields::Named(f) => f,
        Fields::Unnamed(_) | Fields::Unit => {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                "`#[pii]` variants must have named fields",
            ));
        }
    };

    let fields: Vec<PiiFieldModel> = named
        .named
        .iter()
        .map(|f| parse_field(f, &variant.ident))
        .collect::<syn::Result<_>>()?;

    let subject_count = fields
        .iter()
        .filter(|f| matches!(f.role, PiiFieldRole::Subject))
        .count();

    if subject_count != 1 {
        return Err(syn::Error::new_spanned(
            &variant.ident,
            format!(
                "PII variant `{}` must have exactly one `#[pii(subject)]` field, found {subject_count}",
                variant.ident,
            ),
        ));
    }

    let secret_count = fields
        .iter()
        .filter(|f| matches!(f.role, PiiFieldRole::Secret))
        .count();

    if secret_count == 0 {
        return Err(syn::Error::new_spanned(
            &variant.ident,
            format!(
                "PII variant `{}` must have at least one `#[pii(secret)]` field",
                variant.ident,
            ),
        ));
    }

    Ok(PiiVariantModel {
        event_type: raw.event_type,
        sentinel: raw.sentinel,
        fields,
    })
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Walk every variant in the enum, parse and validate any that carry
/// `#[pii(...)]`, and return the resulting [`PiiVariantModel`] list.
///
/// Variants without `#[pii(...)]` are silently skipped — they will be passed
/// through unchanged by the generated codec.
///
/// Returns the first [`syn::Error`] encountered, if any.
pub fn parse_pii_variants(
    variants: &Punctuated<Variant, Comma>,
) -> syn::Result<Vec<PiiVariantModel>> {
    let mut result = Vec::new();

    for variant in variants {
        let Some(pii_attr) = find_pii_attr(&variant.attrs) else {
            continue; // non-PII variant — skip
        };

        let raw = parse_variant_pii_attr(pii_attr)?;
        result.push(parse_pii_variant(variant, raw)?);
    }

    Ok(result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use zyn::syn::{Data, DeriveInput, parse_quote};

    use super::*;

    fn variants_from(input: DeriveInput) -> Punctuated<Variant, Comma> {
        match input.data {
            Data::Enum(e) => e.variants,
            _ => panic!("expected enum"),
        }
    }

    #[test]
    fn skips_unannotated_variants() {
        let input: DeriveInput = parse_quote! {
            enum E {
                Started { id: uuid::Uuid },
                Completed,
            }
        };
        let result = parse_pii_variants(&variants_from(input)).unwrap();
        assert!(result.is_empty(), "non-PII variants must be skipped");
    }

    #[test]
    fn parses_single_pii_variant() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "PersonCaptured")]
                PersonCaptured {
                    #[pii(plaintext)] person_ref: String,
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret)] name: String,
                    #[pii(secret)] email: String,
                }
            }
        };
        let variants = variants_from(input);
        let result = parse_pii_variants(&variants).unwrap();
        assert_eq!(result.len(), 1);

        let v = &result[0];
        assert_eq!(v.event_type, "PersonCaptured");
        assert_eq!(v.sentinel, "encrypted_pii");
        assert_eq!(v.fields.len(), 4);
        assert!(!v.is_single_secret());
    }

    #[test]
    fn parses_custom_sentinel() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "PersonDetailsUpdated", sentinel = "encrypted_data")]
                PersonDetailsUpdated {
                    #[pii(plaintext)] person_ref: String,
                    #[pii(subject)]   subject_id: uuid::Uuid,
                    #[pii(secret)]    data: serde_json::Value,
                }
            }
        };
        let variants = variants_from(input);
        let result = parse_pii_variants(&variants).unwrap();

        let v = &result[0];
        assert_eq!(v.sentinel, "encrypted_data");
        assert!(v.is_single_secret());
    }

    #[test]
    fn parses_mixed_pii_and_non_pii_variants() {
        let input: DeriveInput = parse_quote! {
            enum E {
                Started { id: uuid::Uuid },
                #[pii(event_type = "Pii")]
                Pii {
                    #[pii(subject)]  subject_id: uuid::Uuid,
                    #[pii(secret)]   secret: String,
                },
                Completed,
            }
        };
        let variants = variants_from(input);
        let result = parse_pii_variants(&variants).unwrap();
        assert_eq!(
            result.len(),
            1,
            "only the annotated variant should be returned"
        );
        assert_eq!(result[0].event_type, "Pii");
    }

    #[test]
    fn errors_on_missing_event_type() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(sentinel = "x")]
                Bad {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret)]  secret: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("event_type"),
            "error should mention `event_type`, got: {err}"
        );
    }

    #[test]
    fn errors_on_unannotated_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "Bad")]
                Bad {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    unannotated: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("unannotated"),
            "error should mention the unannotated field, got: {err}"
        );
    }

    #[test]
    fn errors_on_missing_subject() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "Bad")]
                Bad {
                    #[pii(plaintext)] tag: String,
                    #[pii(secret)]    secret: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("subject"),
            "error should mention missing subject, got: {err}"
        );
    }

    #[test]
    fn errors_on_missing_secret() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "Bad")]
                Bad {
                    #[pii(subject)]   subject_id: uuid::Uuid,
                    #[pii(plaintext)] tag: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("secret"),
            "error should mention missing secret, got: {err}"
        );
    }

    #[test]
    fn errors_on_unit_variant() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "Bad")]
                Bad
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("named fields"),
            "error should mention named fields, got: {err}"
        );
    }

    #[test]
    fn errors_on_multiple_subject_fields() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "Bad")]
                Bad {
                    #[pii(subject)] subject_a: uuid::Uuid,
                    #[pii(subject)] subject_b: uuid::Uuid,
                    #[pii(secret)]  secret: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("subject"),
            "error should mention duplicate subject, got: {err}"
        );
    }

    #[test]
    fn parses_redact_override_on_unknown_secret_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret, redact = "fallback-value")] dob: MyDate,
                }
            }
        };
        let result = parse_pii_variants(&variants_from(input)).unwrap();
        let dob = result[0]
            .fields
            .iter()
            .find(|f| f.ident.to_string() == "dob")
            .expect("dob field must be present");
        match &dob.redact {
            Some(RedactValue::Literal(s)) => assert_eq!(s, "fallback-value"),
            other => panic!("expected RedactValue::Literal(\"fallback-value\"), got: {other:?}"),
        }
    }

    #[test]
    fn errors_on_redact_override_for_string_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret, redact = "x")] name: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("redact override"),
            "error should mention redact override, got: {err}"
        );
    }

    #[test]
    fn errors_on_redact_override_for_option_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret, redact = "x")] maybe: Option<String>,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("redact override"),
            "error should mention redact override, got: {err}"
        );
    }

    #[test]
    fn errors_on_redact_override_for_value_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret, redact = "x")] data: serde_json::Value,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("redact override"),
            "error should mention redact override, got: {err}"
        );
    }

    #[test]
    fn errors_on_redact_override_for_subject_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject, redact = "x")] subject_id: uuid::Uuid,
                    #[pii(secret)] name: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("only applies to"),
            "error should explain redact only applies to secret fields, got: {err}"
        );
    }

    #[test]
    fn errors_on_redact_override_for_plaintext_field() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(plaintext, redact = "x")] tag: String,
                    #[pii(secret)] name: String,
                }
            }
        };
        let err = parse_pii_variants(&variants_from(input)).unwrap_err();
        assert!(
            err.to_string().contains("only applies to"),
            "error should explain redact only applies to secret fields, got: {err}"
        );
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn parses_naive_date_with_default_redact() {
        let input: DeriveInput = parse_quote! {
            enum E {
                #[pii(event_type = "X")]
                X {
                    #[pii(subject)] subject_id: uuid::Uuid,
                    #[pii(secret)]  dob: chrono::NaiveDate,
                }
            }
        };
        let result = parse_pii_variants(&variants_from(input)).unwrap();
        let dob = result[0]
            .fields
            .iter()
            .find(|f| f.ident.to_string() == "dob")
            .expect("dob field must be present");
        match &dob.redact {
            Some(RedactValue::Literal(s)) => assert_eq!(s, "0000-01-01"),
            other => panic!("expected RedactValue::Literal(\"0000-01-01\"), got: {other:?}"),
        }
    }
}
