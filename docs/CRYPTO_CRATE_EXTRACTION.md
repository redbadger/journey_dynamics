# Extracting `cqrs-es-crypto` — Design Document

| | |
|---|---|
| **Service** | Journey Dynamics |
| **Feature** | Extract crypto-shredding layer into a standalone `cqrs-es-crypto` crate |
| **Status** | Proposed |

---

## Table of Contents

1. [Motivation](#motivation)
2. [Current State](#current-state)
3. [Design Goals](#design-goals)
4. [Crate Boundary](#crate-boundary)
   - [What moves](#what-moves)
   - [What stays](#what-stays)
5. [Generic Event Codec](#generic-event-codec)
6. [New Crate Structure](#new-crate-structure)
   - [Cargo.toml](#cargotoml)
   - [Module layout](#module-layout)
   - [Public API surface](#public-api-surface)
7. [Changes to `journey_dynamics`](#changes-to-journey_dynamics)
   - [New `JourneyPiiCodec`](#new-journeypiicodec)
   - [Dependency and import changes](#dependency-and-import-changes)
   - [Migration ownership](#migration-ownership)
8. [Test Strategy](#test-strategy)
9. [Step-by-Step Execution Plan](#step-by-step-execution-plan)
10. [Risk Assessment](#risk-assessment)
11. [Future Work](#future-work)

---

## Motivation

The crypto-shredding layer (`crates/journey_dynamics/src/crypto/`) is a
self-contained subsystem with clear inputs and outputs. Extracting it into its
own workspace crate yields:

- **Separation of concerns** — cryptographic code has a distinct review cadence,
  audit surface, and set of domain experts compared to business logic.
- **Independent testing** — cipher, key-store, and repository tests can run
  without compiling the full application.
- **Reusability** — a second service (e.g. a GDPR compliance worker) can depend
  on `cqrs-es-crypto` without pulling in Axum, the decision engine, or the
  journey aggregate.
- **Compile-time isolation** — changes to journey business logic no longer
  trigger a rebuild of the crypto layer.

Because there are no external consumers today, we can make the correct
structural choices without worrying about breaking changes.

---

## Current State

The `crypto` module consists of three files:

| File | Responsibility | External dependencies |
|------|---------------|-----------------------|
| `cipher.rs` | AES-256-GCM field encryption, AES-256-KWP key wrapping (`PiiCipher`, `KeyMaterial`, `EncryptedPayload`, `CryptoError`) | `aes-gcm`, `aes-kw`, `uuid`, `zeroize` |
| `key_store.rs` | `KeyStore` trait, `InMemoryKeyStore`, `PostgresKeyStore` | `sqlx`, `async-trait`, `uuid`, `zeroize` + `cipher.rs` |
| `repository.rs` | `CryptoShreddingEventRepository<R>`, `InMemoryEventRepository` | `cqrs-es`, `base64`, `serde_json`, `uuid` + `cipher.rs`, `key_store.rs` |

### Coupling points

`cipher.rs` and `key_store.rs` have **zero** domain coupling — they know nothing
about journeys, events, or payload shapes.

`repository.rs` has **two** domain coupling points:

1. **Hard-coded event-type strings and JSON layout** — it knows that
   `PersonCaptured` contains `name`, `email`, `phone`, `person_ref`,
   `subject_id`; that `PersonDetailsUpdated` contains `data`, `person_ref`,
   `subject_id`; and that other event types are non-PII.
2. **Test-time `Journey` aggregate** — the `#[cfg(test)]` module imports
   `crate::domain::journey::Journey` to satisfy `PersistedEventRepository`'s
   `A: Aggregate` type parameter.

The call sites in the host crate are:

| File | What it uses |
|------|-------------|
| `config.rs` | `PiiCipher`, `KeyStore`, `CryptoShreddingEventRepository` |
| `state.rs` | `PiiCipher`, `KeyStore`, `PostgresKeyStore` |
| `route_handler.rs` | `state.key_store: Arc<dyn KeyStore>` (via `state.rs`) |

---

## Design Goals

1. **`cipher.rs` and `key_store.rs` move verbatim** — these are already clean.
2. **`repository.rs` becomes generic** — the domain-specific
   `PersonCaptured` / `PersonDetailsUpdated` knowledge is replaced by a trait
   (`PiiEventCodec`) that the host crate implements.
3. **The new crate has no dependency on `journey_dynamics`** — the dependency
   arrow points strictly downward.
4. **Zero wire-format change** — the on-disk event format (AAD scheme, JSON
   sentinel fields, base64 encoding, wrapped-key format) is identical before and
   after extraction.
5. **The `subject_encryption_keys` table DDL stays in the application migration
   set** but is documented in the new crate's README.

---

## Crate Boundary

### What moves

| Source | Destination |
|--------|------------|
| `src/crypto/cipher.rs` | `crates/cqrs-es-crypto/src/cipher.rs` |
| `src/crypto/key_store.rs` | `crates/cqrs-es-crypto/src/key_store.rs` |
| `src/crypto/repository.rs` (generalised) | `crates/cqrs-es-crypto/src/repository.rs` |
| — | `crates/cqrs-es-crypto/src/lib.rs` (new) |
| — | `crates/cqrs-es-crypto/Cargo.toml` (new) |

### What stays

| Item | Reason |
|------|--------|
| `src/crypto/` directory | Deleted entirely; replaced by `cqrs-es-crypto` dep |
| `JourneyPiiCodec` impl | Domain knowledge lives in the host crate |
| `subject_encryption_keys` migration | Co-located with all other app migrations |

---

## Generic Event Codec

The key abstraction that replaces the hard-coded event knowledge is a trait that
tells the crypto repository how to detect, extract, encrypt, and reassemble PII
for any event type:

```rust
/// Describes how to locate and transform PII within a serialised event payload.
///
/// Implementors encode the domain-specific knowledge of which event types carry
/// PII, where the subject ID lives, which fields are sensitive, and how to
/// reassemble the payload after encryption or when redacting.
#[async_trait]
pub trait PiiEventCodec: Send + Sync {
    /// Inspect a serialised event and return encryption instructions, or `None`
    /// if this event type carries no PII and should be stored verbatim.
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields>;

    /// Rebuild the event payload from decrypted PII bytes.
    ///
    /// `original` is the encrypted-form payload (containing sentinels like
    /// `encrypted_pii` / `nonce`). `plaintext_pii` is the decrypted JSON blob.
    fn reconstruct(
        &self,
        event: &SerializedEvent,
        plaintext_pii: &Value,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>>;

    /// Rebuild the event payload with redacted placeholders (key deleted).
    fn redact(
        &self,
        event: &SerializedEvent,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>>;
}
```

The supporting type returned by `classify`:

```rust
/// Instructions for encrypting a single event's PII.
pub struct PiiFields {
    /// The data-subject identifier — used to look up / create the DEK.
    pub subject_id: Uuid,

    /// The JSON blob of PII fields to encrypt (will be serialised to bytes
    /// and fed to AES-256-GCM).
    pub plaintext_pii: Value,

    /// A function that takes the original payload and the encryption sentinel
    /// fields (`encrypted_pii` / `nonce` as base64 strings) and returns the
    /// payload to persist.
    ///
    /// The non-PII fields (e.g. `person_ref`, `subject_id`) are preserved by
    /// this function; only the PII fields are replaced with the sentinel.
    pub build_encrypted_payload: Box<dyn FnOnce(EncryptedPiiSentinel) -> Value + Send>,
}

/// The base64-encoded ciphertext and nonce to embed in the persisted payload.
pub struct EncryptedPiiSentinel {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
}
```

`CryptoShreddingEventRepository` becomes:

```rust
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    inner: R,
    key_store: Arc<dyn KeyStore>,
    cipher: Arc<PiiCipher>,
    codec: Arc<dyn PiiEventCodec>,
}
```

The host crate provides a `JourneyPiiCodec` that encodes exactly the same
`PersonCaptured` / `PersonDetailsUpdated` logic that lives in `repository.rs`
today.

---

## New Crate Structure

### Cargo.toml

```toml
[package]
name = "cqrs-es-crypto"
version = "0.1.0"
edition.workspace = true

[dependencies]
aes-gcm     = "0.10"
aes-kw      = "0.3"
async-trait  = "0.1"
base64       = "0.22"
cqrs-es      = "0.5.0"
serde        = { version = "1.0", features = ["derive"] }
serde_json   = "1.0"
sqlx         = { version = "0.8", features = ["postgres", "runtime-tokio-native-tls", "uuid"] }
thiserror    = "2.0"
uuid         = { version = "1", features = ["v4", "serde"] }
zeroize      = "1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

### Module layout

```text
crates/cqrs-es-crypto/
├── Cargo.toml
├── README.md
└── src/
    ├── lib.rs          # Re-exports, crate-level docs
    ├── cipher.rs       # PiiCipher, KeyMaterial, EncryptedPayload, CryptoError
    ├── key_store.rs    # KeyStore trait, InMemoryKeyStore, PostgresKeyStore
    └── repository.rs   # PiiEventCodec trait, PiiFields, EncryptedPiiSentinel,
                        # CryptoShreddingEventRepository<R>,
                        # InMemoryEventRepository (behind #[cfg(test)] or
                        # a `testing` feature flag)
```

### Public API surface

`lib.rs` re-exports the primary types so consumers rarely need to reach into
submodules:

```rust
// Cipher
pub use cipher::{CryptoError, EncryptedPayload, KeyMaterial, PiiCipher};

// Key store
pub use key_store::{InMemoryKeyStore, KeyStore, KeyStoreError, PostgresKeyStore};

// Repository
pub use repository::{
    CryptoShreddingEventRepository,
    EncryptedPiiSentinel,
    PiiEventCodec,
    PiiFields,
};
```

`InMemoryEventRepository` is used only for testing. It should be gated behind a
Cargo feature:

```toml
[features]
testing = []
```

```rust
#[cfg(any(test, feature = "testing"))]
pub use repository::InMemoryEventRepository;
```

---

## Changes to `journey_dynamics`

> **Note:** In Rust source the crate name is `cqrs_es_crypto` (underscores),
> matching Cargo's automatic hyphen-to-underscore mapping.

### New `JourneyPiiCodec`

A new file `crates/journey_dynamics/src/pii_codec.rs` (or similar) implements
`PiiEventCodec` with the exact logic currently in `repository.rs`'s
`encrypt_person_captured`, `encrypt_person_details_updated`,
`decrypt_person_captured`, and `decrypt_person_details_updated`:

```rust
pub struct JourneyPiiCodec;

impl PiiEventCodec for JourneyPiiCodec {
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields> {
        match event.event_type.as_str() {
            "PersonCaptured" => { /* extract subject_id, bundle name/email/phone */ }
            "PersonDetailsUpdated" => { /* extract subject_id, bundle data */ }
            _ => None,
        }
    }

    fn reconstruct(&self, event: &SerializedEvent, plaintext_pii: &Value)
        -> Result<Value, Box<dyn std::error::Error + Send + Sync>>
    {
        match event.event_type.as_str() {
            "PersonCaptured" => { /* rebuild with name, email, phone from plaintext_pii */ }
            "PersonDetailsUpdated" => { /* rebuild with data from plaintext_pii */ }
            _ => Ok(event.payload.clone()),
        }
    }

    fn redact(&self, event: &SerializedEvent)
        -> Result<Value, Box<dyn std::error::Error + Send + Sync>>
    {
        match event.event_type.as_str() {
            "PersonCaptured" => { /* person_ref + subject_id kept; name/email/phone → "[redacted]" / null */ }
            "PersonDetailsUpdated" => { /* person_ref + subject_id kept; data → {} */ }
            _ => Ok(event.payload.clone()),
        }
    }
}
```

### Dependency and import changes

In `crates/journey_dynamics/Cargo.toml`:

```toml
[dependencies]
cqrs-es-crypto = { path = "../cqrs-es-crypto", features = ["testing"] }
# Remove: aes, aes-gcm, aes-kw, zeroize (no longer used directly)
```

Import paths change from `crate::crypto::*` to `cqrs_es_crypto::*`:

| File | Before | After |
|------|--------|-------|
| `config.rs` | `crate::crypto::{cipher::PiiCipher, key_store::KeyStore, repository::CryptoShreddingEventRepository}` | `cqrs_es_crypto::{PiiCipher, KeyStore, CryptoShreddingEventRepository}` |
| `state.rs` | `crate::crypto::{cipher::PiiCipher, key_store::{KeyStore, PostgresKeyStore}}` | `cqrs_es_crypto::{PiiCipher, KeyStore, PostgresKeyStore}` |
| `lib.rs` | `pub mod crypto;` | *(removed)* |

The `CryptoShreddingEventRepository::new` call in `config.rs` gains a codec
argument:

```rust
let codec = Arc::new(JourneyPiiCodec);
let crypto_repo = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec);
```

### Migration ownership

The `subject_encryption_keys` DDL remains in
`migrations/20260423132137_init.up.sql`. The `cqrs-es-crypto` README documents
the required table schema so future consumers know what to provision.

---

## Test Strategy

### `cqrs-es-crypto` crate tests

| Module | Coverage | Notes |
|--------|----------|-------|
| `cipher.rs` | All existing tests move verbatim | No domain dependencies |
| `key_store.rs` | All existing tests move verbatim | `InMemoryKeyStore` tests run unconditionally; `PostgresKeyStore` tests remain `#[ignore]` |
| `repository.rs` | New tests using a **test codec** | Define a trivial `TestPiiCodec` that encrypts/decrypts a synthetic `"TestPii"` event type, plus a `TestAggregate` that satisfies `cqrs_es::Aggregate`. Covers the generic encrypt → persist → read → decrypt → redact flow without any journey knowledge. |

> Run with `cargo test -p cqrs-es-crypto` (and `-- --ignored` for the
> Postgres-backed `key_store` tests).

### `journey_dynamics` crate tests

| What | Coverage |
|------|----------|
| `JourneyPiiCodec` unit tests | `classify`, `reconstruct`, and `redact` for `PersonCaptured`, `PersonDetailsUpdated`, and non-PII events |
| Integration tests (existing) | The existing `repository.rs` integration tests move here, now exercising `CryptoShreddingEventRepository` with `JourneyPiiCodec` + `InMemoryEventRepository` + `InMemoryKeyStore`. They use `Journey` as the aggregate type parameter as before. |
| Hurl end-to-end tests | `full-flight-booking_with_shredding.hurl` and `full-flight-booking_with_shredding_by_email.hurl` confirm no behavioural drift. |

---

## Step-by-Step Execution Plan

### Phase 1 — Scaffold the new crate

1. Create `crates/cqrs-es-crypto/` with `Cargo.toml`, `src/lib.rs`.
2. Copy `cipher.rs` and `key_store.rs` verbatim. Fix `use super::` → `use
   crate::` paths. Run `cargo check -p cqrs-es-crypto`.
3. Copy `cipher.rs` and `key_store.rs` tests. Run `cargo test -p
   cqrs-es-crypto`.

### Phase 2 — Introduce `PiiEventCodec` and generalise the repository

4. Define `PiiEventCodec`, `PiiFields`, and `EncryptedPiiSentinel` in
   `repository.rs`.
5. Rewrite `CryptoShreddingEventRepository` to delegate to `self.codec` instead
   of hard-coding `encrypt_person_captured` / `decrypt_person_captured` etc.
6. Move `InMemoryEventRepository` behind `#[cfg(any(test, feature =
   "testing"))]`.
7. Write `TestPiiCodec` + `TestAggregate` and new generic repository tests.
8. `cargo test -p cqrs-es-crypto` — all green.

### Phase 3 — Wire into `journey_dynamics`

9. Add `cqrs-es-crypto` dependency to `journey_dynamics/Cargo.toml`.
10. Create `pii_codec.rs` with `JourneyPiiCodec` implementing `PiiEventCodec`.
11. Delete `src/crypto/` directory and `pub mod crypto` from `lib.rs`.
12. Update imports in `config.rs`, `state.rs`, `lib.rs`.
13. Pass `JourneyPiiCodec` to `CryptoShreddingEventRepository::new` in
    `config.rs`.
14. Move the integration tests from the old `repository.rs#[cfg(test)]` into
    `journey_dynamics` (either inline in `pii_codec.rs` or as a dedicated test
    module), now using `JourneyPiiCodec`.
15. Remove now-unused direct dependencies (`aes`, `aes-gcm`, `aes-kw`,
    `zeroize`) from `journey_dynamics/Cargo.toml`.
16. `cargo test -p journey_dynamics` — all green.
17. Run hurl end-to-end tests.

### Phase 4 — Clean up

18. Add `crates/cqrs-es-crypto/README.md` documenting the crate purpose,
    required Postgres DDL, and usage example.
19. Update `docs/ARCHITECTURE_REVIEW.md` and other docs to reflect the new crate
    boundary.
20. Final `cargo test --workspace` and full hurl suite.

---

## Risk Assessment

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Wire-format drift (events unreadable after migration) | **Very low** — the base64/AAD/sentinel format is preserved by contract in the codec, not reinvented | `TestPiiCodec` in `journey_crypto` + `JourneyPiiCodec` integration tests in `journey_dynamics` + hurl end-to-end tests |
| `PiiEventCodec` trait is wrong-shaped for future event types | **Low** — the classify/reconstruct/redact triple covers all current and foreseeable patterns | The trait is in our crate, so we can evolve it freely |
| Performance regression from trait-object dispatch on codec | **Negligible** — one virtual call per event, dwarfed by Postgres I/O | — |
| Increased compile time from extra crate | **Negligible** — the crate is small and Cargo parallelises crate compilation | — |

---

## Future Work

- **Feature-gated Postgres support** — put `PostgresKeyStore` and the `sqlx`
  dependency behind a `postgres` feature flag so non-Postgres consumers can use
  the crate without pulling in sqlx.
- **Configurable table name** — make the `subject_encryption_keys` table name a
  parameter on `PostgresKeyStore::new` rather than a hard-coded SQL literal.
- **Co-located migrations** — move the `subject_encryption_keys` DDL into
  `crates/cqrs-es-crypto/migrations/` and expose a helper to run it via
  `sqlx::migrate!`.
- **`#[derive]` macro for `PiiEventCodec`** — if the number of PII event types
  grows, a proc-macro could generate the codec from annotated enum variants.