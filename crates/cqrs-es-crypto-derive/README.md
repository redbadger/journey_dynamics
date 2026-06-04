# cqrs-es-crypto-derive

Proc-macro companion to [`cqrs-es-crypto`](https://crates.io/crates/cqrs-es-crypto).

Provides `#[derive(PiiCodec)]`, which generates a `{Name}PiiCodec` struct and a
`PiiEventCodec` implementation from an annotated event enum.

The generated codec uses the **multi-subject partitioned** model introduced in
`cqrs-es-crypto` 0.3.0: it emits `extract_partitions`, `reconstruct`,
`redact_partitions`, and `extract_encrypted_legacy`. For a derived variant the
codec produces a single partition with `label = "default"`, and transparently
reads events written by the pre-0.3.0 single-ciphertext format.

## Usage

Enable the `derive` feature on `cqrs-es-crypto` — you do not need to depend on this
crate directly:

```toml
[dependencies]
cqrs-es-crypto = { version = "0.3", features = ["derive"] }
```

Then annotate your event enum. Unannotated variants pass through unchanged:

```rust
use cqrs_es_crypto::PiiCodec;

#[derive(PiiCodec)]
enum MyEvent {
    // Non-PII variant — no annotation needed.
    PlainEvent {
        data: String,
    },

    // `event_type` must match `DomainEvent::event_type()` for this variant.
    #[pii(event_type = "SensitiveEvent")]
    SensitiveEvent {
        #[pii(subject)]
        subject_id: uuid::Uuid,
        #[pii(plaintext)]
        tag: String,
        #[pii(secret)]
        secret: String,
    },
}
// Generates: pub struct MyEventPiiCodec;
// + impl PiiEventCodec for MyEventPiiCodec { ... }
```

### Field roles

| Attribute | Role |
|-----------|------|
| `#[pii(subject)]` | The data-subject UUID. Exactly one per PII variant. Kept in the clear so the read path can locate the DEK. The field name is up to you (`subject_id`, `user_id`, `customer_ref`, …); whatever identifier you write becomes the JSON key in the stored payload. |
| `#[pii(plaintext)]` | A non-PII field kept in the clear on write and read, including after shredding. |
| `#[pii(secret)]` | A PII field, encrypted on write and decrypted or redacted on read. Every field on a PII variant must be annotated; an unannotated field is a compile error. |

The encrypted-blob field defaults to `"encrypted_pii"`; override it per variant
with `#[pii(event_type = "…", sentinel = "encrypted_data")]`.

### Redaction defaults

When a subject's DEK has been deleted, each `#[pii(secret)]` field is replaced
with a per-type default:

| Field type          | Redacted as    | Overridable?                              |
|---------------------|----------------|-------------------------------------------|
| `String`            | `"[redacted]"` | no                                        |
| `Option<_>`         | `null`         | no                                        |
| `serde_json::Value` | `{}`           | no                                        |
| `Vec<_>`            | `[]`           | no                                        |
| `chrono::NaiveDate` | `"0000-01-01"` | yes (requires the `chrono` feature)       |
| anything else       | compile error  | supply `#[pii(secret, redact = "…")]`     |

```rust,ignore
#[pii(secret, redact = "1900-01-01")] dob: chrono::NaiveDate,
```

## When *not* to use the derive macro

The macro's sweet spot is "this variant has these fixed PII fields." It is not
suited to **schema-driven, path-keyed events** whose PII classification is
determined at runtime (e.g. a `SetAttributes` / `AttributesSet`-style event
whose `changes` map is partitioned by an `AttributeSchema`). For those,
hand-write `PiiEventCodec` so classification can consult the runtime schema and
return one `SecretPartition` per subject. See the
[`cqrs-es-crypto` manual-implementation docs](https://docs.rs/cqrs-es-crypto)
for a worked example, and `JourneyPiiCodec`
(`crates/journey_dynamics/src/pii_codec.rs`) for a complete, tested
multi-subject implementation.

See the [`cqrs-es-crypto` documentation](https://docs.rs/cqrs-es-crypto) for full
usage instructions, the field-role reference, multi-subject partitioning, and
wiring examples.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT License](../../LICENSE-MIT) at your option.
