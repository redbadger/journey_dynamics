//! Intermediate representation produced by parsing `#[pii(...)]` annotations.
//!
//! [`PiiVariantModel`] and [`PiiFieldModel`] are the validated, structured form
//! of what the user wrote. They are consumed by the code-generation layer to emit
//! the four methods of [`PiiEventCodec`].

use zyn::syn::Ident;

// ── Field role ────────────────────────────────────────────────────────────────

/// The role of a field within a `#[pii]`-annotated variant.
#[derive(Debug, Clone)]
pub enum PiiFieldRole {
    /// The data-subject identifier (`#[pii(subject)]`).
    ///
    /// Exactly one field per PII variant must carry this role.  Its value is
    /// kept in plaintext in the encrypted payload so the correct DEK can be
    /// located on the read path without decrypting anything first.
    Subject,

    /// A non-PII field kept in plaintext (`#[pii(plaintext)]`).
    ///
    /// These fields survive encryption and shredding unchanged and are
    /// preserved verbatim in all four codec methods.
    Plaintext,

    /// A PII field that must be encrypted on write and decrypted (or
    /// redacted) on read (`#[pii(secret)]`).
    ///
    /// At least one field per PII variant must carry this role.
    Secret,
}

// ── Field model ───────────────────────────────────────────────────────────────

/// A single named field within a `#[pii]`-annotated variant, after parsing and
/// role assignment.
#[derive(Debug, Clone)]
pub struct PiiFieldModel {
    /// The field's identifier (e.g. `name`, `email`, `subject_id`).
    pub ident: Ident,
    /// The field's classified role.
    pub role: PiiFieldRole,
}

// ── Variant model ─────────────────────────────────────────────────────────────

/// A fully-parsed and validated `#[pii(...)]`-annotated enum variant.
#[derive(Debug, Clone)]
pub struct PiiVariantModel {
    /// The variant identifier (e.g. `PersonCaptured`).
    pub ident: Ident,

    /// The `event_type` string from `#[pii(event_type = "...")]`.
    ///
    /// This is the value returned by `DomainEvent::event_type()` for this
    /// variant and must match the `event_type` field in `SerializedEvent`.
    pub event_type: String,

    /// The encrypted-blob sentinel field name in the stored JSON payload.
    ///
    /// Defaults to `"encrypted_pii"`.  Override with
    /// `#[pii(sentinel = "encrypted_data")]`.
    pub sentinel: String,

    /// All fields of the variant in declaration order, each with a role.
    pub fields: Vec<PiiFieldModel>,
}

impl PiiVariantModel {
    /// Returns the single field carrying the [`PiiFieldRole::Subject`] role.
    ///
    /// # Panics
    ///
    /// Panics if the variant was not properly validated (i.e. there is not
    /// exactly one subject field).  This invariant is enforced by
    /// [`parse_pii_variants`](crate::parse::parse_pii_variants).
    pub fn subject_field(&self) -> &PiiFieldModel {
        self.fields
            .iter()
            .find(|f| matches!(f.role, PiiFieldRole::Subject))
            .expect("invariant: exactly one subject field — enforced by parse_pii_variants")
    }

    /// Returns an iterator over fields with the [`PiiFieldRole::Plaintext`] role.
    pub fn plaintext_fields(&self) -> impl Iterator<Item = &PiiFieldModel> {
        self.fields
            .iter()
            .filter(|f| matches!(f.role, PiiFieldRole::Plaintext))
    }

    /// Returns an iterator over fields with the [`PiiFieldRole::Secret`] role.
    pub fn secret_fields(&self) -> impl Iterator<Item = &PiiFieldModel> {
        self.fields
            .iter()
            .filter(|f| matches!(f.role, PiiFieldRole::Secret))
    }

    /// Returns `true` when there is exactly one secret field.
    ///
    /// When true, the codec passes the field's value directly as
    /// `plaintext_pii` (not wrapped in an object).  When false, all secret
    /// fields are bundled into a JSON object keyed by field name.
    pub fn is_single_secret(&self) -> bool {
        self.secret_fields().count() == 1
    }
}
