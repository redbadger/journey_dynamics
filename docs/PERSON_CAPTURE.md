# Capturing Person Data in Journeys

This guide explains how to capture structured personally identifiable information (PII) — name, email, and phone — in journeys using the
`CapturePerson` command, how the `subject_id` field ties a person's identity across journeys,
and what to expect from the encryption and crypto-shredding layer.

## Overview

The `CapturePerson` command captures a real person's identity into a journey. When executed it:

1. Validates the journey is in a started (not completed) state
2. Emits a `PersonCaptured` event carrying the person's PII and their `subject_id`
3. Persists the event to the event store — **PII fields are encrypted at rest** (see
   [CRYPTO_SHREDDING_DESIGN.md](./CRYPTO_SHREDDING_DESIGN.md) for full details)
4. Projects the decrypted data into the `journey_person` table for efficient querying

## The `subject_id` Field

`subject_id` is a stable, caller-supplied `Uuid` that identifies the **data subject** — the real
person — independently of any individual journey.

| Property | Details |
|---|---|
| **Who supplies it** | The caller (your application layer or API client) |
| **When to create it** | Once per person, at account creation or first contact; reuse it for every subsequent journey |
| **Why it matters** | One `subject_id` may span many journeys. A single `DELETE /subjects/{subject_id}` call shreds that person's PII across **all** of their journeys simultaneously |
| **Format** | Standard UUID v4 |

The `subject_id` is stored in both the `journey_person` read-model table and the
`journey_subject_mapping` table (maintained by the crypto layer). It is also embedded in every
`PersonCaptured` event so the aggregate and projections can reference it.

## Rust Usage

```rust
use journey_dynamics::domain::commands::JourneyCommand;
use uuid::Uuid;

let subject_id = Uuid::new_v4(); // generate once per person; reuse across journeys

// Capture person with phone number
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        subject_id,
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;

// Capture person without phone number
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        subject_id,
        name: "Bob Smith".to_string(),
        email: "bob.smith@example.com".to_string(),
        phone: None,
    },
).await?;
```

## HTTP API

Send a `POST` to `/journeys/{journey_id}` with a `CapturePerson` payload:

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "subject_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      "name": "Alice Johnson",
      "email": "alice.johnson@example.com",
      "phone": "+1-555-0123"
    }
  }'
```

`subject_id` must be a valid UUID. Generate it in your identity layer and keep it stable for the
lifetime of the person's relationship with your system.

## PII Encryption at Rest

All three PII fields (`name`, `email`, `phone`) in every `PersonCaptured` event are **encrypted
before being written to the event store** using Advanced Encryption Standard 256-bit Galois/Counter Mode (AES-256-GCM) with a per-subject Data Encryption
Key (DEK). The `journey_person` read-model table stores the projected plaintext (decrypted on the
way out of the event store), but the event log itself never contains plaintext PII.

`Modified` events emitted after a `CapturePerson` on the same journey are also encrypted using the
same subject DEK.

See [CRYPTO_SHREDDING_DESIGN.md](./CRYPTO_SHREDDING_DESIGN.md) for the full design: key
management, authenticated additional data (AAD) binding, the `CryptoShreddingEventRepository` wrapper, and the shredding endpoint.

## Database Schema

### `journey_person`

```sql
CREATE TABLE journey_person
(
    id          SERIAL      NOT NULL PRIMARY KEY,
    journey_id  UUID        NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    subject_id  UUID,
    name        TEXT        NOT NULL,
    email       TEXT        NOT NULL,
    phone       TEXT,
    created_at  TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (journey_id)
);

CREATE INDEX idx_journey_person_journey_id ON journey_person(journey_id);
CREATE INDEX idx_journey_person_email      ON journey_person(email);
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
```

`subject_id` is nullable to remain compatible with legacy rows captured before crypto-shredding
support was introduced.

## Querying Person Data

All query methods live on `StructuredJourneyViewRepository`.

### `load_person` — get the person for a specific journey

```rust
let repo = StructuredJourneyViewRepository::new(pool);

if let Some(person) = repo.load_person(&journey_id).await? {
    println!("Name:  {}", person.name);
    println!("Email: {}", person.email);
    println!("Phone: {:?}", person.phone);
}
```

If the subject's DEK has been deleted (i.e. the person has been forgotten), the projection row is
also deleted and `load_person` returns `None`.

### `find_by_email` — look up journeys by email address

```rust
let journeys = repo.find_by_email("alice.johnson@example.com").await?;

for journey in journeys {
    println!("Journey {}: {:?}", journey.id, journey.state);
}
```

### `load_all_persons` — enumerate all known persons

```rust
let persons = repo.load_all_persons().await?;

for person in persons {
    println!("{} — {} (journey {})", person.name, person.email, person.journey_id);
}
```

## Event Flow

```
CapturePerson { subject_id, name, email, phone }
        │
        ▼
Journey::handle()  ──validates──▶  journey must be Started, not Completed
        │
        ▼
PersonCaptured { subject_id, name, email, phone }
        │
        ▼
CryptoShreddingEventRepository::persist()
  • looks up (or creates) the DEK for subject_id
  • encrypts name/email/phone → encrypted_pii blob
  • records aggregate_id → subject_id in journey_subject_mapping
        │
        ▼
events table  (payload contains ciphertext, never plaintext PII)
        │
        ▼ (read path — crypto layer decrypts before events reach projectors)
        │
        ▼
StructuredJourneyViewRepository::update_view()
  • upserts journey_person row (with subject_id, plaintext fields)
  • uses ON CONFLICT (journey_id) DO UPDATE for re-captures
```

## Business Rules

- The journey **must be started** before `CapturePerson` can be issued.
- The journey **must not be completed** — attempting to capture person data on a completed journey
  returns an error.
- **One person per journey** — enforced by a `UNIQUE (journey_id)` constraint. A second
  `CapturePerson` on the same journey performs an upsert (updates name/email/phone/subject_id).

## After General Data Protection Regulation (GDPR) Shredding

When `DELETE /subjects/{subject_id}` is called:

1. The subject's DEK is hard-deleted from `subject_encryption_keys`.
2. A `ForgetSubject { subject_id }` command is issued for each affected journey aggregate,
   emitting a `SubjectForgotten { subject_id }` event as an audit trail.
3. All `journey_person` rows for that `subject_id` are deleted.
4. `journey_view.accumulated_data` is cleared to `{}` for all affected journeys.
5. Subsequent reads of `PersonCaptured` events in the store return `"[redacted]"` for
   name/email/phone (the ciphertext is present but the key is gone).

The shredding operation is **irreversible**.

## Complete Flow Example

```rust
use journey_dynamics::domain::commands::JourneyCommand;
use uuid::Uuid;

// 1. Start the journey
let journey_id = Uuid::new_v4();
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Start { id: journey_id },
).await?;

// 2. Capture person — supply a stable subject_id from your identity system
let subject_id = Uuid::new_v4();
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        subject_id,
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;

// 3. Capture step data (will be encrypted because a subject is now associated)
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Capture {
        step: "preferences".to_string(),
        data: serde_json::json!({"newsletter": true}),
    },
).await?;

// 4. Complete the journey
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Complete,
).await?;

// 5. Query person data from the read model
let repo = StructuredJourneyViewRepository::new(pool.clone());
let person = repo.load_person(&journey_id).await?.unwrap();
println!("Journey completed for: {} ({})", person.name, person.email);
```
