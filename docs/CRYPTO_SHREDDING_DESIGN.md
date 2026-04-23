# General Data Protection Regulation (GDPR) Crypto-Shredding for personally identifiable information (PII) Data

> **⚠️ SUPERSEDED**
>
> This document describes the original single-subject crypto-shredding design.
> It has been superseded by [MULTI_SUBJECT_DESIGN.md](./MULTI_SUBJECT_DESIGN.md),
> which extends the model to support multiple independent data subjects per journey
> and eliminates the `journey_subject_mapping` table.
>
> The implementation reflects the new design. This document is retained for
> historical reference only.

| | |
|---|---|
| **Service** | Journey Dynamics |
| **Feature** | Crypto-shredding for GDPR right-to-erasure |
| **Status** | Superseded — see [MULTI_SUBJECT_DESIGN.md](./MULTI_SUBJECT_DESIGN.md) |

---

## Table of Contents

1. [Motivation](#motivation)
2. [Problem Analysis](#problem-analysis)
   - [PII in PersonCaptured events](#pii-in-personcaptured-events)
   - [PII in Modified events](#pii-in-modified-events)
   - [PII in read-side projections](#pii-in-read-side-projections)
3. [Design Overview](#design-overview)
4. [Key Concepts](#key-concepts)
   - [Subject ID](#subject-id)
   - [Data Encryption Key (DEK)](#data-encryption-key-dek)
   - [Journey–Subject Association](#journeysubject-association)
   - [PII Classification](#pii-classification)
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
10. [Testing Strategy](#testing-strategy)
11. [Migration Plan](#migration-plan)
12. [Future Considerations](#future-considerations)

---

## Motivation

GDPR Article 17 grants data subjects the "right to erasure" — the right to have their personal data deleted. In a conventional database this is straightforward: delete the rows. In an event-sourced system, however, events are immutable and append-only. Deleting or mutating events would break the event stream's integrity, aggregate rehydration, and audit guarantees.

**Crypto-shredding** solves this tension. Instead of deleting the events themselves, we encrypt PII fields within events using a per-subject encryption key. When a data subject exercises their right to erasure, we delete the key. The encrypted PII in the event store becomes permanently unreadable — effectively erased — while the event stream's structure, ordering, and non-PII fields remain intact.

---

## Problem Analysis

PII currently enters the system through two distinct paths, and persists in three locations. All three must be addressed.

### PII in `PersonCaptured` events

The `CapturePerson` command produces a `PersonCaptured` event containing `name`, `email`, and `phone`. These fields are serialized as plaintext JSON in the `events.payload` column.

This path is well-contained: the PII fields are clearly identified and the event type is dedicated to person data. Encrypting these fields is straightforward.

### PII in `Modified` events

The `Capture` command produces a `Modified` event carrying a `data: Value` field — arbitrary JSON from form submissions. In practice this JSON routinely contains PII. The flight-booking example schema makes this concrete:

| Schema type | PII fields |
|---|---|
| `PassengerDetail` | `firstName`, `lastName`, `dateOfBirth`, `passportNumber`, `nationality` |
| `Payment` | `transaction_id` |
| `SearchCriteria` | `origin`, `destination`, `departureDate` (linkable to a person once a subject is established) |

This data flows through the `Modified` event into `accumulated_data` on both the aggregate (in-memory) and the `journey_view` table (persisted). Crucially, the `Capture` path is the system's primary data-ingestion mechanism — blocking PII from it is not feasible. The decision engine needs this data to route the journey.

**This is the harder problem.** A design that only encrypts `PersonCaptured` events and tries to "firewall" PII out of `Modified` events is insufficient. PII in JSON payloads is not a bug — it is a fundamental characteristic of the domain.

### PII in read-side projections

PII ends up in two read-model locations:

| Table | PII content |
|---|---|
| `journey_person` | `name`, `email`, `phone` — projected from `PersonCaptured` |
| `journey_view` | `accumulated_data` (JSONB) — projected from `Modified`, contains passenger details, payment info, etc. |

Both must be scrubbed when a subject exercises their right to erasure.

### Summary of PII surfaces

| Location | Source | Contains PII | Encrypted today |
|---|---|---|---|
| `events.payload` — `PersonCaptured` | `CapturePerson` command | ✅ name, email, phone | ❌ |
| `events.payload` — `Modified` | `Capture` command | ✅ passenger details, payment, etc. | ❌ |
| `journey_person` table | Read-side projection | ✅ name, email, phone | ❌ |
| `journey_view.accumulated_data` | Read-side projection | ✅ merged form data | ❌ |
| `events.payload` — all other event types | Various | ❌ | N/A |

---

## Design Overview

The core insight is that under GDPR, "personal data" is any information relating to an identified or identifiable natural person. Once a journey is linked to a subject (via `CapturePerson`), **all data captured in that journey is personal data** — the flight destination they searched for, the seat class they selected, the payment method they chose. It all relates to an identifiable person.

This leads to a **journey-level encryption** approach: once a subject is associated with a journey, all event payloads in that journey that carry data fields are encrypted with the subject's key. This is simpler, safer, and more legally sound than attempting field-level PII classification.

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
        │     "PersonCaptured" →                │
        │       record journey→subject mapping  │
        │       encrypt PII fields with DEK     │
        │     "Modified" →                      │
        │       if journey has subject →        │
        │         encrypt data field with DEK   │
        │     other events → pass through       │
        │                                       │
        │   get_events():                       │
        │     for each event →                  │
        │       if encrypted fields present →   │
        │         look up DEK                   │
        │         if found → decrypt            │
        │         if missing → redact           │
        └───────────────────┬───────────────────┘
                            │
        ┌───────────────────┴───────────────────┐
        │       PostgresEventRepository         │
        │       (unchanged — stores cipher-     │
        │        text in payload column)        │
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

The `subject_id` is stored in plaintext in the event payload (never encrypted). This allows the crypto layer to find the right key on the read path without needing to decrypt anything first.

### Data Encryption Key (DEK)

Each subject gets a unique symmetric encryption key (the DEK). The DEK is:

- Generated on first use (when the first `PersonCaptured` event is persisted for a given `subject_id`)
- Stored in a dedicated `subject_encryption_keys` table
- Used to encrypt/decrypt all PII fields for that subject across all events and all journeys
- Itself encrypted at rest with a Key Encryption Key (KEK) — see [Encryption Scheme](#encryption-scheme)

### Journey–Subject Association

The crypto layer must know which journeys belong to which subjects, so that `Modified` events can be encrypted with the correct key. This is maintained via a `journey_subject_mapping` table, populated by the crypto layer when it persists a `PersonCaptured` event.

```
    journey_subject_mapping
    ┌─────────────────────────────┐
    │ aggregate_id  │ subject_id  │
    │───────────────│─────────────│
    │ journey-abc   │ subj-123    │
    │ journey-def   │ subj-123    │  ← same person, two journeys
    │ journey-ghi   │ subj-456    │
    └─────────────────────────────┘
```

When the crypto layer sees a `Modified` event, it checks this mapping by `aggregate_id`:

- **Mapping exists** → encrypt the `data` field with the subject's DEK
- **No mapping** → the journey has no associated subject yet; pass through unencrypted

This means events persisted *before* `CapturePerson` is called remain in plaintext. This is acceptable: at that point the data is not yet linked to an identifiable person. The moment the person is identified (via `CapturePerson`), all subsequent data is encrypted. See [Pre-Subject Events](#pre-subject-events) for further discussion.

### PII Classification

Rather than classifying individual fields as PII, we adopt a journey-level approach:

| Event type | Encryption rule |
|---|---|
| `PersonCaptured` | **Always encrypted.** The `name`, `email`, and `phone` fields are encrypted. The `subject_id` remains in plaintext. |
| `Modified` | **Encrypted if the journey has a subject.** The entire `data` field is encrypted. The `step` field remains in plaintext (it is a workflow routing label, not personal data). |
| `Started` | Never encrypted. |
| `WorkflowEvaluated` | Never encrypted. Suggested actions are workflow metadata. |
| `StepProgressed` | Never encrypted. Step names are workflow metadata. |
| `Completed` | Never encrypted. |
| `SubjectForgotten` | Never encrypted. Contains only the `subject_id` (not PII). |

This approach is:

- **Simple** — no need for field-level schema annotations or PII-pattern matching
- **Safe** — no risk of missing a PII field buried in a JSON payload
- **Legally sound** — all data relating to an identified person is treated as personal data, which aligns with GDPR's broad definition
- **Transparent to the domain layer** — the aggregate and decision engine always work with decrypted data in memory

The trade-off is that non-PII data in `Modified` events (e.g., trip type, cabin class) also becomes unreadable after shredding. This is acceptable: if the subject exercises their right to erasure, their entire journey context should be considered personal data. The structural events (`Started`, `StepProgressed`, `Completed`) remain readable and are sufficient for aggregate lifecycle management.

---

## Architecture

### Crypto-Shredding Event Repository

A new struct that wraps any `PersistedEventRepository` and adds encryption/decryption:

```rust
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    inner: R,
    key_store: Arc<dyn KeyStore>,
    subject_mapping: Arc<dyn SubjectMapping>,
    cipher: PiiCipher,
}
```

It implements `PersistedEventRepository` by delegating to `inner`, intercepting `SerializedEvent` values on both the write and read paths.

#### Write path (`persist`)

```
for each SerializedEvent in events:

    if event.event_type == "PersonCaptured":
        subject_id = event.payload["PersonCaptured"]["subject_id"]
        subject_mapping.associate(event.aggregate_id, subject_id)
        dek = key_store.get_or_create_key(subject_id)
        pii = extract { name, email, phone } from event.payload["PersonCaptured"]
        aad = aggregate_id || sequence
        encrypted = cipher.encrypt(dek, serialize(pii), aad)
        replace payload["PersonCaptured"] with:
            { "subject_id": <uuid>, "encrypted_pii": <base64>, "nonce": <base64> }

    else if event.event_type == "JourneyModified":
        subject_id = subject_mapping.get_subject(event.aggregate_id)
        if subject_id is Some:
            dek = key_store.get_or_create_key(subject_id)
            data = extract event.payload["Modified"]["data"]
            aad = aggregate_id || sequence
            encrypted = cipher.encrypt(dek, serialize(data), aad)
            replace payload["Modified"]["data"] with:
                { "encrypted_data": <base64>, "nonce": <base64> }

    // All other event types: pass through unmodified

delegate to inner.persist(events)
```

#### Read path (`get_events`, `get_last_events`, `stream_events`, `stream_all_events`)

```
events = inner.get_events(aggregate_id)
for each SerializedEvent in events:

    if event.event_type == "PersonCaptured"
       and event.payload contains "encrypted_pii":
        subject_id = event.payload["PersonCaptured"]["subject_id"]
        dek = key_store.get_key(subject_id)
        if dek is Some:
            aad = aggregate_id || sequence
            pii = cipher.decrypt(dek, encrypted_pii, nonce, aad)
            restore full payload: { subject_id, name, email, phone }
        else:
            // Key deleted — subject was forgotten
            substitute: { subject_id, name: "[redacted]", email: "[redacted]", phone: null }

    else if event.event_type == "JourneyModified"
       and event.payload["Modified"]["data"] contains "encrypted_data":
        subject_id = subject_mapping.get_subject(event.aggregate_id)
        if subject_id is Some:
            dek = key_store.get_key(subject_id)
        if dek is Some:
            aad = aggregate_id || sequence
            data = cipher.decrypt(dek, encrypted_data, nonce, aad)
            restore full payload: { step, data: <decrypted> }
        else:
            // Key deleted — subject was forgotten
            substitute: { step, data: {} }

return events
```

The redacted sentinels allow the aggregate and projections to process events without crashing. For `PersonCaptured`, `"[redacted]"` is a valid `String`. For `Modified`, an empty `{}` means the accumulated data for this step is lost — which is the desired outcome after shredding. The `step` field remains readable, so `StepProgressed` and workflow evaluation history are preserved.

Note: the presence of `"encrypted_pii"` or `"encrypted_data"` keys in the payload is how the read path distinguishes encrypted events from legacy plaintext events. Events persisted before the crypto layer was introduced will not have these keys and will pass through unmodified.

#### Snapshot path (`get_snapshot`)

If snapshots are used in the future, the aggregate's serialized state could contain PII (if person data is ever added to the aggregate). For now, `PersonCaptured` produces minimal aggregate state change (only `subject_id`, which is not PII), so snapshots are clean. The `accumulated_data` on the aggregate *does* contain PII in memory, and would be serialized into a snapshot. If snapshots are introduced, the crypto layer must also encrypt the `accumulated_data` field in the snapshot payload. The `get_snapshot` method on the crypto repository wrapper is the natural place for this.

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

### Subject Mapping

A trait and Postgres implementation for the journey–subject association:

```rust
#[async_trait]
pub trait SubjectMapping: Send + Sync {
    /// Record that a journey (aggregate) belongs to a subject.
    async fn associate(&self, aggregate_id: &str, subject_id: &Uuid) -> Result<(), MappingError>;

    /// Look up the subject for a journey. Returns None if no subject has been
    /// associated (i.e., CapturePerson has not yet been called for this journey).
    async fn get_subject(&self, aggregate_id: &str) -> Result<Option<Uuid>, MappingError>;

    /// Find all journeys belonging to a subject (used during shredding
    /// to clean up read-model projections).
    async fn get_journeys(&self, subject_id: &Uuid) -> Result<Vec<String>, MappingError>;
}
```

The implementation is backed by the `journey_subject_mapping` table (see [Database Schema Changes](#database-schema-changes)). For performance, the crypto layer should cache mappings in memory with a bounded Least Recently Used (LRU) cache, since `get_subject` is called on every `Modified` event during both persist and load.

### Encryption Scheme

| Layer | Algorithm | Purpose |
|---|---|---|
| **Field encryption (DEK)** | Advanced Encryption Standard (AES)-256-Galois/Counter Mode (GCM) | Encrypts PII/data fields within events. Authenticated encryption ensures integrity. |
| **Key encryption (KEK)** | AES-256-Key Wrap with Padding (KWP) or Key Management Service (KMS) envelope encryption | Encrypts DEKs at rest in the key store. The KEK is either a static secret from config/environment or, preferably, a key in an external KMS (AWS KMS, GCP KMS, HashiCorp Vault). |

AES-256-GCM requires a unique nonce per encryption operation. The nonce is stored alongside the ciphertext in the event payload. The authenticated additional data (AAD) includes the `aggregate_id` and `sequence` number, binding the ciphertext to its position in the stream and preventing event payload transplantation.

```rust
pub struct PiiCipher {
    // If using a local KEK for DEK wrapping:
    kek: Zeroizing<Vec<u8>>,
}

impl PiiCipher {
    /// Encrypt a plaintext payload with the given DEK.
    /// aad should include aggregate_id and sequence to bind ciphertext to event position.
    pub fn encrypt(&self, dek: &KeyMaterial, plaintext: &[u8], aad: &[u8]) -> EncryptedPayload;

    /// Decrypt a ciphertext payload with the given DEK.
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

Add `subject_id` to `CapturePerson`:

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

The `ForgetSubject` operation is handled outside the journey aggregate — it is not a journey command. It operates across all journeys for a subject and is exposed as a dedicated API endpoint. See [Shredding Flow](#shredding-flow).

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

#### What events look like in the event store

A `PersonCaptured` event is stored with encrypted PII:

```json
{
    "PersonCaptured": {
        "subject_id": "550e8400-e29b-41d4-a716-446655440000",
        "encrypted_pii": "dGhpcyBpcyBiYXNlNjQgZW5jcnlwdGVk...",
        "nonce": "dW5pcXVlIG5vbmNl..."
    }
}
```

A `Modified` event for a journey with an associated subject:

```json
{
    "Modified": {
        "step": "passenger_details",
        "data": {
            "encrypted_data": "YW5vdGhlciBiYXNlNjQgY2lwaGVydGV4dA==...",
            "nonce": "YW5vdGhlciBub25jZQ==..."
        }
    }
}
```

A `Modified` event for a journey with *no* associated subject (pre-`CapturePerson`, or a journey that never captures a person):

```json
{
    "Modified": {
        "step": "search",
        "data": {
            "search": {
                "tripType": "round-trip",
                "origin": "LHR",
                "destination": "JFK",
                "departureDate": "2026-03-15"
            }
        }
    }
}
```

The domain layer never sees the encrypted forms — it always works with the decrypted variants.

### Aggregate

The `apply` method for `PersonCaptured` currently does nothing. With the introduction of `subject_id`, we store it on the aggregate so that business rules can reference it if needed:

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

Add `subject_id` column. The projection continues to receive decrypted events (the crypto layer decrypts before events reach the query dispatchers). When the key has been deleted, the projection receives redacted sentinels:

```rust
JourneyEvent::PersonCaptured { subject_id, name, email, phone } => {
    if name == "[redacted]" {
        // Subject has been forgotten — skip projection update
        return Ok(());
    }
    // ... normal upsert logic, now including subject_id ...
}
```

### `journey_view` table — `accumulated_data`

This is the more complex case. The `Modified` event projection merges `data` into `accumulated_data` via a JSON merge:

```sql
UPDATE journey_view
SET accumulated_data = accumulated_data || $2, ...
WHERE id = $1
```

After shredding, `Modified` events for the shredded subject decrypt to `data: {}`. On a projection rebuild, this would produce empty merges — effectively dropping the shredded data from `accumulated_data`. But we do not want to rely on full projection rebuilds for the normal shredding flow.

Instead, shredding directly clears the read model:

```sql
-- Clear accumulated_data for all journeys belonging to the shredded subject
UPDATE journey_view
SET accumulated_data = '{}', updated_at = CURRENT_TIMESTAMP
WHERE id IN (
    SELECT aggregate_id FROM journey_subject_mapping
    WHERE subject_id = $1
);

-- Delete person records
DELETE FROM journey_person WHERE subject_id = $1;
```

This is fast, atomic, and does not require replaying events.

If a more surgical approach is desired (preserving non-PII accumulated data for the journey's structural integrity), a projection rebuild for affected journeys can be triggered after shredding. The rebuilt projection would replay all events through the crypto layer, which would:
- Decrypt non-encrypted events (pre-subject) normally
- Return `data: {}` for encrypted events whose key has been deleted
- Return `"[redacted]"` sentinels for `PersonCaptured` events

The result would be `accumulated_data` containing only the pre-subject data.

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

### New table: `journey_subject_mapping`

```sql
CREATE TABLE journey_subject_mapping (
    aggregate_id    TEXT        NOT NULL PRIMARY KEY,
    subject_id      UUID        NOT NULL,
    created_at      TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_journey_subject_mapping_subject_id
    ON journey_subject_mapping(subject_id);
```

This table is populated by the crypto layer when it persists a `PersonCaptured` event, and queried on every `Modified` event to determine whether encryption is required.

### Altered table: `journey_person`

```sql
ALTER TABLE journey_person ADD COLUMN subject_id UUID;
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
```

### No changes to: `events`

The `events` table schema is unchanged. The `payload` column continues to store JSON — it just happens to contain ciphertext for encrypted events instead of plaintext. The crypto layer is transparent to the storage layer.

---

## Shredding Flow

### API endpoint

```
DELETE /subjects/{subject_id}
```

This is a dedicated endpoint, separate from the journey command/query endpoints. It does not go through the Command Query Responsibility Segregation (CQRS) command handler because it operates across aggregates.

### Sequence

```
    Client                API              KeyStore        SubjectMapping      Read Model
      │                    │                  │                 │                  │
      │  DELETE /subjects  │                  │                 │                  │
      │  /{subject_id}     │                  │                 │                  │
      │───────────────────>│                  │                 │                  │
      │                    │                  │                 │                  │
      │                    │  delete_key()    │                 │                  │
      │                    │─────────────────>│                 │                  │
      │                    │       Ok(())     │                 │                  │
      │                    │<─────────────────│                 │                  │
      │                    │                  │                 │                  │
      │                    │  get_journeys()  │                 │                  │
      │                    │──────────────────────────────────> │                  │
      │                    │  [journey-abc, journey-def]        │                  │
      │                    │<────────────────────────────────── │                  │
      │                    │                  │                 │                  │
      │                    │  Clear accumulated_data & person rows                 │
      │                    │─────────────────────────────────────────────────────> │
      │                    │                  │                 │                  │
      │                    │  (emit SubjectForgotten event for audit trail)        │
      │                    │                  │                 │                  │
      │    204 No Content  │                  │                 │                  │
      │<───────────────────│                  │                 │                  │
```

### What happens after shredding

1. **Event store**: `PersonCaptured` events still exist, but their PII fields are AES-256-GCM ciphertext. `Modified` events for post-subject captures still exist, but their `data` fields are ciphertext. The key is gone. The data is permanently unreadable.

2. **Aggregate rehydration**: When a shredded journey is loaded, the crypto layer detects the missing key and substitutes sentinels. `PersonCaptured` yields `"[redacted]"` strings. `Modified` yields `data: {}`. The aggregate loads successfully — `subject_id` is set, `state` and `current_step` are preserved, but `accumulated_data` and person details are empty.

3. **Read model**: The `journey_person` rows for this subject have been deleted. The `journey_view.accumulated_data` for affected journeys has been cleared to `'{}'`. Queries for this subject return no personal data.

4. **Workflow decisions and step history**: `WorkflowEvaluated` and `StepProgressed` events are not encrypted and remain fully readable. The structural history of the journey — which steps were taken, what actions were suggested — survives shredding. This is intentional: these are workflow metadata, not personal data.

5. **Audit trail**: A `SubjectForgotten { subject_id }` event records when the erasure occurred, without containing any PII.

### Pre-subject events

Events persisted before `CapturePerson` is called for a journey remain in plaintext. This includes any `Modified` events from early journey steps (e.g., a flight search before the user identifies themselves).

This is acceptable for two reasons:

1. **Legal**: Before a subject is identified, the data is not linked to a natural person. Under GDPR, it is not personal data until it can be attributed to someone.

2. **Practical**: If the search data itself is sensitive (e.g., destination reveals health-related travel), the correct mitigation is to require `CapturePerson` early in the journey — ideally as the first step after `Start`. This is an API-level convention, not an encryption-layer concern.

For maximum safety, journey designs should call `CapturePerson` as early as possible — before any `Capture` commands that include sensitive form data.

---

## Testing Strategy

### Unit tests

| Test | Description |
|---|---|
| `test_encrypt_decrypt_round_trip` | PII encrypts and decrypts correctly with a known key |
| `test_encrypt_produces_different_ciphertext` | Same plaintext with different nonces produces different ciphertext |
| `test_decrypt_with_wrong_key_fails` | Decryption with an incorrect key returns an error |
| `test_decrypt_with_deleted_key_returns_redacted` | When key store returns `None`, `PersonCaptured` yields `"[redacted]"` sentinels |
| `test_decrypt_modified_with_deleted_key_returns_empty` | When key store returns `None`, `Modified` yields `data: {}` |
| `test_non_pii_events_pass_through` | `Started`, `WorkflowEvaluated`, `StepProgressed`, `Completed` are not modified by the crypto layer |
| `test_subject_id_remains_plaintext` | The `subject_id` field in an encrypted `PersonCaptured` payload is readable without decryption |
| `test_step_remains_plaintext` | The `step` field in an encrypted `Modified` payload is readable without decryption |
| `test_aad_binding` | Ciphertext cannot be moved to a different aggregate/sequence position |
| `test_modified_before_subject_not_encrypted` | `Modified` events persisted before `CapturePerson` remain plaintext |
| `test_modified_after_subject_encrypted` | `Modified` events persisted after `CapturePerson` are encrypted |

### Integration tests

| Test | Description |
|---|---|
| `test_persist_and_load_person_captured` | Write a `PersonCaptured` event through the crypto repo, read it back, verify plaintext matches |
| `test_persist_and_load_modified_with_subject` | Write `PersonCaptured` then `Modified` through the crypto repo, verify `Modified.data` is encrypted in DB but decrypted on load |
| `test_persist_modified_without_subject` | Write `Modified` without a prior `CapturePerson`, verify it is stored in plaintext |
| `test_shred_then_load_person_captured` | Write event, delete key, load events, verify redacted sentinels |
| `test_shred_then_load_modified` | Write `PersonCaptured` + `Modified`, delete key, load events, verify `data: {}` |
| `test_shred_clears_read_model` | Write events, project to read model, shred, verify `journey_person` rows deleted and `accumulated_data` cleared |
| `test_projection_receives_plaintext` | Verify the query dispatcher receives decrypted events (not ciphertext) |
| `test_multiple_journeys_same_subject` | Two journeys with the same `subject_id` — shredding affects both |
| `test_legacy_plaintext_events_still_load` | Events persisted before the crypto layer was introduced load correctly (no encrypted markers present → no decryption attempted) |
| `test_mixed_event_stream` | A stream with pre-subject plaintext `Modified`, `PersonCaptured`, and post-subject encrypted `Modified` all load correctly |

### Property-based tests

| Test | Description |
|---|---|
| `test_event_stream_integrity` | After encrypt → persist → load → decrypt, the full event stream (all event types interleaved) is identical to the original |
| `test_shredding_is_complete` | After shredding, no plaintext PII for the subject exists in any table (`events`, `journey_person`, `journey_view`) |

---

## Migration Plan

### Phase 1: Infrastructure

1. Add `subject_encryption_keys` table (migration)
2. Add `journey_subject_mapping` table (migration)
3. Add `subject_id` column to `journey_person` (migration)
4. Implement `KeyStore` trait and `PostgresKeyStore`
5. Implement `SubjectMapping` trait and `PostgresSubjectMapping`
6. Implement `PiiCipher` (AES-256-GCM encryption/decryption, KEK wrapping)

### Phase 2: Crypto repository

7. Implement `CryptoShreddingEventRepository<R>` wrapping `PersistedEventRepository`
8. Implement write-path encryption for `PersonCaptured` events
9. Implement write-path encryption for `Modified` events (with subject lookup)
10. Implement read-path decryption and redaction

### Phase 3: Domain changes

11. Add `subject_id` to `CapturePerson` command and `PersonCaptured` event
12. Add `subject_id` to `Journey` aggregate struct and `apply`
13. Add `SubjectForgotten` event variant
14. Update `StructuredJourneyViewRepository` projection to include `subject_id` and handle redacted sentinels

### Phase 4: Wiring

15. Update `config.rs` to wrap `PostgresEventRepository` with `CryptoShreddingEventRepository`
16. Update `ApplicationState` and type aliases
17. Add `DELETE /subjects/{subject_id}` API endpoint for shredding
18. Add shredding logic: delete key, clear read model, emit audit event

### Phase 5: Backfill (if existing data contains plaintext PII)

19. Write a one-time migration script that:
    - Reads all existing `PersonCaptured` events from the event store
    - Generates DEKs for each distinct subject (or uses a default subject_id derived from email for legacy events that lack one)
    - Populates `journey_subject_mapping` for all affected journeys
    - Re-encrypts `PersonCaptured` payloads in place
    - Re-encrypts `Modified` payloads for journeys with established subjects
    - Populates `subject_id` in `journey_person` rows

This backfill is a sensitive operation — it should be run in a maintenance window with a full database backup taken first. It is the one permitted mutation of historical event data.

---

## Future Considerations

### External KMS for KEK management

The initial implementation can use a KEK loaded from an environment variable or secrets manager. For production hardening, the KEK should be managed by an external KMS (AWS KMS, GCP KMS, or HashiCorp Vault) with:

- Automatic key rotation
- Access audit logging
- Hardware security module (HSM) backing

The `PiiCipher` abstraction is designed to make this a drop-in change — swap the `wrap_dek`/`unwrap_dek` implementation to call the KMS API instead of doing local AES key wrapping.

### Extending to other PII event types

The `CryptoShreddingEventRepository` identifies encryptable events by `event_type` string matching. If new event types containing PII are introduced (e.g., `AddressCaptured`, `PaymentDetailsCaptured`), they simply need to be added to the match list in the crypto layer. The pattern is designed to be repeatable.

### Right to portability (GDPR Article 20)

The same key-store and crypto infrastructure supports data portability requests. To export all PII for a subject: use `journey_subject_mapping` to find all journeys, load and decrypt all events, and package them in a portable format. The `subject_id` as a cross-journey stable identifier makes this query straightforward.

### Key rotation

DEK rotation (re-encrypting existing events with a new key) is possible but expensive — it requires reading, decrypting, re-encrypting, and rewriting all events for a subject. KEK rotation is simpler — re-wrap all DEKs with the new KEK without touching the event store. The design should prioritise KEK rotation and treat DEK rotation as an exceptional maintenance operation.

### Aggregate snapshots

If snapshots are introduced, the aggregate's serialized state will include `accumulated_data` — which contains PII (the merged form data). Snapshot payloads must be encrypted using the subject's DEK. The `get_snapshot` / `persist` (snapshot path) methods on the crypto repository wrapper are the natural place for this. The `subject_id` on the aggregate (which would be serialized in the snapshot) provides the key lookup handle. If the key has been deleted, the snapshot should be treated as stale and the aggregate rehydrated from events instead.

### Caching and performance

The crypto layer adds latency: key lookups, subject mapping lookups, and AES operations on every event. Mitigations:

- **LRU cache for subject mappings**: Once a journey → subject association is established, it never changes. Cache aggressively.
- **LRU cache for DEKs**: Cache unwrapped DEKs in memory (with Time to Live (TTL)). Use `Zeroizing<Vec<u8>>` to ensure keys are cleared from memory when evicted.
- **Batch key lookups**: When loading a full event stream for an aggregate, look up the subject mapping once, then use the cached DEK for all events.
- **Skip non-encrypted events early**: Check for `"encrypted_pii"` / `"encrypted_data"` keys in the JSON before attempting any crypto operations on the read path.
