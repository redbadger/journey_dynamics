# cqrs-es-crypto-derive

Proc-macro companion to [`cqrs-es-crypto`](https://crates.io/crates/cqrs-es-crypto).

Provides `#[derive(PiiCodec)]`, which generates a `{Name}PiiCodec` struct and a
`PiiEventCodec` implementation from an annotated event enum.

## Usage

Enable the `derive` feature on `cqrs-es-crypto` — you do not need to depend on this
crate directly:

```toml
[dependencies]
cqrs-es-crypto = { version = "0.1", features = ["derive"] }
```

Then annotate your event enum:

```rust
use cqrs_es_crypto::PiiCodec;

#[derive(PiiCodec)]
enum MyEvent {
    #[pii(event_type = "SensitiveEvent")]
    SensitiveEvent {
        #[pii(subject)]   subject_id: uuid::Uuid,
        #[pii(plaintext)] tag: String,
        #[pii(secret)]    secret: String,
    },
    PlainEvent { data: String },
}
// Generates: pub struct MyEventPiiCodec;
// + impl PiiEventCodec for MyEventPiiCodec { ... }
```

See the [`cqrs-es-crypto` documentation](https://docs.rs/cqrs-es-crypto) for full
usage instructions, field-role reference, and wiring examples.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT License](../../LICENSE-MIT) at your option.
