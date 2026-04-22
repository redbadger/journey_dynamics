# General Data Protection Regulation (GDPR) Crypto-Shredding Implementation

## Overview

This document summarises the implementation of GDPR crypto-shredding support in the journey
dynamics service. The feature allows a data subject's personally identifiable information (PII) to be permanently and irreversibly
erased — across all of their journeys — by deleting a single Data Encryption Key (DEK). No
event records are modified or deleted; the ciphertext remains in the event store but is
rendered permanently unreadable.

---

## What Was Built

The implementation was delivered in four phases.

---

### Phase 1 — Infrastructure

**New database tables**

`subject_encryption_keys` — stores one AES-256 DEK per subject, wrapped (encrypted) with the
service-wide Key Encryption Key (KEK) loaded from the `JOURNEY_KEK` environment variable:

```sql
CREATE TABLE subject_encryption_keys (
    key_id      UUID    NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id  UUID    NOT NULL UNIQUE,
    wrapped_key BYTEA   NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

`journey_subject_mapping` — records which aggregate IDs belong to which subject, so that a
shredding call can locate every affected journey:

```sql
CREATE TABLE journey_subject_mapping (
    aggregate_id  TEXT  NOT NULL PRIMARY KEY,
    subject_id    UUID  NOT NULL,
    created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

**New column on `journey_person`**

```sql
ALTER TABLE journey_person ADD COLUMN subject_id UUID;
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
```

**`PiiCipher`**

Provides Advanced Encryption Standard 256-bit Galois/Counter Mode (AES-256-GCM) authenticated encryption for individual fields and Advanced Encryption Standard 256-bit Key Wrap with Padding (AES-256-KWP) key
wrapping/unwrapping for DEK storage. Each encryption call generates a fresh random nonce;
Authenticated Additional Data (AAD) is bound to the `aggregate_id:sequence` of the event
being encrypted, preventing ciphertext transplantation between events.

**`KeyStore` trait + implementations**

```rust
pub trait KeyStore {
    async fn get_or_create_key(&self, subject_id: Uuid) -> Result<KeyMaterial>;
    async fn get_key(&self, subject_id: Uuid) -> Result<Option<KeyMaterial>>;
    async fn delete_key(&self, subject_id: Uuid) -> Result<()>;
}
```

- `PostgresKeyStore` — production implementation backed by `subject_encryption_keys`
- `InMemoryKeyStore` — used in unit tests; holds keys in a `HashMap` behind a `Mutex`

**`SubjectMapping` trait + implementations**

```rust
pub trait SubjectMapping {
    async fn associate(&self, aggregate_id: &str, subject_id: Uuid) -> Result<()>;
    async fn get_subject(&self, aggregate_id: &str) -> Result<Option<Uuid>>;
    async fn get_journeys(&self, subject_id: Uuid) -> Result<Vec<String>>;
}
```

- `PostgresSubjectMapping` — backed by `journey_subject_mapping`
- `InMemorySubjectMapping` — used in unit tests

---

### Phase 2 — Crypto Repository

**`CryptoShreddingEventRepository<R>`**

A wrapper type that implements `PersistedEventRepository` by delegating to an inner repository
`R` while transparently encrypting on write and decrypting on read.

**Write path (`persist`)**

1. If the event is `PersonCaptured`, extract `name`/`email`/`phone`, encrypt them into a
   single `encrypted_pii` blob, and record `aggregate_id → subject_id` in
   `journey_subject_mapping`.
2. If the event is `Modified` and the aggregate already has a subject association, encrypt the
   `data` field.
3. All other events pass through unchanged.
4. AAD for every encrypted field is `"<aggregate_id>:<sequence_number>"`, binding the
   ciphertext to its exact position in the event stream.

**Read path (`get_events`, `get_last_events`, `stream_events`, `stream_all_events`)**

For each event retrieved from the inner repository:

- `PersonCaptured` with an `encrypted_pii` blob: attempt to unwrap the DEK and decrypt. If the
  DEK is absent (shredded), substitute `"[redacted]"` for name and email; phone becomes `None`.
- `Modified` with an `encrypted_data` blob: decrypt if the DEK is present; return `data: {}`
  if the DEK is gone.
- All other events pass through unchanged.
- Legacy events (written before encryption was enabled) have no encrypted blob and pass through
  as-is.

**`InMemoryEventRepository`**

A fully in-memory `PersistedEventRepository` implementation used in unit tests, enabling the
crypto repository to be tested without a real database.

---

### Phase 3 — Domain Changes

**`CapturePerson` command** — `subject_id: Uuid` field added (caller-supplied):

```rust
JourneyCommand::CapturePerson {
    subject_id: Uuid,
    name: String,
    email: String,
    phone: Option<String>,
}
```

**`PersonCaptured` event** — `subject_id: Uuid` field added:

```rust
JourneyEvent::PersonCaptured {
    subject_id: Uuid,
    name: String,
    email: String,
    phone: Option<String>,
}
```

**`Journey` aggregate** — `subject_id: Option<Uuid>` field added; populated when `PersonCaptured`
is applied so subsequent commands can reference the subject.

**`SubjectForgotten` event** — new event emitted as an audit trail when a subject is shredded:

```rust
JourneyEvent::SubjectForgotten { subject_id: Uuid }
```

**`ForgetSubject` command** — new command that emits `SubjectForgotten`:

```rust
JourneyCommand::ForgetSubject { subject_id: Uuid }
```

**View repository projection changes**

- `PersonCaptured` projection: stores `subject_id` in the `journey_person` row. If `name` is
  `"[redacted]"` (DEK already gone at projection time), the upsert is skipped.
- `SubjectForgotten` projection: deletes the `journey_person` row for the subject and clears
  `journey_view.accumulated_data` to `{}` for all affected journeys.

---

### Phase 4 — Wiring

**`config.rs`**

Wraps `PostgresEventRepository` with `CryptoShreddingEventRepository` to produce the
`CryptoCqrs` type alias used throughout the application:

```rust
pub type CryptoCqrs = CqrsFramework<
    Journey,
    CryptoShreddingEventRepository<PostgresEventRepository>,
>;
```

**`state.rs`**

- Reads `JOURNEY_KEK` from the environment at startup (panics if absent or malformed).
- Constructs `PostgresKeyStore` and `PostgresSubjectMapping`.
- Exposes `key_store` and `subject_mapping` on `ApplicationState` so the shredding endpoint
  can access them.

**`DELETE /subjects/{subject_id}` endpoint**

1. Deletes the subject's DEK from `subject_encryption_keys` (irreversible).
2. Queries `journey_subject_mapping` for all aggregate IDs associated with the subject.
3. Executes `ForgetSubject { subject_id }` on each affected journey aggregate, causing
   `SubjectForgotten` to be appended to each event stream as an audit record.
4. The view projector handles `SubjectForgotten` by clearing the read-side data synchronously.

---

## Key Design Decisions

**Journey-level encryption, not field-level in the DB**
Once a subject is identified within a journey, all subsequent `Modified` events for that
journey are considered personal data. Encrypting at the event-store layer (in the repository
wrapper) keeps this concern out of the domain model entirely.

**AAD prevents ciphertext transplantation**
Every encrypted field includes `"<aggregate_id>:<sequence>"` as authenticated additional data.
A ciphertext copied from one event to another will fail decryption, preventing subtle data
integrity attacks.

**`subject_id` is stable across journeys**
A single person may have many journeys. Because all their DEKs share the same `subject_id`,
one `DELETE /subjects/{subject_id}` call shreds every journey simultaneously without needing
to enumerate them in the API call.

**Hard deletion of key rows**
There is no `deleted_at` soft-delete column. The DEK row is hard-deleted, making the key
material irrecoverable. The `SubjectForgotten` event in the event store provides the audit
trail required by GDPR Article 30 record-keeping obligations.

**Shredding is irreversible by design**
This is not a bug. The entire purpose of crypto-shredding is that recovery must be impossible
once the key is gone.

---

## Test Coverage

89 unit and integration tests passing across the feature.

| Area | Tests | What is covered |
|---|---|---|
| `PiiCipher` | 14 | Encrypt/decrypt round-trip, wrong-key rejection, wrong-AAD rejection, nonce uniqueness across multiple encryptions, DEK wrap/unwrap |
| `KeyStore` | 16 | `InMemoryKeyStore` and `PostgresKeyStore`: get-or-create, get, delete, missing-key behaviour |
| `SubjectMapping` | 13 | `InMemorySubjectMapping` and `PostgresSubjectMapping`: associate, get subject, get journeys, unknown aggregate |
| `CryptoShreddingEventRepository` | 16 | Write-path encryption, read-path decryption, redaction after shredding, legacy plaintext passthrough, cross-journey shredding, `stream_events` |
| Domain aggregate | — | `ForgetSubject` command emits `SubjectForgotten`; `CapturePerson` populates `subject_id` on the aggregate |
| View repository | — | `SubjectForgotten` projection deletes `journey_person` row and clears `accumulated_data` |

---

## Files Added or Modified

| File | Change |
|---|---|
| `src/crypto/cipher.rs` | New — `PiiCipher`, `EncryptedPayload` |
| `src/crypto/key_store.rs` | New — `KeyStore` trait, `PostgresKeyStore`, `InMemoryKeyStore`, `KeyMaterial` |
| `src/crypto/subject_mapping.rs` | New — `SubjectMapping` trait, `PostgresSubjectMapping`, `InMemorySubjectMapping` |
| `src/crypto/repository.rs` | New — `CryptoShreddingEventRepository`, `InMemoryEventRepository` |
| `src/crypto/mod.rs` | New — module re-exports |
| `src/domain/commands.rs` | `CapturePerson` gains `subject_id`; `ForgetSubject` added |
| `src/domain/events.rs` | `PersonCaptured` gains `subject_id`; `SubjectForgotten` added |
| `src/domain/journey.rs` | Aggregate gains `subject_id`; handlers for new command/event |
| `src/view_repository.rs` | `PersonCaptured` stores `subject_id`; `SubjectForgotten` projection added |
| `src/config.rs` | Wraps repo with `CryptoShreddingEventRepository`; `CryptoCqrs` type alias |
| `src/state.rs` | Loads `JOURNEY_KEK`; exposes `key_store`, `subject_mapping` |
| `src/routes/subjects.rs` | New — `DELETE /subjects/{subject_id}` handler |
| `migrations/…_crypto_shredding.up.sql` | New tables and `subject_id` column |
| `docs/CRYPTO_SHREDDING_DESIGN.md` | New — full design reference |
| `docs/PERSON_CAPTURE.md` | Updated — `subject_id` field, encryption note |

---

## Status

**Complete.** All 89 tests pass. The shredding endpoint is live. See
[CRYPTO_SHREDDING_DESIGN.md](./CRYPTO_SHREDDING_DESIGN.md) for the full design rationale and
[PERSON_CAPTURE.md](./PERSON_CAPTURE.md) for the updated developer guide.