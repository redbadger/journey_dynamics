# Capturing Person Data in Journeys

This guide explains how to capture personally identifiable information (PII) for one or more
data subjects within a journey, how `person_ref` ties a slot to a subject across the journey,
and what to expect from the encryption and crypto-shredding layer.

---

## Overview

Two commands capture PII. Neither triggers workflow evaluation — only `Capture` does that.

| Command | Purpose | Event emitted | Encrypted? |
|---|---|---|---|
| `CapturePerson` | Register or update a person's identity fields (name, email, phone) in a named slot | `PersonCaptured` | ✅ name, email, phone |
| `CapturePersonDetails` | Capture free-form PII details (passport, DoB, nationality, …) for an existing slot | `PersonDetailsUpdated` | ✅ entire `data` blob |

Both commands require `person_ref`, a client-assigned slot name that is **not PII** and is
stored in plaintext. `CapturePerson` must be called before `CapturePersonDetails` for the same
`person_ref`.

---

## The `person_ref` Field

`person_ref` is a journey-local string that names a slot within the journey. Examples:
`"lead_booker"`, `"passenger_0"`, `"passenger_1"`.

| Property | Details |
|---|---|
| **Who supplies it** | The caller (your application layer or API client) |
| **Scope** | Local to a single journey — the same string in two different journeys refers to two unrelated slots |
| **Format** | Any non-empty string; use a consistent convention (e.g. `"passenger_N"`) |
| **PII?** | No — stored in plaintext in the event store |

---

## The `subject_id` Field

`subject_id` is supplied on `CapturePerson` and identifies the **data subject** — the real
person — independently of any individual journey or slot.

| Property | Details |
|---|---|
| **Who supplies it** | The caller (from your identity system) |
| **When to create it** | Once per person, at account creation or first contact; reuse it for every subsequent journey |
| **Why it matters** | One `subject_id` may span many journeys and many slots. A single `DELETE /subjects/{subject_id}` call shreds that person's PII across **all** of their journeys simultaneously |
| **Format** | Standard UUID v4 |
| **PII?** | No — stored in plaintext; used as the DEK lookup key |

---

## Rust Usage

```rust
use journey_dynamics::domain::commands::JourneyCommand;
use serde_json::json;
use uuid::Uuid;

let subject_id = Uuid::new_v4(); // generate once per person; reuse across journeys

// 1. Register the person's identity in a named slot.
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        person_ref: "lead_booker".to_string(),
        subject_id,
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;

// 2. Capture additional PII details for the same slot.
//    CapturePerson must be called first for the same person_ref.
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePersonDetails {
        person_ref: "lead_booker".to_string(),
        data: json!({
            "dateOfBirth":    "1990-05-15",
            "passportNumber": "GB123456789",
            "nationality":    "GB"
        }),
    },
).await?;

// 3. Add a second passenger with a different subject_id.
let subject_id_2 = Uuid::new_v4();
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        person_ref: "passenger_1".to_string(),
        subject_id: subject_id_2,
        name: "Bob Smith".to_string(),
        email: "bob.smith@example.com".to_string(),
        phone: None,
    },
).await?;
```

---

## HTTP API

### `CapturePerson`

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "person_ref": "lead_booker",
      "subject_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      "name": "Alice Johnson",
      "email": "alice.johnson@example.com",
      "phone": "+1-555-0123"
    }
  }'
```

`person_ref` must be unique within the journey for each data subject. Calling `CapturePerson`
again with the same `person_ref` and the same `subject_id` updates the identity fields
(idempotent). Calling it with the same `person_ref` but a **different** `subject_id` returns
an error (`PersonRefConflict`).

### `CapturePersonDetails`

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePersonDetails": {
      "person_ref": "lead_booker",
      "data": {
        "dateOfBirth":    "1990-05-15",
        "passportNumber": "GB123456789",
        "nationality":    "GB"
      }
    }
  }'
```

`CapturePersonDetails` does not include `subject_id` — it is looked up automatically from the
slot created by the prior `CapturePerson` call. If `person_ref` does not exist,
`PersonNotFound` is returned.

Multiple `CapturePersonDetails` calls for the same `person_ref` are allowed; the `data` is
**merged** (JSON merge-patch) into the slot's existing details on each call.

---

## PII Encryption at Rest

### `PersonCaptured` events

`name`, `email`, and `phone` are serialised into a single JSON blob and encrypted with
AES-256-GCM before being written to the event store. The `person_ref` and `subject_id` remain
in plaintext so the read path can locate the correct DEK without decrypting anything first.

Stored event payload:

```json
{
  "PersonCaptured": {
    "person_ref":    "lead_booker",
    "subject_id":    "a1b2c3d4-...",
    "encrypted_pii": "<base64-ciphertext>",
    "nonce":         "<base64-nonce>"
  }
}
```

### `PersonDetailsUpdated` events

The entire `data` field is encrypted under the same subject's DEK. `person_ref` and
`subject_id` remain in plaintext.

Stored event payload:

```json
{
  "PersonDetailsUpdated": {
    "person_ref":     "lead_booker",
    "subject_id":     "a1b2c3d4-...",
    "encrypted_data": "<base64-ciphertext>",
    "nonce":          "<base64-nonce>"
  }
}
```

### `Modified` events

`Modified` events carry only shared, non-PII journey data and are **never** encrypted.

---

## Database Schema

### `journey_person`

```sql
CREATE TABLE journey_person
(
    journey_id  UUID      NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    person_ref  TEXT      NOT NULL,
    subject_id  UUID      NOT NULL,
    name        TEXT,                         -- nulled on SubjectForgotten
    email       TEXT,                         -- nulled on SubjectForgotten
    phone       TEXT,                         -- nulled on SubjectForgotten
    details     JSONB     NOT NULL DEFAULT '{}', -- cleared on SubjectForgotten
    forgotten   BOOLEAN   NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (journey_id, person_ref)
);

CREATE INDEX idx_journey_person_subject_id ON journey_person (subject_id);
```

Multiple rows per journey — one per `person_ref`. Shredding one subject nulls only that
subject's row; all other rows and the journey's shared data are untouched.

---

## Querying Person Data

All query methods live on `StructuredJourneyViewRepository`.

### `load_persons` — all person slots for a journey

```rust
let repo = StructuredJourneyViewRepository::new(pool);

let persons = repo.load_persons(&journey_id).await?;
for person in &persons {
    if person.forgotten {
        println!("{}: [forgotten]", person.person_ref);
    } else {
        println!(
            "{}: {} <{}>",
            person.person_ref,
            person.name.as_deref().unwrap_or(""),
            person.email.as_deref().unwrap_or(""),
        );
    }
}
```

### `find_by_email` — look up journeys by email (non-forgotten only)

```rust
let journeys = repo.find_by_email("alice.johnson@example.com").await?;

for journey in journeys {
    println!("Journey {}: {:?}", journey.id, journey.state);
}
```

---

## Business Rules

- The journey **must be started** before either command can be issued.
- The journey **must not be completed**.
- `CapturePersonDetails` requires a prior `CapturePerson` for the same `person_ref` —
  otherwise `PersonNotFound` is returned.
- `CapturePerson` with an existing `person_ref` and the **same** `subject_id` is an upsert
  (updates identity fields).
- `CapturePerson` with an existing `person_ref` but a **different** `subject_id` returns
  `PersonRefConflict` — a slot cannot be reassigned to a different subject.
- There is no hard limit on the number of persons per journey.

---

## After GDPR Shredding

When `DELETE /subjects/{subject_id}` is called:

1. The subject's DEK is hard-deleted from `subject_encryption_keys`.
2. `PersonCaptured` and `PersonDetailsUpdated` events for that subject become permanently
   unreadable (ciphertext remains; key is gone).
3. A `ForgetSubject { subject_id }` command is issued for each affected journey, emitting a
   `SubjectForgotten { subject_id }` audit event.
4. All `journey_person` rows for that `subject_id` are nulled (`name`, `email`, `phone`,
   `details` cleared; `forgotten = TRUE`).
5. **`journey_view.shared_data` is not modified.** Other persons' rows are not modified.

The shredding operation is **irreversible**.

---

## Event Flow

```
CapturePerson { person_ref, subject_id, name, email, phone }
        │
        ▼
Journey::handle()  ──validates──▶  started, not completed, no person_ref conflict
        │
        ▼
PersonCaptured { person_ref, subject_id, name, email, phone }
        │
        ▼
CryptoShreddingEventRepository::persist()
  • gets or creates the DEK for subject_id
  • encrypts name/email/phone → encrypted_pii blob
  • person_ref and subject_id stored in plaintext
        │
        ▼
events table (payload: person_ref + subject_id in plaintext, PII ciphertext)
        │
        ▼ (read path — crypto layer decrypts before events reach projectors)
        │
        ▼
StructuredJourneyViewRepository::update_view()
  • upserts journey_person row on (journey_id, person_ref)


CapturePersonDetails { person_ref, data }
        │
        ▼
Journey::handle()  ──looks up──▶  subject_id from persons[person_ref]
        │
        ▼
PersonDetailsUpdated { person_ref, subject_id, data }
        │
        ▼
CryptoShreddingEventRepository::persist()
  • gets or creates the DEK for subject_id
  • encrypts data → encrypted_data blob
        │
        ▼
events table
        │
        ▼
StructuredJourneyViewRepository::update_view()
  • merges data into journey_person.details (JSONB merge)
```
