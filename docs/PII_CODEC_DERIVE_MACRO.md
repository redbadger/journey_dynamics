# `#[derive(PiiCodec)]` — Design Document

| | |
|---|---|
| **Crate** | `cqrs-es-crypto` |
| **Feature** | Derive `PiiEventCodec` from annotated event enums |
| **Status** | Proposed |
| **Depends on** | [CRYPTO_CRATE_EXTRACTION.md](./CRYPTO_CRATE_EXTRACTION.md) (Implemented) |

---

## Table of Contents

1. [Motivation](#motivation)
2. [Current State](#current-state)
3. [Design Goals](#design-goals)
4. [Annotation Design](#annotation-design)
   - [Variant-level attributes](#variant-level-attributes)
   - [Field-level attributes](#field-level-attributes)
   - [Full annotated example](#full-annotated-example)
5. [Generated Code](#generated-code)
   - [classify](#classify)
   - [extract_encrypted](#extract_encrypted)
   - [reconstruct](#reconstruct)
   - [redact](#redact)
6. [Crate Structure](#crate-structure)
7. [Implementation with `zyn`](#implementation-with-zyn)
   - [Why `zyn`](#why-zyn)
   - [Attribute parsing](#attribute-parsing)
   - [Derive entry point](#derive-entry-point)
   - [Elements](#elements)
   - [Testing](#testing)
8. [Redaction Strategy](#redaction-strategy)
9. [Sentinel Naming](#sentinel-naming)
10. [Edge Cases and Validation](#edge-cases-and-validation)
11. [Step-by-Step Execution Plan](#step-by-step-execution-plan)
12. [Risk Assessment](#risk-assessment)
13. [Future Work](#future-work)

---

## Motivation

Today, implementing `PiiEventCodec` for a domain event enum requires writing
~200 lines of repetitive, pattern-matched code across four methods. Each PII
variant needs a `classify`, `extract_encrypted`, `reconstruct`, and `redact`
arm — and all four must agree on field names, sentinel names, JSON keys, and
redaction values. Getting any of these out of sync is a silent correctness bug.

A derive macro eliminates this by making the event enum definition the single
source of truth. Developers annotate which variants carry PII, which fields are
secret, and which field identifies the data subject — the macro generates the
entire `PiiEventCodec` implementation.

---

## Current State

The hand-written `JourneyPiiCodec` in `crates/journey_dynamics/src/pii_codec.rs`
handles two PII variants (`PersonCaptured`, `PersonDetailsUpdated`) and passes
through all others. The implementation follows a rigid pattern for each variant:

| Method | Per-variant pattern |
|--------|-------------------|
| `classify` | Match on `event_type` string → extract `subject_id` → bundle `#[pii(secret)]` fields into `plaintext_pii` JSON → build encrypted payload closure |
| `extract_encrypted` | Match on `event_type` string → check sentinel presence → extract `subject_id` → base64-decode ciphertext and nonce |
| `reconstruct` | Match on `event_type` string → read plaintext fields from stored event → merge with decrypted PII fields |
| `redact` | Match on `event_type` string → read plaintext fields from stored event → replace PII fields with redacted values |

Every variant follows the same template. The only things that vary are:

- The event-type string (e.g. `"PersonCaptured"`)
- The outer JSON key (same as the event-type string under serde external tagging)
- Which fields are `plaintext`, which is the `subject`, and which are `secret`
- The sentinel field name (`encrypted_pii` vs `encrypted_data`)
- The redacted value per field (`"[redacted]"`, `null`, `{}`)

This is exactly the kind of mechanical, data-driven boilerplate that a derive
macro excels at eliminating.

---

## Design Goals

1. **Single source of truth** — the event enum definition, with annotations,
   completely determines the codec behaviour.
2. **Compile-time correctness** — misconfigurations (missing `subject` field,
   no `secret` fields on a PII variant, etc.) are caught at compile time with
   clear error messages.
3. **Wire-format compatibility** — the generated code must produce the exact
   same JSON payloads, sentinel names, and redacted values as today's
   hand-written implementation.
4. **Opt-in** — only variants annotated with `#[pii(...)]` are treated as PII.
   All others are passed through unchanged.
5. **Reasonable defaults, full overrides** — sentinel names and redaction
   values have sensible defaults but can be overridden per-variant or
   per-field.

---

## Annotation Design

### Variant-level attributes

Applied to enum variants that carry PII:

```rust
#[pii(event_type = "PersonCaptured")]
```

| Attribute | Required | Default | Purpose |
|-----------|----------|---------|---------|
| `event_type` | Yes | — | The `event_type` string in `SerializedEvent` that identifies this variant |
| `sentinel` | No | `"encrypted_pii"` | The JSON field name used for the encrypted blob in the stored payload |

Variants without `#[pii(...)]` are non-PII and generate no match arms.

### Field-level attributes

Applied to fields within a `#[pii(...)]` variant:

| Attribute | Purpose | Rules |
|-----------|---------|-------|
| `#[pii(subject)]` | Identifies the data-subject UUID field — used to look up the DEK | Exactly one per PII variant. Must be a `Uuid`-like type. |
| `#[pii(plaintext)]` | Kept in the clear in the encrypted payload — not encrypted, not redacted | Zero or more per variant. |
| `#[pii(secret)]` | Encrypted on write, decrypted on read, redacted when the DEK is gone | At least one per PII variant. |

Fields with no `#[pii(...)]` annotation on a PII variant are an error — the
macro requires every field to be explicitly classified, preventing accidental
PII exposure.

#### Redaction overrides on `#[pii(secret)]`

```rust
#[pii(secret, redact = "[redacted]")]
name: String,

#[pii(secret, redact_with = "serde_json::json!({})")]
data: Value,
```

| Option | Purpose |
|--------|---------|
| `redact = "<literal>"` | Use a JSON string literal as the redacted value |
| `redact_with = "<expr>"` | Use an arbitrary expression that produces a `serde_json::Value` |

When neither is specified, the macro infers a default from the field type:

| Type pattern | Default redacted value |
|-------------|----------------------|
| `String` | `"[redacted]"` |
| `Option<_>` | `null` |
| `Value` / `serde_json::Value` | `{}` |
| Anything else | Compile error: "specify `redact` or `redact_with`" |

### Full annotated example

```rust
use cqrs_es_crypto::PiiCodec;

#[derive(PiiCodec)]
enum JourneyEvent {
    // ── Non-PII variants (no annotation) ──────────────────────────
    Started {
        id: Uuid,
    },
    Modified {
        step: String,
        data: Value,
    },
    WorkflowEvaluated {
        suggested_actions: Vec<String>,
    },
    StepProgressed {
        from_step: Option<String>,
        to_step: String,
    },
    Completed,
    SubjectForgotten {
        subject_id: Uuid,
    },

    // ── PII variants ─────────────────────────────────────────────
    #[pii(event_type = "PersonCaptured")]
    PersonCaptured {
        #[pii(plaintext)]
        person_ref: String,
        #[pii(subject)]
        subject_id: Uuid,
        #[pii(secret)]
        name: String,
        #[pii(secret)]
        email: String,
        #[pii(secret)]
        phone: Option<String>,
    },

    #[pii(event_type = "PersonDetailsUpdated", sentinel = "encrypted_data")]
    PersonDetailsUpdated {
        #[pii(plaintext)]
        person_ref: String,
        #[pii(subject)]
        subject_id: Uuid,
        #[pii(secret)]
        data: Value,
    },
}
```

This replaces the entire hand-written `JourneyPiiCodec` struct and its
`impl PiiEventCodec`.

---

## Generated Code

The macro generates a struct and a `PiiEventCodec` implementation. The struct
name is derived from the enum name: `{Enum}PiiCodec` (e.g.
`JourneyEventPiiCodec`).

Below is a sketch of what the macro produces for each trait method given the
annotated example above.

### `classify`

For each `#[pii]` variant, a match arm is generated:

```rust
fn classify(&self, event: &SerializedEvent) -> Option<PiiFields> {
    match event.event_type.as_str() {
        "PersonCaptured" => {
            let key = "PersonCaptured";
            let subject_id_str = event.payload[key]["subject_id"].as_str()?.to_string();
            let subject_id = Uuid::parse_str(&subject_id_str).ok()?;

            // Read plaintext fields
            let person_ref_str = event.payload[key]["person_ref"]
                .as_str().unwrap_or("").to_string();

            // Bundle secret fields
            let inner = event.payload[key].as_object()?;
            let plaintext_pii = serde_json::json!({
                "name":  inner.get("name").cloned().unwrap_or(Value::Null),
                "email": inner.get("email").cloned().unwrap_or(Value::Null),
                "phone": inner.get("phone").cloned().unwrap_or(Value::Null),
            });

            Some(PiiFields {
                subject_id,
                plaintext_pii,
                build_encrypted_payload: Box::new(move |sentinel| {
                    serde_json::json!({
                        "PersonCaptured": {
                            "person_ref":    person_ref_str,
                            "subject_id":    subject_id_str,
                            "encrypted_pii": sentinel.ciphertext_b64,
                            "nonce":         sentinel.nonce_b64,
                        }
                    })
                }),
            })
        }
        "PersonDetailsUpdated" => {
            // ... same pattern, using sentinel = "encrypted_data"
            // and plaintext_pii = event.payload[key]["data"].clone()
        }
        _ => None,
    }
}
```

**Key generation rule for `plaintext_pii`:** when there are multiple `secret`
fields, they are bundled into a JSON object keyed by field name. When there is
exactly one `secret` field, its value is used directly (not wrapped in an
object). This matches the current hand-written behaviour where `PersonCaptured`
bundles `{name, email, phone}` but `PersonDetailsUpdated` passes `data`
through directly.

### `extract_encrypted`

```rust
fn extract_encrypted(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
    match event.event_type.as_str() {
        "PersonCaptured" => {
            let key = "PersonCaptured";
            event.payload[key].get("encrypted_pii")?; // sentinel check
            let subject_id = Uuid::parse_str(
                event.payload[key]["subject_id"].as_str()?
            ).ok()?;
            let ciphertext = BASE64.decode(
                event.payload[key]["encrypted_pii"].as_str()?
            ).ok()?;
            let nonce = BASE64.decode(
                event.payload[key]["nonce"].as_str()?
            ).ok()?;
            Some(EncryptedPiiExtract { subject_id, ciphertext, nonce })
        }
        "PersonDetailsUpdated" => {
            let key = "PersonDetailsUpdated";
            event.payload[key].get("encrypted_data")?; // custom sentinel
            // ... same pattern
        }
        _ => None,
    }
}
```

### `reconstruct`

```rust
fn reconstruct(
    &self,
    event: &SerializedEvent,
    plaintext_pii: &Value,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    match event.event_type.as_str() {
        "PersonCaptured" => {
            let key = "PersonCaptured";
            let person_ref = event.payload[key]["person_ref"].clone();
            let subject_id = event.payload[key]["subject_id"].clone();
            Ok(serde_json::json!({
                "PersonCaptured": {
                    "person_ref": person_ref,
                    "subject_id": subject_id,
                    // Multiple secrets → access by field name from the PII blob
                    "name":       plaintext_pii["name"],
                    "email":      plaintext_pii["email"],
                    "phone":      plaintext_pii["phone"],
                }
            }))
        }
        "PersonDetailsUpdated" => {
            let key = "PersonDetailsUpdated";
            let person_ref = event.payload[key]["person_ref"].clone();
            let subject_id = event.payload[key]["subject_id"].clone();
            Ok(serde_json::json!({
                "PersonDetailsUpdated": {
                    "person_ref": person_ref,
                    "subject_id": subject_id,
                    // Single secret → plaintext_pii IS the value directly
                    "data":       plaintext_pii,
                }
            }))
        }
        _ => Ok(event.payload.clone()),
    }
}
```

### `redact`

```rust
fn redact(
    &self,
    event: &SerializedEvent,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    match event.event_type.as_str() {
        "PersonCaptured" => {
            let key = "PersonCaptured";
            let person_ref = event.payload[key]["person_ref"].clone();
            let subject_id = event.payload[key]["subject_id"].clone();
            Ok(serde_json::json!({
                "PersonCaptured": {
                    "person_ref": person_ref,
                    "subject_id": subject_id,
                    "name":       "[redacted]",   // String default
                    "email":      "[redacted]",   // String default
                    "phone":      null,            // Option<_> default
                }
            }))
        }
        "PersonDetailsUpdated" => {
            // ...
            //  "data": {},                        // Value default
        }
        _ => Ok(event.payload.clone()),
    }
}
```

---

## Crate Structure

Proc macros must live in a dedicated crate. Following the ecosystem convention
(`serde` → `serde_derive`, `thiserror` → `thiserror-impl`):

```
crates/
├── cqrs-es-crypto/                  # existing library crate
│   ├── Cargo.toml                   # gains optional dep on cqrs-es-crypto-derive
│   └── src/lib.rs                   # re-exports #[derive(PiiCodec)] behind feature
│
└── cqrs-es-crypto-derive/           # new proc-macro crate
    ├── Cargo.toml
    └── src/
        ├── lib.rs                   # #[zyn::derive] entry point
        ├── model.rs                 # Parsed intermediate representation
        ├── classify.rs              # @element for classify arms
        ├── extract.rs               # @element for extract_encrypted arms
        ├── reconstruct.rs           # @element for reconstruct arms
        └── redact.rs                # @element for redact arms
```

### Dependency graph

```
cqrs-es-crypto-derive
  ├── zyn          (proc macro framework)
  ├── syn          (via zyn re-export)
  ├── quote        (via zyn re-export)
  └── proc-macro2  (via zyn re-export)

cqrs-es-crypto
  ├── cqrs-es-crypto-derive  (optional, behind "derive" feature)
  └── ... (existing deps)
```

### Feature flag in `cqrs-es-crypto`

```toml
# crates/cqrs-es-crypto/Cargo.toml
[features]
derive = ["dep:cqrs-es-crypto-derive"]

[dependencies]
cqrs-es-crypto-derive = { path = "../cqrs-es-crypto-derive", optional = true }
```

```rust
// crates/cqrs-es-crypto/src/lib.rs
#[cfg(feature = "derive")]
pub use cqrs_es_crypto_derive::PiiCodec;
```

Consumers opt in with:

```toml
cqrs-es-crypto = { path = "...", features = ["derive"] }
```

---

## Implementation with `zyn`

### Why `zyn`

[`zyn`](https://docs.rs/zyn/latest/zyn/) is a proc-macro framework that
provides:

- **Template syntax** (`zyn::zyn!`) — write output tokens as if they were
  source code, with `{{ }}` interpolation and `@for` / `@if` / `@match`
  control flow. This replaces the `quote!` + manual `TokenStream`
  concatenation pattern and makes the generated code visually obvious.
- **Elements** (`#[zyn::element]`) — reusable template components with typed
  props. Each trait method arm can be its own element, composed into the final
  `#[zyn::derive]` output.
- **Typed attribute parsing** (`#[derive(Attribute)]`) — parses `#[pii(...)]`
  annotations into Rust structs with compile-time validation and auto-suggest
  on typos.
- **Built-in test harness** — `assert_tokens!`, `assert_diagnostic_error!`,
  and friends for testing generated output and error messages without
  `trybuild`.
- **Debug mode** — `ZYN_DEBUG="*" cargo build` prints the generated code
  inline, invaluable during development.

### Attribute parsing

Use `#[derive(zyn::Attribute)]` to parse both variant-level and field-level
`#[pii(...)]` attributes into typed structs:

```rust
/// Variant-level: `#[pii(event_type = "...", sentinel = "...")]`
#[derive(zyn::Attribute)]
#[zyn("pii")]
struct PiiVariantAttr {
    event_type: String,
    #[zyn(default = "encrypted_pii".to_string())]
    sentinel: String,
}

/// Field-level: `#[pii(subject)]`, `#[pii(plaintext)]`,
///              `#[pii(secret)]`, `#[pii(secret, redact = "...")]`
#[derive(zyn::Attribute)]
#[zyn("pii")]
struct PiiFieldAttr {
    #[zyn(default)]
    subject: bool,
    #[zyn(default)]
    plaintext: bool,
    #[zyn(default)]
    secret: bool,
    #[zyn(default)]
    redact: Option<String>,
    #[zyn(default)]
    redact_with: Option<String>,
}
```

`zyn`'s `Attribute` derive generates `from_args` / `FromInput`
implementations and provides auto-suggest on typos (e.g. `#[pii(secrete)]` →
"did you mean `secret`?").

### Derive entry point

The main macro is a `#[zyn::derive]` function. It receives the parsed
`DeriveInput`, extracts variant and field metadata, validates constraints, and
composes the output from elements:

```rust
#[zyn::derive]
fn pii_codec(
    #[zyn(input)] ident: zyn::Extract<zyn::syn::Ident>,
    #[zyn(input)] variants: zyn::Variants<zyn::syn::FieldsNamed>,
) -> zyn::TokenStream {
    let codec_ident = zyn::format_ident!("{}PiiCodec", ident);

    // Parse and validate all #[pii] variants into our IR
    let pii_variants = parse_pii_variants(&variants);

    zyn::zyn! {
        pub struct {{ codec_ident }};

        impl ::cqrs_es_crypto::PiiEventCodec for {{ codec_ident }} {
            fn classify(
                &self,
                event: &::cqrs_es::persist::SerializedEvent,
            ) -> ::core::option::Option<::cqrs_es_crypto::PiiFields> {
                match event.event_type.as_str() {
                    @for (v in &pii_variants) {
                        @classify_arm(variant = v.clone())
                    }
                    _ => None,
                }
            }

            fn extract_encrypted(
                &self,
                event: &::cqrs_es::persist::SerializedEvent,
            ) -> ::core::option::Option<::cqrs_es_crypto::EncryptedPiiExtract> {
                match event.event_type.as_str() {
                    @for (v in &pii_variants) {
                        @extract_arm(variant = v.clone())
                    }
                    _ => None,
                }
            }

            // ... reconstruct and redact follow the same pattern
        }
    }
}
```

### Elements

Each match arm is a reusable `#[zyn::element]` that receives a `PiiVariant`
(our intermediate representation struct) and emits the tokens for one arm of
the match:

```rust
#[zyn::element]
fn classify_arm(variant: PiiVariant) -> zyn::TokenStream {
    let event_type = &variant.event_type;
    let key = &variant.key;
    let sentinel = &variant.sentinel;
    let subject_field = &variant.subject_field;
    let plaintext_fields = &variant.plaintext_fields;
    let secret_fields = &variant.secret_fields;

    zyn::zyn! {
        {{ event_type }} => {
            let key = {{ key }};
            let subject_id_str = event.payload[key][{{ subject_field }}]
                .as_str()?.to_string();
            let subject_id = ::uuid::Uuid::parse_str(&subject_id_str).ok()?;

            @for (f in plaintext_fields) {
                let {{ f.binding_ident }} = event.payload[key][{{ f.name }}]
                    .as_str().unwrap_or("").to_string();
            }

            @if (secret_fields.len() > 1) {
                // Multiple secrets → bundle into JSON object
                let inner = event.payload[key].as_object()?;
                let plaintext_pii = ::serde_json::json!({
                    @for (f in secret_fields) {
                        {{ f.name }}: inner.get({{ f.name }})
                            .cloned()
                            .unwrap_or(::serde_json::Value::Null),
                    }
                });
            } @else {
                // Single secret → use value directly
                let plaintext_pii = event.payload[key][{{ secret_fields[0].name }}].clone();
            }

            Some(::cqrs_es_crypto::PiiFields {
                subject_id,
                plaintext_pii,
                build_encrypted_payload: Box::new(move |sentinel| {
                    ::serde_json::json!({
                        {{ key }}: {
                            @for (f in plaintext_fields) {
                                {{ f.name }}: {{ f.binding_ident }},
                            }
                            "subject_id": subject_id_str,
                            {{ sentinel }}: sentinel.ciphertext_b64,
                            "nonce": sentinel.nonce_b64,
                        }
                    })
                }),
            })
        }
    }
}
```

The `extract_arm`, `reconstruct_arm`, and `redact_arm` elements follow the
same structure, each emitting one match arm for one PII variant.

### Testing

`zyn` provides first-class test support. Every element can be tested in
isolation:

```rust
#[test]
fn classify_arm_generates_correct_tokens() {
    let input: zyn::Input = zyn::parse!(
        "enum E {
            #[pii(event_type = \"TestPii\")]
            TestPii {
                #[pii(plaintext)] tag: String,
                #[pii(subject)] subject_id: Uuid,
                #[pii(secret)] secret: String,
            }
        }" => zyn::syn::DeriveInput
    ).unwrap().into();

    let variant = parse_pii_variants_from_input(&input)[0].clone();
    let output = zyn::zyn!(@classify_arm(variant = variant));

    zyn::assert_tokens_contain!(output, "encrypted_pii");
    zyn::assert_tokens_contain!(output, "subject_id");
}

#[test]
fn rejects_variant_without_subject_field() {
    let input: zyn::Input = zyn::parse!(
        "enum E {
            #[pii(event_type = \"Bad\")]
            Bad { #[pii(secret)] s: String }
        }" => zyn::syn::DeriveInput
    ).unwrap().into();

    let output = pii_codec_derive(&input);
    zyn::assert_diagnostic_error!(output, "must have exactly one `#[pii(subject)]` field");
}
```

Integration-level tests can use `trybuild` or compile-test crates to verify
that the full derive produces compilable, correct code when applied to a real
enum — and that it rejects invalid annotations with good error messages.

---

## Redaction Strategy

The macro needs to know what value to emit for each `#[pii(secret)]` field
when the DEK has been deleted. The priority order is:

1. **`redact = "literal"`** — explicit JSON string literal. Emitted as
   `serde_json::json!("literal")`.
2. **`redact_with = "expr"`** — arbitrary expression producing
   `serde_json::Value`. Emitted verbatim.
3. **Type-based default** — inferred at macro expansion time from the field's
   `syn::Type`:

| Type | Detected by | Redacted value |
|------|-------------|---------------|
| `String` | Path ends with `String` | `serde_json::json!("[redacted]")` |
| `Option<T>` | Path starts with `Option` | `serde_json::Value::Null` |
| `Value` / `serde_json::Value` | Path ends with `Value` | `serde_json::json!({})` |
| Anything else | — | **Compile error** with message: "cannot infer redaction for type `T` — add `redact = \"...\"` or `redact_with = \"...\"`" |

Type detection uses `zyn`'s `ext::TypeExt` (behind the `ext` feature) which
provides helpers like `is_option()` and `inner_type()`.

---

## Sentinel Naming

The sentinel is the JSON field name that replaces the secret fields in the
encrypted payload (e.g. `"encrypted_pii"` or `"encrypted_data"`).

| Scenario | Sentinel name |
|----------|--------------|
| Default (no `sentinel` attribute) | `"encrypted_pii"` |
| Explicit `sentinel = "encrypted_data"` | `"encrypted_data"` |

The sentinel name is used in three places in the generated code:

1. **`classify`** — the `build_encrypted_payload` closure writes it.
2. **`extract_encrypted`** — the sentinel-presence check and base64 decode
   read it.
3. Neither `reconstruct` nor `redact` use it — they rebuild from the
   original field names.

---

## Edge Cases and Validation

The macro must reject invalid annotations at compile time with helpful
diagnostics. `zyn`'s `bail!` / `error!` macros and auto-suggest make this
straightforward.

| Condition | Error message |
|-----------|--------------|
| `#[pii(...)]` on a unit variant (no fields) | "`#[pii]` variants must have named fields" |
| `#[pii(...)]` on a tuple variant | "`#[pii]` variants must have named fields (not tuple fields)" |
| PII variant with no `#[pii(subject)]` field | "PII variant `{name}` must have exactly one `#[pii(subject)]` field" |
| PII variant with multiple `#[pii(subject)]` fields | "PII variant `{name}` has multiple `#[pii(subject)]` fields — exactly one is required" |
| PII variant with no `#[pii(secret)]` fields | "PII variant `{name}` must have at least one `#[pii(secret)]` field" |
| PII variant field with no `#[pii]` attribute | "field `{name}` on PII variant `{variant}` must be annotated with `#[pii(subject)]`, `#[pii(plaintext)]`, or `#[pii(secret)]`" |
| Field with multiple roles (e.g. `#[pii(subject, secret)]`) | "field `{name}` cannot be both `subject` and `secret`" |
| Missing `event_type` on `#[pii]` variant | "`event_type` is required on `#[pii(...)]`" |
| Cannot infer redaction for secret field type | "cannot infer redaction for type `{ty}` — add `redact = \"...\"` or `redact_with = \"...\"`" |
| `#[derive(PiiCodec)]` on a non-enum | "`#[derive(PiiCodec)]` can only be applied to enums" |
| Enum has no `#[pii]` variants | "enum `{name}` has no `#[pii]` variants — the generated codec would pass through all events unchanged" (warning, not error) |

---

## Step-by-Step Execution Plan

### Phase 1 — Scaffold the derive crate

1. Create `crates/cqrs-es-crypto-derive/` with `Cargo.toml` depending on
   `zyn`.
2. Define the intermediate representation (`PiiVariant`, `PiiField`).
3. Implement attribute parsing with `#[derive(zyn::Attribute)]`.
4. Write the `#[zyn::derive] fn pii_codec(...)` entry point that parses and
   validates but emits an empty struct + stub impl.
5. Add the `derive` feature flag to `cqrs-es-crypto` and re-export.
6. `cargo check -p cqrs-es-crypto-derive` — compiles.

### Phase 2 — Generate `classify` and `extract_encrypted`

7. Implement `@classify_arm` element.
8. Implement `@extract_arm` element.
9. Write element-level tests using `zyn::assert_tokens_contain!`.
10. Write an integration test: annotate a test enum, derive the codec, wire it
    into `CryptoShreddingEventRepository` with `InMemoryEventRepository`, and
    verify encrypt → persist → read round-trip.

### Phase 3 — Generate `reconstruct` and `redact`

11. Implement `@reconstruct_arm` element.
12. Implement `@redact_arm` element.
13. Element-level tests.
14. Integration test: full encrypt → read → decrypt + shred → redact cycle.

### Phase 4 — Validation and diagnostics

15. Add all compile-time validation checks from the table above.
16. Test each diagnostic with `zyn::assert_diagnostic_error!`.
17. Add `trybuild` tests for end-to-end compile-fail cases.

### Phase 5 — Wire into `journey_dynamics`

18. Replace the hand-written `JourneyPiiCodec` with `#[derive(PiiCodec)]` on
    `JourneyEvent`.
19. Delete `pii_codec.rs` (or reduce it to `pub use` the generated codec).
20. Verify all existing `pii_codec::tests` still pass.
21. `cargo test --workspace` — all green.
22. Run hurl end-to-end tests — no behavioural drift.

### Phase 6 — Clean up

23. Add documentation and examples to `cqrs-es-crypto-derive`.
24. Update `crates/cqrs-es-crypto/README.md` with derive usage.
25. Update `docs/CRYPTO_CRATE_EXTRACTION.md` to mark this future-work item as
    implemented.

---

## Risk Assessment

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Generated JSON doesn't match hand-written format | **Medium** — the macro constructs `serde_json::json!` calls from field metadata; any mismatch in key names or nesting would break wire compatibility | The existing integration tests in `pii_codec::tests` are the acceptance suite — they must pass unchanged after Phase 5 |
| `zyn` API instability | **Low** — `zyn` 0.5.x is published and actively maintained | Pin to `0.5` in `Cargo.toml`; the derive crate is a leaf dependency so upgrades are isolated |
| Proc-macro compile times | **Low** — `zyn` compiles quickly and our macro is small | The derive crate only compiles when the `derive` feature is enabled |
| Edge cases in type detection for redaction defaults | **Medium** — `syn::Type` can be complex (qualified paths, type aliases, generics) | Conservative matching: only detect `String`, `Option<_>`, and `Value` by simple path suffix; require explicit `redact` for everything else |
| Single-secret vs multi-secret behavioural split | **Low** — the rule is clear (1 secret = direct value, N secrets = JSON object) but it's implicit | Document the rule prominently; consider adding an explicit `#[pii(bundle)]` attribute in the future if needed |

---

## Future Work

- **`#[pii(bundle)]` / `#[pii(direct)]`** — explicit control over whether
  multiple secret fields are bundled into a JSON object or treated
  individually, instead of relying on the count-based heuristic.
- **Custom codec name** — `#[pii_codec(name = "MyCodec")]` enum-level
  attribute to override the default `{Enum}PiiCodec` naming.
- **Attribute-level `event_type` inference** — derive `event_type` from the
  variant name automatically when it matches the `DomainEvent::event_type()`
  convention, falling back to the explicit attribute only when they diverge.
- **IDE support** — `zyn`'s debug mode (`ZYN_DEBUG="*" cargo build`) already
  shows generated code inline; ensure the derive crate opts into this for
  development ergonomics.