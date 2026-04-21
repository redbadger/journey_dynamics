# GDPR Crypto-Shredding for PII Data

| | |
|---|---|
| **Service** | Journey Dynamics |
| **Feature** | Crypto-shredding for GDPR right-to-erasure |
| **Date** | April 2026 |
| **Status** | Design — ready for review |

---

## Table of Contents

1. [Motivation](#motivation)
2. [Problem Analysis](#problem-analysis)
3. [Design Overview](#design-overview)
4. [Key Concepts](#key-concepts)
   - [Subject ID](#subject-id)
   - [Data Encryption Key (DEK)](#data-encryption-key-dek)
   - [PII Boundary](#pii-boundary)
5. [Architecture](#architecture)
   - [Crypto-Shredding Event Repository](#crypto-shredding-event-repository)
   - [Key Store](#key-store)
   - [Encryption Scheme](#encryption-scheme)
6. [Domain Model Changes](#domain-model-changes)
   - [Commands](#commands)
   - [Events](#events)
   - [Aggregate](#aggregate)
7. [Read-Side Projection Changes](#read-side-projection-changes)
8. [Database Schema Changes](#database-schema-changes)
9. [Shredding Flow](#shredding-flow)
10. [PII Firewall](#pii-firewall)
11. [Testing Strategy](#testing-strategy)
12. [Migration Plan](#migration-plan)
13. [Future Considerations](#future-considerations)

---

## Motivation

GDPR Article 17 grants data subjects the "right to erasure" — the right to have their personal data deleted. In a conventional database this is straightforward: delete the rows. In an event-sourced system, however, events are immutable and append-only. Deleting or mutating events would break the event stream's integrity, aggregate rehydration, and audit guarantees.

**Crypto-shredding** solves this tension. Instead of deleting the events themselves, we encrypt PII fields within events using a per-subject encryption key. When a data subject exercises their right to erasure, we delete the key. The encrypted PII in the event store becomes permanently unreadable — effectively erased — while the event stream's structure, ordering, and non-PII fields remain intact.

---

## Problem Analysis

### Current state

Today, PII flows through two paths in the system:

| Path | Event | Where PII ends up |
|---|---|---|
| `CapturePerson` command | `PersonCaptured { name, email, phone }` | `events.payload` (plaintext JSON), `journey_person` table |
| `Capture` command | `Modified { step, data }` | `events.payload` (plaintext JSON), `journey_view.accumulated_data` |

Both paths persist PII as plaintext in the event store. The first path is well-contained — `PersonCaptured` events carry clearly identified PII fields. The second path is more dangerous: arbitrary JSON in `Capture` commands could contain PII (e.g., an email field in form data), and this ends up merged into `accumulated_data` on both the aggregate and the read model.

### What needs to change

1. PII in events must be encrypted before it reaches the event store
2. PII must be decryptable on the read path (for projections and aggregate rehydration) — until the subject is forgotten
3. A per-subject encryption key must be managed in a dedicated key store
4. Deleting the key must be sufficient to render all PII for that subject permanently unreadable
5. PII must not leak through the `Capture` path into `accumulated_data`
6. Read-side projections containing PII must be cleared or rebuilt on shredding

---

## Design Overview

```
                        Domain Layer
                     (plaintext events)
                            │
                ┌───────────┴───────────┐
                │    EventSink          │
                │    collects events    │
                │    serde to Value     │
                └───────────┬───────────┘
                            │
                    SerializedEvent
               { event_type, payload: Value }
                            │
        ┌───────────────────┴───────────────────┐
        │   CryptoShreddingEventRepository<R>   │
        │                                       │
        │   persist():                          │
        │     match event_type:                 │
        │       "PersonCaptured" →              │
        │         extract subject_id            │
        │         get/create DEK from KeyStore  │
        │         encrypt PII fields in payload │
        │       _ → pass through                │
        │     delegate to inner.persist()       │
        │                                       │
        │   get_events():                       │
        │     delegate to inner.get_events()    │
        │     match event_type:                 │
        │       "PersonCaptured" →              │
        │         look up DEK from KeyStore     │
        │         if found → decrypt PII fields │
        │         if missing → substitute       │
        │           redacted sentinel           │
        │       _ → pass through                │
        └───────────────────┬───────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        │       PostgresEventRepository         │
        │       (unchanged — stores cipher-     │
        │        text in payload column)         │
        └───────────────────────────────────────┘
```

The encryption/decryption layer wraps the `PersistedEventRepository` trait from `cqrs-es`. The domain layer — aggregate, command handlers, event definitions — continues to work with plaintext. The crypto boundary sits between serialization and persistence, which is the natural seam.

---

## Key Concepts

### Subject ID

A `subject_id: Uuid` identifies the data subject (the person whose PII is being captured). It is:

- Supplied by the caller when issuing a `CapturePerson` command
- Stored in the `PersonCaptured` event alongside the PII fields
- Used as the lookup key in the key store
- **Not itself PII** — it is an opaque identifier with no intrinsic meaning
- Stable across journeys — the same person in multiple journeys uses the same `subject_id`, so a single key deletion shreds PII across all their journeys

The `subject_id` is stored in plaintext in the event payload (it is not encrypted). This allows the crypto layer to find the right key on the read path without needing to decrypt anything first.

### Data Encryption Key (DEK)

Each subject gets a unique symmetric encryption key (the DEK). The DEK is:

- Generated on first use (when the first `PersonCaptured` event is persisted for a given `subject_id`)
- Stored in a dedicated `subject_encryption_keys` table
- Used to encrypt/decrypt all PII fields for that subject across all events and all journeys
- Itself encrypted at rest with a Key Encryption Key (KEK) — see [Encryption Scheme](#encryption-scheme)

### PII Boundary

We define a clear, strict boundary around which events and fields contain PII:

| Event type | PII fields | Non-PII fields |
|---|---|---|
| `PersonCaptured` | `name`, `email`, `phone` | `subject_id` |
| `Started` | — | `id` |
| `Modified` | **none** (enforced — see [PII Firewall](#pii-firewall)) | `step`, `data` |
| `WorkflowEvaluated` | — | `suggested_actions` |
| `StepProgressed` | — | `from_step`, `to_step` |
| `Completed` | — | — |
| `SubjectForgotten` | — | `subject_id` |

Only `PersonCaptured` events contain PII. This is enforced at the API and validation layers.

---

## Architecture

### Crypto-Shredding Event Repository

A new struct that wraps any `PersistedEventRepository` and adds encryption/decryption:

```rust
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    inner: R,
    key_store: Arc<dyn KeyStore>,
    cipher: PiiCipher,
}
```

It implements `PersistedEventRepository` by delegating to `inner`, intercepting `SerializedEvent` values on both the write and read paths.

#### Write path (`persist`)

```
for each SerializedEvent in events:
    if event.event_type == "PersonCaptured":
        subject_id = event.payload["subject_id"]     // plaintext, always present
        dek = key_store.get_or_create_key(subject_id)
        pii = extract { name, email, phone } from event.payload
        encrypted = cipher.encrypt(dek, serialize(pii))
        replace payload with:
            { "subject_id": <uuid>, "encrypted_pii": <base64>, "nonce": <base64> }
    delegate to inner.persist(events)
```

#### Read path (`get_events`, `get_last_events`, `stream_events`, `stream_all_events`)

```
events = inner.get_events(aggregate_id)
for each SerializedEvent in events:
    if event.event_type == "PersonCaptured":
        subject_id = event.payload["subject_id"]
        dek = key_store.get_key(subject_id)
        if dek is Some:
            pii = cipher.decrypt(dek, event.payload["encrypted_pii"], event.payload["nonce"])
            restore payload to:
                { "subject_id": <uuid>, "name": ..., "email": ..., "phone": ... }
        else:
            // Key has been deleted — subject was forgotten
            restore payload to:
                { "subject_id": <uuid>, "name": "[redacted]", "email": "[redacted]", "phone": null }
return events
```

The redacted sentinel allows the aggregate and projections to process the event without crashing. The domain types remain `String`, so `"[redacted]"` is a valid value. Projections can check for this sentinel and handle accordingly.

#### Snapshot path (`get_snapshot`)

If snapshots are used in the future, the aggregate's serialized state could contain PII (if person data is ever added to the aggregate). For now, `PersonCaptured` produces no aggregate state change, so snapshots are clean. This should be revisited if person data is added to the aggregate struct.

### Key Store

A trait and Postgres implementation for managing per-subject encryption keys:

```rust
#[async_trait]
pub trait KeyStore: Send + Sync {
    /// Get or create a DEK for the given subject.
    /// If no key exists, generate one, persist it, and return it.
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError>;

    /// Get the DEK for the given subject, if it exists.
    /// Returns None if the key has been deleted (subject was forgotten).
    async fn get_key(&self, subject_id: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError>;

    /// Delete the DEK for the given subject. This is the shredding operation.
    /// After this call, all PII encrypted with this key is permanently unreadable.
    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError>;
}
```

`KeyMaterial` wraps the raw key bytes and the key ID for traceability:

```rust
pub struct KeyMaterial {
    pub key_id: Uuid,
    pub key: Zeroizing<Vec<u8>>,  // 256-bit AES key, zeroized on drop
}
```

### Encryption Scheme

| Layer | Algorithm | Purpose |
|---|---|---|
| **Field encryption (DEK)** | AES-256-GCM | Encrypts PII fields within events. Authenticated encryption ensures integrity. |
| **Key encryption (KEK)** | AES-256-KWP or KMS envelope encryption | Encrypts DEKs at rest in the key store. The KEK is either a static secret from config/environment or, preferably, a key in an external KMS (AWS KMS, GCP KMS, HashiCorp Vault). |

AES-256-GCM requires a unique nonce per encryption operation. The nonce is stored alongside the ciphertext in the event payload. The authenticated data (AAD) should include the `aggregate_id` and `sequence` number to bind the ciphertext to its position in the stream.

```rust
pub struct PiiCipher {
    // If using a local KEK for DEK wrapping:
    kek: Zeroizing<Vec<u8>>,
}

impl PiiCipher {
    /// Encrypt a PII payload with the given DEK.
    pub fn encrypt(&self, dek: &KeyMaterial, plaintext: &[u8], aad: &[u8]) -> EncryptedPayload;

    /// Decrypt a PII payload with the given DEK.
    pub fn decrypt(&self, dek: &KeyMaterial, encrypted: &EncryptedPayload, aad: &[u8])
        -> Result<Vec<u8>, CryptoError>;

    /// Unwrap a DEK that was encrypted with the KEK (loaded from key store).
    pub fn unwrap_dek(&self, wrapped_key: &[u8]) -> Result<KeyMaterial, CryptoError>;

    /// Wrap a DEK with the KEK before storing in the key store.
    pub fn wrap_dek(&self, dek: &KeyMaterial) -> Vec<u8>;
}

pub struct EncryptedPayload {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
}
```

---

## Domain Model Changes

### Commands

Add `subject_id` to `CapturePerson` and introduce a new `ForgetSubject` command:

```rust
#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    Start {
        id: Uuid,
    },
    Capture {
        step: String,
        data: Value,
    },
    CapturePerson {
        subject_id: Uuid,    // NEW — identifies the data subject
        name: String,
        email: String,
        phone: Option<String>,
    },
    Complete,
}
```

The `ForgetSubject` command is handled outside the journey aggregate (it does not belong to any single journey — it operates across all journeys for a subject). It is exposed as a dedicated API endpoint that:

1. Calls `key_store.delete_key(subject_id)`
2. Clears/anonymizes the `journey_person` read-side projection for that subject
3. Optionally emits a `SubjectForgotten` audit event (see below)

### Events

Add `subject_id` to `PersonCaptured` and introduce `SubjectForgotten`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JourneyEvent {
    Started { id: Uuid },
    Modified { step: String, data: Value },
    PersonCaptured {
        subject_id: Uuid,    // NEW — stored in plaintext (not PII)
        name: String,        // encrypted at rest in event store
        email: String,       // encrypted at rest in event store
        phone: Option<String>, // encrypted at rest in event store
    },
    WorkflowEvaluated { suggested_actions: Vec<String> },
    StepProgressed { from_step: Option<String>, to_step: String },
    Completed,
    SubjectForgotten {       // NEW — audit trail (no PII)
        subject_id: Uuid,
    },
}
```

Note: in the event store, the `PersonCaptured` payload will actually look like this (after the crypto layer encrypts it):

```json
{
    "PersonCaptured": {
        "subject_id": "550e8400-e29b-41d4-a716-446655440000",
        "encrypted_pii": "dGhpcyBpcyBiYXNlNjQgZW5jcnlwdGVk...",
        "nonce": "dW5pcXVlIG5vbmNl..."
    }
}
```

But the domain layer never sees this form — it always works with the decrypted `PersonCaptured` variant.

### Aggregate

The `apply` method for `PersonCaptured` currently does nothing:

```rust
JourneyEvent::PersonCaptured { .. } => {
    // Person data is projected to read model tables
    // No state change needed in the aggregate
}
```

With the introduction of `subject_id`, we should store it on the aggregate so that business rules can reference it if needed in the future:

```rust
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    accumulated_data: Value,
    current_step: Option<String>,
    latest_workflow_decision: Option<WorkflowDecisionState>,
    subject_id: Option<Uuid>,  // NEW
}
```

```rust
JourneyEvent::PersonCaptured { subject_id, .. } => {
    self.subject_id = Some(subject_id);
}
```

The `subject_id` is not PII and does not need encryption in snapshots.

---

## Read-Side Projection Changes

### `journey_person` table

Add `subject_id` column:

```sql
ALTER TABLE journey_person ADD COLUMN subject_id UUID;
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
```

The projection in `StructuredJourneyViewRepository::update_view` continues to receive decrypted events (the crypto layer decrypts before events reach the query dispatchers). When the key has been deleted, the projection receives redacted sentinels and should handle them:

```rust
JourneyEvent::PersonCaptured { subject_id, name, email, phone } => {
    if name == "[redacted]" {
        // Subject has been forgotten — skip projection update
        // or delete existing row
        return Ok(());
    }
    // ... normal upsert logic, now including subject_id ...
}
```

### Shredding the read model

When `ForgetSubject` is invoked, in addition to deleting the key, we must clear PII from the read model:

```sql
DELETE FROM journey_person WHERE subject_id = $1;
```

Alternatively, anonymize rather than delete (to preserve the structural relationship):

```sql
UPDATE journey_person
SET name = '[redacted]', email = '[redacted]', phone = NULL, updated_at = CURRENT_TIMESTAMP
WHERE subject_id = $1;
```

The choice depends on whether downstream consumers need to know a person record once existed for a given journey.

---

## Database Schema Changes

### New table: `subject_encryption_keys`

```sql
CREATE TABLE subject_encryption_keys (
    key_id          UUID        NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id      UUID        NOT NULL UNIQUE,
    wrapped_key     BYTEA       NOT NULL,   -- DEK encrypted with the KEK
    created_at      TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_subject_keys_subject_id ON subject_encryption_keys(subject_id);
```

Note: there is no `deleted_at` column. When a key is deleted, the row is **hard-deleted**. This ensures the key material is irrecoverable and satisfies the erasure requirement. An audit trail is maintained via the `SubjectForgotten` event in the event store.

### Altered table: `journey_person`

```sql
ALTER TABLE journey_person ADD COLUMN subject_id UUID;
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
```

### No changes to: `events`

The `events` table schema is unchanged. The `payload` column continues to store JSON — it just happens to contain ciphertext for PII events instead of plaintext. The crypto layer is transparent to the storage layer.

---

## Shredding Flow

### Sequence diagram

```
    Client                API              KeyStore           Read Model
      │                    │                  │                   │
      │  DELETE /subjects  │                  │                   │
      │  /{subject_id}     │                  │                   │
      │───────────────────>│                  │                   │
      │                    │                  │                   │
      │                    │  delete_key()    │                   │
      │                    │─────────────────>│                   │
      │                    │                  │                   │
      │                    │       Ok(())     │                   │
      │                    │<─────────────────│                   │
      │                    │                  │                   │
      │                    │  DELETE FROM journey_person          │
      │                    │  WHERE subject_id = ...              │
      │                    │────────────────────────────────────> │
      │                    │                  │                   │
      │                    │  (emit SubjectForgotten event        │
      │                    │   for audit trail)                   │
      │                    │                  │                   │
      │    204 No Content  │                  │                   │
      │<───────────────────│                  │                   │
```

### What happens after shredding

1. **Event store**: `PersonCaptured` events for this subject still exist, but their PII fields are encrypted ciphertext. The key is gone. The data is permanently unreadable.

2. **Aggregate rehydration**: When a journey containing a shredded `PersonCaptured` event is loaded, the crypto layer detects the missing key and substitutes `"[redacted]"` sentinels. The aggregate loads successfully; `subject_id` is set but the PII is gone.

3. **Read model**: The `journey_person` rows for this subject have been deleted (or anonymized). Queries by email for this subject return nothing.

4. **Audit trail**: A `SubjectForgotten { subject_id }` event records when the erasure occurred, without containing any PII.

---

## PII Firewall

A critical invariant: **PII must only enter the system through the `CapturePerson` command.** If PII leaks through the `Capture` command into `Modified` events and `accumulated_data`, crypto-shredding is bypassed entirely.

### Enforcement strategy

1. **Schema validation**: The `SchemaValidator` service already validates `Capture` payloads. We extend the schema to **reject known PII field names** (e.g., `name`, `email`, `phone`, `date_of_birth`, etc.) in `Capture` payloads. This is a blocklist approach.

2. **API-level guidance**: Documentation and API design make it clear that `CapturePerson` is the only correct path for person data. The `Capture` command is for journey/form data only.

3. **Review and monitoring**: A query or scheduled job can scan `journey_view.accumulated_data` for patterns that look like PII (email regex, phone patterns) and flag them. This is a detective control, not a preventive one, but it catches mistakes.

The blocklist in the schema validator is the primary enforcement mechanism. It runs inside the aggregate's `handle` method for every `Capture` command, before any events are emitted.

---

## Testing Strategy

### Unit tests

| Test | Description |
|---|---|
| `test_encrypt_decrypt_round_trip` | PII encrypts and decrypts correctly with a known key |
| `test_encrypt_produces_different_ciphertext` | Same plaintext with different nonces produces different ciphertext |
| `test_decrypt_with_wrong_key_fails` | Decryption with an incorrect key returns an error |
| `test_decrypt_with_deleted_key_returns_redacted` | When key store returns `None`, redacted sentinels are substituted |
| `test_non_pii_events_pass_through` | `Started`, `Modified`, `Completed`, etc. are not modified by the crypto layer |
| `test_subject_id_remains_plaintext` | The `subject_id` field in an encrypted `PersonCaptured` payload is readable without decryption |
| `test_aad_binding` | Ciphertext cannot be moved to a different aggregate/sequence position |

### Integration tests

| Test | Description |
|---|---|
| `test_persist_and_load_with_encryption` | Write a `PersonCaptured` event through the crypto repository, read it back, verify plaintext matches |
| `test_shred_then_load` | Write event, delete key, load events, verify redacted sentinels |
| `test_shred_clears_read_model` | Write event, project to `journey_person`, shred, verify `journey_person` row is gone |
| `test_projection_receives_plaintext` | Verify the query dispatcher receives decrypted events (not ciphertext) |
| `test_pii_firewall_rejects_email_in_capture` | `Capture` command with an `email` field in `data` is rejected by schema validation |
| `test_multiple_journeys_same_subject` | Two journeys with the same `subject_id` — shredding deletes PII from both |

### Property-based tests

| Test | Description |
|---|---|
| `test_event_stream_integrity` | After encrypt → persist → load → decrypt, the full event stream (PII and non-PII events interleaved) is identical to the original |
| `test_shredding_is_complete` | After shredding, no plaintext PII for the subject exists in any table (events, journey_person, journey_view) |

---

## Migration Plan

### Phase 1: Infrastructure

1. Add `subject_encryption_keys` table (migration)
2. Add `subject_id` column to `journey_person` (migration)
3. Implement `KeyStore` trait and `PostgresKeyStore`
4. Implement `PiiCipher` (AES-256-GCM encryption/decryption)
5. Implement `CryptoShreddingEventRepository<R>`

### Phase 2: Domain changes

6. Add `subject_id` to `CapturePerson` command and `PersonCaptured` event
7. Add `subject_id` to `Journey` aggregate struct and `apply`
8. Add `SubjectForgotten` event variant
9. Update `StructuredJourneyViewRepository` projection to include `subject_id` and handle redacted sentinels

### Phase 3: Wiring

10. Update `config.rs` to wrap `PostgresEventRepository` with `CryptoShreddingEventRepository`
11. Update `ApplicationState` and type aliases
12. Add `ForgetSubject` API endpoint

### Phase 4: PII firewall

13. Extend schema validation to reject PII field names in `Capture` payloads
14. Add monitoring/scanning for PII in `accumulated_data`

### Phase 5: Backfill (if existing data contains plaintext PII)

15. Write a one-time migration script that:
    - Reads all existing `PersonCaptured` events from the event store
    - Generates DEKs for each distinct subject
    - Re-encrypts the payloads in place (this is the one permitted mutation of event data)
    - Populates `subject_id` in `journey_person` rows

This backfill is a sensitive operation — it should be run in a maintenance window with a full database backup taken first.

---

## Future Considerations

### External KMS for KEK management

The initial implementation can use a KEK loaded from an environment variable or secrets manager. For production hardening, the KEK should be managed by an external KMS (AWS KMS, GCP KMS, or HashiCorp Vault) with:

- Automatic key rotation
- Access audit logging
- Hardware security module (HSM) backing

The `PiiCipher` abstraction is designed to make this a drop-in change — swap the `wrap_dek`/`unwrap_dek` implementation to call the KMS API instead of doing local AES key wrapping.

### Extending to other PII event types

The `CryptoShreddingEventRepository` identifies PII events by `event_type` string matching. If new event types containing PII are introduced (e.g., `AddressCaptured`, `PaymentDetailsCaptured`), they simply need to be added to the match list in the crypto layer. The pattern is designed to be repeatable.

### Right to portability (GDPR Article 20)

The same key-store and crypto infrastructure supports data portability requests. To export all PII for a subject: query all events across all aggregates where `subject_id` matches, decrypt them, and package them in a portable format. The `subject_id` as a cross-journey stable identifier makes this query straightforward.

### Key rotation

DEK rotation (re-encrypting existing events with a new key) is possible but expensive — it requires reading, decrypting, re-encrypting, and rewriting all events for a subject. KEK rotation is simpler — re-wrap all DEKs with the new KEK without touching the event store. The design should prioritise KEK rotation and treat DEK rotation as an exceptional maintenance operation.

### Aggregate snapshots

If snapshots are introduced and the aggregate stores PII (e.g., person name for display), snapshot payloads would also need encryption. The `get_snapshot` method on the crypto repository wrapper is the natural place for this. For now, the aggregate does not store PII (only `subject_id`), so snapshots are clean.
