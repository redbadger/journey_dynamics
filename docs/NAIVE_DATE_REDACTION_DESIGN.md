# NaiveDate redaction support for `cqrs-es-crypto-derive`

## Goal

Allow `chrono::NaiveDate` to appear as a `#[pii(secret)]` field in events
processed by `#[derive(PiiCodec)]`, so the field can be transparently
encrypted, decrypted, and redacted (after crypto-shredding) like the
already-supported types (`String`, `Option<_>`, `serde_json::Value`).

The redacted form must still parse back into `NaiveDate`, so callers can
deserialize shredded events without runtime errors.

## Background

`crates/cqrs-es-crypto-derive/src/parse.rs::infer_redact` (lines 116–133)
classifies the secret-field type by its last path segment and returns the
[`RedactValue`] to emit when the DEK has been deleted:

| Last segment | Redaction value |
|--------------|-----------------|
| `String`     | `"[redacted]"`  |
| `Option`     | `null`          |
| `Value`      | `{}`            |

Any other type is rejected at compile time with
`"cannot infer redaction value for this type"`.

A bare `NaiveDate` field therefore fails to compile. `Option<NaiveDate>`
already works (it matches the `Option` branch and redacts to `null`); this
spec is only about the bare, non-optional case.

## Design

### Cargo feature `chrono`

A new opt-in Cargo feature, named `chrono`, is added to
`cqrs-es-crypto-derive`. It is re-exported from `cqrs-es-crypto` so consumers
can enable it through the umbrella crate.

```toml
# cqrs-es-crypto-derive/Cargo.toml
[features]
chrono = []
```

```toml
# cqrs-es-crypto/Cargo.toml
[features]
chrono = ["cqrs-es-crypto-derive/chrono"]
```

The feature does **not** add a runtime dependency on the `chrono` crate. The
proc-macro inspects the field type by string-matching its last path segment
(the existing mechanism), so it never needs to link against `chrono` itself.
Whether the consumer's event enum imports `chrono::NaiveDate` from the real
chrono crate or some re-export is the consumer's concern.

The feature is named `chrono` (not `naive-date`) so future support for
`NaiveDateTime`, `NaiveTime`, `DateTime<Tz>`, etc. can be added inside the
same feature without a breaking rename.

### Default redaction for `NaiveDate`

When the `chrono` feature is enabled, `infer_redact` recognizes `NaiveDate`
and returns:

```rust
RedactValue::Literal("0000-01-01".to_string())
```

`"0000-01-01"` is an obviously-sentinel date — clearly not a real value,
unambiguous when audited, and parses cleanly as a `NaiveDate`. This matches
the existing convention of "one canonical default per type".

When the `chrono` feature is **off**, `NaiveDate` errors with the existing
"cannot infer redaction value for this type" message — no behaviour change.

### Per-field override: `#[pii(secret, redact = "...")]`

A new key-value form is added to the field-level `#[pii(...)]` attribute:

```rust
#[pii(secret, redact = "1900-01-01")]
date_of_birth: NaiveDate,
```

The override:

- Accepts a string literal only.
- Emits the literal verbatim as a JSON string (`RedactValue::Literal`). The
  proc-macro does not validate that the string parses as the field's type —
  if it doesn't, downstream `serde_json::from_value::<NaiveDate>` will fail
  at runtime, the same as if the redacted JSON were tampered with by any
  other means.
- Is **rejected at compile time** on fields whose type is `String`,
  `Option<_>`, or `serde_json::Value`. Their inferred defaults
  (`"[redacted]"`, `null`, `{}`) are part of the crate's contract and not
  user-overridable.
- Is **rejected at compile time** on `subject` and `plaintext` fields —
  those have no redaction value at all.
- Is otherwise accepted, regardless of feature flags. Any type that does
  not have an inferred default can be redacted by supplying an override.

This means the `chrono` feature's only role is to supply a *default*
sentinel for `NaiveDate`. With the feature off, callers can still redact a
`NaiveDate` field by writing `#[pii(secret, redact = "0000-01-01")]`
explicitly — they just don't get a default.

### Decision matrix

| Field type | Feature `chrono` | Override supplied? | Result |
|------------|------------------|--------------------|--------|
| `String` / `Option<_>` / `Value` | n/a | no  | inferred default (`"[redacted]"` / `null` / `{}`). |
| `String` / `Option<_>` / `Value` | n/a | yes | compile error: override not allowed on inferred-default types. |
| `NaiveDate` | on  | no  | default `"0000-01-01"`. |
| `NaiveDate` | on  | yes | override value. |
| `NaiveDate` | off | no  | compile error: cannot infer redaction value. |
| `NaiveDate` | off | yes | override value. |
| any other unknown type | n/a | no  | compile error: cannot infer redaction value. |
| any other unknown type | n/a | yes | override value. |

### Architecture

All changes are confined to the derive crate. The runtime crate
(`cqrs-es-crypto`) only gains a passthrough feature definition.

| File | Change |
|------|--------|
| `cqrs-es-crypto-derive/Cargo.toml` | Add `chrono` feature. |
| `cqrs-es-crypto-derive/src/parse.rs` | Extend `RawFieldAttr` with `redact: Option<String>`. Extend `parse_field_pii_attr` to recognize the `redact` key. In `parse_field`: reject the override on non-secret fields; for secret fields, if an override is supplied, reject it for inferred-default types (`String`/`Option`/`Value`) and otherwise use it directly; if no override is supplied, fall back to `infer_redact`. Behind `#[cfg(feature = "chrono")]`, add the `NaiveDate` arm to `infer_redact`. |
| `cqrs-es-crypto/Cargo.toml` | Add `chrono` passthrough feature. |
| `cqrs-es-crypto-derive/tests/integration.rs` | Behind `#[cfg(feature = "chrono")]`, add an integration test variant whose secret field is `chrono::NaiveDate`. |
| `cqrs-es-crypto-derive/Cargo.toml` (dev-dependencies) | Add `chrono = "0.4"` (no features). |
| `cqrs-es-crypto-derive/src/lib.rs` (doc comment) | Document the new attribute and the feature. |
| `cqrs-es-crypto/README.md` (and crate-level doc in `lib.rs`) | Mention the feature. |

`model.rs` does not change: `RedactValue::Literal(String)` already covers
both the inferred `NaiveDate` default and any user-supplied override.
None of the codegen `*_arm` functions change.

### Validation rules (compile-time errors)

| Situation | Error message |
|-----------|---------------|
| `redact = "..."` on a `String`/`Option<_>`/`Value` field | `"redact override is not allowed on this type; the inferred redaction value is fixed"` |
| `redact = "..."` on a `subject` or `plaintext` field | `"redact only applies to #[pii(secret)] fields"` |
| `redact = <non-string>` (e.g. integer literal, bool) | `"expected string literal"` (raised by `LitStr::parse`). |
| Unknown-typed secret field with no override | existing `"cannot infer redaction value for this type"`. |

### Data flow

No change to runtime data flow. On the read path with a deleted DEK:

1. `redact()` (generated method) emits the JSON payload with each
   `secret` field replaced by its `RedactValue`.
2. For a `NaiveDate` field this is the JSON string `"0000-01-01"` (or the
   override).
3. The caller deserializes the payload as usual; `serde_json` parses the
   string back into `NaiveDate`.

### Testing

Unit tests in `parse.rs` (none feature-gated):

1. `parses_redact_override_on_unknown_secret_field` —
   `#[pii(secret, redact = "x")]` on a field of some non-inferred type
   (`MyDate` works fine, no chrono needed) produces
   `RedactValue::Literal("x")`.
2. `errors_on_redact_override_for_string_field` — overriding a `String`
   secret is rejected.
3. `errors_on_redact_override_for_option_field` — overriding an
   `Option<String>` secret is rejected.
4. `errors_on_redact_override_for_value_field` — overriding a `Value`
   secret is rejected.
5. `errors_on_redact_override_for_subject_field` — `redact` on a
   `#[pii(subject)]` field is rejected.
6. `errors_on_redact_override_for_plaintext_field` — `redact` on a
   `#[pii(plaintext)]` field is rejected.

Unit tests in `parse.rs` gated on `#[cfg(feature = "chrono")]`:

7. `parses_naive_date_with_default_redact` — `NaiveDate` without an
   explicit override produces `RedactValue::Literal("0000-01-01")`.

Integration test in `tests/integration.rs` (feature-gated):

8. `naive_date_redacts_to_default_sentinel_when_key_deleted` — persist an
   event with a `NaiveDate` secret field; delete the DEK; read the event
   back; assert the field equals `"0000-01-01"`.
9. `naive_date_override_redacts_to_custom_sentinel_when_key_deleted` —
   same shape but with `redact = "1900-01-01"`; assert the override
   value appears.

The integration tests use the real `chrono` crate so the test exercises the
exact type a downstream caller will use. `chrono` is added as a regular
unconditional `dev-dependency` of `cqrs-es-crypto-derive` (Cargo can't
gate dev-dependencies on a top-level feature). Test code that references
`chrono::NaiveDate` is itself wrapped in `#[cfg(feature = "chrono")]`, so
nothing chrono-related compiles when the feature is off; the unused
dev-dep just sits in the lockfile.

No chrono features are needed — neither the test code nor the proc-macro
actually parses, formats, or serdes a `NaiveDate` value. The type only
needs to resolve at compile time; the proc-macro inspects its last path
segment by string, and the integration test constructs JSON payloads
directly with `serde_json::json!` (using string literals like
`"2025-01-15"`).

### Documentation

`cqrs-es-crypto-derive/src/lib.rs` crate-level doc comment gets a new
sub-section showing:

```rust,ignore
#[derive(PiiCodec)]
enum MyEvent {
    #[pii(event_type = "PersonCaptured")]
    PersonCaptured {
        #[pii(subject)]                   subject_id: uuid::Uuid,
        #[pii(secret)]                    name: String,
        // NaiveDate redaction (default `"0000-01-01"`, requires the
        // `chrono` feature):
        #[pii(secret)]                        dob: chrono::NaiveDate,
        // Override the default redaction sentinel:
        #[pii(secret, redact = "1900-01-01")] dod: chrono::NaiveDate,
    },
}
```

`cqrs-es-crypto/README.md` gets one paragraph describing the `chrono`
feature.

## Out of scope

- Other chrono types (`NaiveDateTime`, `NaiveTime`, `DateTime<Tz>`).
  Easy follow-up under the same feature flag if needed.
- Allowing override of inferred-default types (`String`, `Option<_>`,
  `Value`).
- Validating that the override string is parseable as the field's actual
  type. The proc-macro doesn't link against chrono and can't easily call
  `NaiveDate::parse_from_str`. If a caller writes a malformed override,
  they discover it at runtime when the redacted event is deserialized.
- Non-string literal overrides (integers, bools, JSON objects). The
  current secret types only need string or `null`/`{}`, so a single
  string-literal form covers everything we plan to add for the foreseeable
  future.
