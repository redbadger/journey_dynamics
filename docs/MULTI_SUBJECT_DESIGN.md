# Multi-Subject Crypto-Shredding Design

## Table of Contents

- [Motivation](#motivation)
- [Core Invariant](#core-invariant)
- [Design Overview](#design-overview)
- [Domain Model Changes](#domain-model-changes)
  - [Aggregate](#aggregate)
  - [Commands](#commands)
  - [Events](#events)
  - [Aggregate Behaviour](#aggregate-behaviour)
- [Crypto Layer Changes](#crypto-layer-changes)
  - [What Gets Encrypted](#what-gets-encrypted)
  - [Write Path](#write-path)
  - [Read Path](#read-path)
  - [Removal of Subject Mapping](#removal-of-subject-mapping)
- [Read-Model Changes](#read-model-changes)
  - [Database Schema](#database-schema)
  - [View Projection](#view-projection)
- [Shredding Flow](#shredding-flow)
  - [API Endpoint](#api-endpoint)
  - [Sequence](#sequence)
  - [What Survives Shredding](#what-survives-shredding)
- [Flight-Booking Example](#flight-booking-example)
- [Implementation Plan](#implementation-plan)
  - [Phase 1: Clean Slate — Database and Migrations](#phase-1-clean-slate--database-and-migrations)
  - [Phase 2: Domain Model](#phase-2-domain-model)
  - [Phase 3: Crypto Layer](#phase-3-crypto-layer)
  - [Phase 4: Read Model](#phase-4-read-model)
  - [Phase 5: HTTP Layer](#phase-5-http-layer)
  - [Phase 6: Flight-Booking Example Updates](#phase-6-flight-booking-example-updates)
- [Testing Strategy](#testing-strategy)

---

## Motivation

The current system supports a single data subject per journey. When `CapturePerson` is called,
the journey is bound to one `subject_id`, and all subsequent `Modified` events have their
entire `data` field encrypted under that subject's Data Encryption Key (DEK). When the subject
exercises their GDPR right to erasure, the DEK is deleted and **all** captured data —
including non-PII fields like flight numbers, prices, and booking references — becomes
permanently unreadable.

This is overly destructive and does not support real-world scenarios where a journey involves
multiple data subjects. The flight-booking example is illustrative: a single booking journey
may capture personal details for several passengers. A GDPR erasure request for one passenger
should remove only that passenger's PII, not the entire booking's data.

### Goals

1. Support **multiple independent data subjects** per journey (e.g. several passengers).
2. On erasure, shred **only the target subject's PII** — preserve shared journey data (flight
   selections, pricing, booking reference) and other subjects' data.
3. **Simplify** the crypto layer by removing the journey-to-subject mapping and making
   encryption purely event-driven (each PII event carries its own `subject_id`).

### Non-Goals

- Hierarchical person relationships (e.g. "lead booker owns the journey"). All persons in a
  journey are peers; no cascade semantics.
- Backward compatibility with existing event stores. This is a clean-slate redesign.

---

## Core Invariant

> **Data that flows through `Capture` / `Modified` events MUST NOT contain personally
> identifiable information. All PII MUST flow through person-specific commands and events
> that carry a `subject_id`.**

This is the single rule that makes selective shredding possible. Data in `Modified` events
is never encrypted and always survives shredding. Data in person events is always encrypted
under the subject's DEK and becomes unreadable when that DEK is deleted.

The corollary: data that cannot be linked back to an identifiable natural person on its own
(airport codes, prices, flight numbers, seat classes, booking references) is safe to store in
the clear. The *link* to identity is what constitutes PII under GDPR, and that link is
severed when the person's slot is shredded.

---

## Design Overview

```
                        ┌─────────────────────────────────────────┐
                        │            Domain Layer                 │
                        │       (plaintext commands/events)       │
                        │                                         │
                        │  Journey aggregate                      │
                        │    ├── shared_data: Value   (non-PII)   │
                        │    └── persons: Map<String, Person>     │
                        │          ├── person_ref: "passenger_0"  │
                        │          │   subject_id: UUID           │
                        │          │   pii_data: Value (encrypted)│
                        │          └── person_ref: "passenger_1"  │
                        │              subject_id: UUID           │
                        │              pii_data: Value (encrypted)│
                        └──────────────────┬──────────────────────┘
                                           │
                        ┌──────────────────┴──────────────────────┐
                        │   CryptoShreddingEventRepository        │
                        │                                         │
                        │   persist():                            │
                        │     PersonCaptured → encrypt pii fields │
                        │     PersonDetailsUpdated → encrypt data │
                        │     Modified → pass through (never PII) │
                        │     all others → pass through           │
                        │                                         │
                        │   get_events():                         │
                        │     encrypted fields → decrypt or redact│
                        │     no encrypted fields → pass through  │
                        └──────────────────┬──────────────────────┘
                                           │
                        ┌──────────────────┴──────────────────────┐
                        │       PostgresEventRepository           │
                        │       (stores ciphertext in payload)    │
                        └─────────────────────────────────────────┘
```

Key differences from the current design:

- **No `subject_mapping` table or trait.** Every PII event carries its own `subject_id` in
  plaintext, so the crypto layer can look up the DEK directly.
- **`Modified` events are never encrypted.** They carry only shared, non-PII data.
- **Multiple `PersonCaptured` and `PersonDetailsUpdated` events** can exist in a single
  journey's event stream, each with a different `subject_id`.
- **Shredding one subject** leaves all other subjects and shared data intact.

---

## Domain Model Changes

### Aggregate

```rust
// crates/journey_dynamics/src/domain/journey.rs

pub struct Journey {
    id: Uuid,
    state: JourneyState,
    shared_data: Value,                          // non-PII, accumulated from Capture commands
    persons: BTreeMap<String, PersonSlot>,       // keyed by person_ref
    current_step: Option<String>,
    latest_workflow_decision: Option<WorkflowDecisionState>,
}

pub struct PersonSlot {
    pub subject_id: Uuid,
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub details: Value,              // free-form PII (passport, DoB, nationality, ...)
    pub forgotten: bool,             // set to true by SubjectForgotten
}
```

**`person_ref: String`** is a client-assigned, journey-local slot name. Examples:
`"lead_booker"`, `"passenger_0"`, `"passenger_1"`. It is stable for the lifetime of the
journey and has no meaning outside the journey. It is not PII.

**`subject_id: Uuid`** is the cross-journey identity for the data subject. The same person
across multiple journeys uses the same `subject_id`. This is the key used to look up the
DEK in the key store.

**`shared_data: Value`** replaces the old `accumulated_data`. It is fed exclusively by
`Modified` events and is never encrypted. After shredding, this field is fully intact.

**`persons`** is a map of person slots. Each slot holds identity fields (`name`, `email`,
`phone`) and a free-form `details: Value` for additional PII. When a subject is forgotten,
their slot's `forgotten` flag is set; on the read path, all their encrypted event payloads
become unreadable.

The old `subject_id: Option<Uuid>` field on the aggregate is removed.

### Commands

```rust
// crates/journey_dynamics/src/domain/commands.rs

pub enum JourneyCommand {
    /// Create a new journey.
    Start {
        id: Uuid,
    },

    /// Capture non-PII shared data for a step.
    /// The `data` field MUST NOT contain PII.
    Capture {
        step: String,
        data: Value,
    },

    /// Register or update a person's identity fields in a named slot.
    /// Creates the slot if it does not exist.
    /// Errors if the slot exists with a different `subject_id`.
    CapturePerson {
        person_ref: String,
        subject_id: Uuid,
        name: String,
        email: String,
        phone: Option<String>,
    },

    /// Capture free-form PII details for an existing person slot.
    /// The slot must have been created by a prior `CapturePerson` command.
    /// The `data` is merged into `persons[person_ref].details`.
    CapturePersonDetails {
        person_ref: String,
        data: Value,
    },

    /// Mark the journey as complete.
    Complete,

    /// Emit a SubjectForgotten audit event (called by the shredding handler
    /// after the DEK has been deleted).
    ForgetSubject {
        subject_id: Uuid,
    },
}
```

**Key rules:**

- `CapturePerson` is the only way to create a person slot. It requires both `person_ref` and
  `subject_id`. If a slot already exists with the same `person_ref` but a **different**
  `subject_id`, the command is rejected. If the same `subject_id`, the identity fields are
  updated (idempotent).
- `CapturePersonDetails` requires the slot to already exist (i.e. `CapturePerson` must have
  been called first for that `person_ref`). This ensures there is always a `subject_id`
  associated with the details, which the crypto layer needs for encryption.
- `Capture` carries only shared data. The core invariant is enforced here by convention and
  optionally by schema validation (applications can register schemas that classify fields).

### Events

```rust
// crates/journey_dynamics/src/domain/events.rs

pub enum JourneyEvent {
    /// Journey created.
    Started {
        id: Uuid,
    },

    /// Non-PII shared data captured for a step.
    Modified {
        step: String,
        data: Value,
    },

    /// A person's identity fields were captured or updated.
    PersonCaptured {
        person_ref: String,
        subject_id: Uuid,
        name: String,
        email: String,
        phone: Option<String>,
    },

    /// Free-form PII details were captured for a person.
    PersonDetailsUpdated {
        person_ref: String,
        subject_id: Uuid,
        data: Value,
    },

    /// Decision engine evaluated next steps.
    WorkflowEvaluated {
        suggested_actions: Vec<String>,
    },

    /// Journey progressed from one step to another.
    StepProgressed {
        from_step: Option<String>,
        to_step: String,
    },

    /// Journey completed.
    Completed,

    /// Audit event: a subject's DEK was deleted (crypto-shredded).
    SubjectForgotten {
        subject_id: Uuid,
    },
}
```

**New event: `PersonDetailsUpdated`**. This carries the `person_ref` and `subject_id` in
plaintext (never encrypted) and a `data: Value` field that is encrypted under the subject's
DEK by the crypto layer. The `subject_id` is present so the crypto layer can look up the
DEK without consulting any external mapping.

**`Modified` is never encrypted.** Its `data` field is stored as-is.

#### Event type strings

For the `DomainEvent::event_type()` implementation:

| Variant                | `event_type` string       |
|------------------------|---------------------------|
| `Started`              | `"JourneyOpened"`         |
| `Modified`             | `"JourneyModified"`       |
| `PersonCaptured`       | `"PersonCaptured"`        |
| `PersonDetailsUpdated` | `"PersonDetailsUpdated"`  |
| `WorkflowEvaluated`    | `"WorkflowEvaluated"`     |
| `StepProgressed`       | `"StepProgressed"`        |
| `Completed`            | `"JourneyClosed"`         |
| `SubjectForgotten`     | `"SubjectForgotten"`      |

### Aggregate Behaviour

#### `handle` — command processing

**`Start { id }`** — unchanged from today.

**`Capture { step, data }`** — unchanged from today except:
- Validates the journey is started and not complete.
- Runs schema validation.
- Evaluates the decision engine.
- Emits `Modified`, `WorkflowEvaluated`, and optionally `StepProgressed`.
- Does **not** check for a subject mapping (that concept no longer exists).

**`CapturePerson { person_ref, subject_id, name, email, phone }`**:
- Rejects if `self.id == Uuid::default()` → `NotFound`.
- Rejects if `self.state == Complete` → `AlreadyCompleted`.
- If `self.persons` contains `person_ref` with a different `subject_id` →
  `PersonRefConflict` error.
- Otherwise emits `PersonCaptured { person_ref, subject_id, name, email, phone }`.

**`CapturePersonDetails { person_ref, data }`**:
- Rejects if `self.id == Uuid::default()` → `NotFound`.
- Rejects if `self.state == Complete` → `AlreadyCompleted`.
- If `self.persons` does not contain `person_ref` → `PersonNotFound` error.
- Otherwise reads `subject_id` from `self.persons[person_ref].subject_id` and emits
  `PersonDetailsUpdated { person_ref, subject_id, data }`.

**`Complete`** — unchanged.

**`ForgetSubject { subject_id }`** — unchanged. Emits `SubjectForgotten { subject_id }`.

#### `apply` — event projection onto aggregate state

```rust
fn apply(&mut self, event: Self::Event) {
    match event {
        JourneyEvent::Started { id } => {
            self.id = id;
            self.state = JourneyState::InProgress;
        }
        JourneyEvent::Modified { data, .. } => {
            json_patch::merge(&mut self.shared_data, &data);
        }
        JourneyEvent::PersonCaptured {
            person_ref,
            subject_id,
            name,
            email,
            phone,
        } => {
            let slot = self.persons.entry(person_ref).or_insert_with(|| PersonSlot {
                subject_id,
                name: None,
                email: None,
                phone: None,
                details: json!({}),
                forgotten: false,
            });
            slot.name = Some(name);
            slot.email = Some(email);
            slot.phone = phone;
        }
        JourneyEvent::PersonDetailsUpdated {
            person_ref, data, ..
        } => {
            if let Some(slot) = self.persons.get_mut(&person_ref) {
                json_patch::merge(&mut slot.details, &data);
            }
        }
        JourneyEvent::WorkflowEvaluated { suggested_actions } => {
            self.latest_workflow_decision =
                Some(WorkflowDecisionState { suggested_actions });
        }
        JourneyEvent::StepProgressed { to_step, .. } => {
            self.current_step = Some(to_step);
        }
        JourneyEvent::Completed => {
            self.state = JourneyState::Complete;
        }
        JourneyEvent::SubjectForgotten { subject_id } => {
            for slot in self.persons.values_mut() {
                if slot.subject_id == subject_id {
                    slot.forgotten = true;
                }
            }
        }
    }
}
```

#### New error variants

```rust
pub enum JourneyError {
    #[error("Journey not found")]
    NotFound,
    #[error("Journey already opened")]
    AlreadyStarted,
    #[error("Journey already closed")]
    AlreadyCompleted,
    #[error("Decision engine error: {0}")]
    DecisionEngineError(String),
    #[error("Invalid data: {0}")]
    InvalidData(String),
    #[error("Person slot '{0}' is already bound to a different subject")]
    PersonRefConflict(String),
    #[error("Person slot '{0}' does not exist — call CapturePerson first")]
    PersonNotFound(String),
}
```

---

## Crypto Layer Changes

### What Gets Encrypted

| Event type             | Encrypted fields                              | Plaintext fields                    |
|------------------------|-----------------------------------------------|-------------------------------------|
| `PersonCaptured`       | `name`, `email`, `phone` → single blob        | `person_ref`, `subject_id`          |
| `PersonDetailsUpdated` | `data` → single blob                          | `person_ref`, `subject_id`          |
| `Modified`             | *(nothing — never encrypted)*                 | `step`, `data`                      |
| All other events       | *(nothing)*                                   | All fields                          |

### Write Path

`CryptoShreddingEventRepository::encrypt_events` processes each `SerializedEvent`:

1. **`PersonCaptured`**: Read `subject_id` from the payload (plaintext). Call
   `key_store.get_or_create_key(subject_id)` to obtain the DEK. Serialize `name`, `email`,
   `phone` into a JSON blob, encrypt with AES-256-GCM using AAD = `"<aggregate_id>:<sequence>"`.
   Replace those fields with `encrypted_pii` (base64) and `nonce` (base64). Keep `person_ref`
   and `subject_id` in plaintext.

2. **`PersonDetailsUpdated`**: Read `subject_id` from the payload (plaintext). Obtain the DEK.
   Encrypt the `data` field. Replace it with `encrypted_data` and `nonce`. Keep `person_ref`
   and `subject_id` in plaintext.

3. **`JourneyModified` and all other events**: Pass through unmodified.

#### What events look like in the event store

**`PersonCaptured` (encrypted):**

```json
{
  "PersonCaptured": {
    "person_ref": "passenger_0",
    "subject_id": "aaaaaaaa-...",
    "encrypted_pii": "<base64>",
    "nonce": "<base64>"
  }
}
```

**`PersonDetailsUpdated` (encrypted):**

```json
{
  "PersonDetailsUpdated": {
    "person_ref": "passenger_0",
    "subject_id": "aaaaaaaa-...",
    "encrypted_data": "<base64>",
    "nonce": "<base64>"
  }
}
```

**`Modified` (plaintext — never encrypted):**

```json
{
  "Modified": {
    "step": "search",
    "data": {
      "search": {
        "tripType": "round-trip",
        "origin": "LHR",
        "destination": "JFK",
        "departureDate": "2025-08-15"
      }
    }
  }
}
```

### Read Path

`CryptoShreddingEventRepository::decrypt_events` processes each `SerializedEvent`:

1. **`PersonCaptured` with `encrypted_pii` sentinel**: Read `subject_id`, look up the DEK.
   - DEK found → decrypt, reconstruct the original plaintext payload.
   - DEK not found (key deleted) → redact: set `name` to `"[redacted]"`, `email` to
     `"[redacted]"`, `phone` to `null`. Keep `person_ref` and `subject_id`.

2. **`PersonDetailsUpdated` with `encrypted_data` sentinel**: Read `subject_id`, look up DEK.
   - DEK found → decrypt, reconstruct the original `data` field.
   - DEK not found → set `data` to `{}`.

3. **`PersonCaptured` / `PersonDetailsUpdated` without encrypted sentinels**: Legacy or
   test data — pass through unmodified.

4. **All other events**: Pass through unmodified.

### Removal of Subject Mapping

The `SubjectMapping` trait, `InMemorySubjectMapping`, and `PostgresSubjectMapping` are
**deleted entirely**, along with the `journey_subject_mapping` database table.

In the current design, the subject mapping exists because `Modified` events don't carry a
`subject_id` — the crypto layer must look up "does this journey have a subject?" to decide
whether to encrypt. In the new design, `Modified` events are never encrypted, and the two
PII event types (`PersonCaptured`, `PersonDetailsUpdated`) both carry `subject_id` in their
payload. The crypto layer is now stateless with respect to journey-subject relationships.

This also means:

- `CryptoShreddingEventRepository` no longer holds an `Arc<dyn SubjectMapping>`.
- `ApplicationState` no longer holds a `subject_mapping` field.
- The constructor for `CryptoShreddingEventRepository::new` takes only `inner`, `key_store`,
  and `cipher`.

#### Finding journeys for a subject during shredding

The shredding endpoint (`DELETE /subjects/{subject_id}`) still needs to know which journeys
reference a subject so it can emit `SubjectForgotten` audit events. Without the mapping
table, this is done by querying the event store directly:

```sql
SELECT DISTINCT aggregate_id
FROM events
WHERE aggregate_type = 'Journey'
  AND event_type IN ('PersonCaptured', 'PersonDetailsUpdated')
  AND payload -> 'PersonCaptured' ->> 'subject_id' = $1::text
   OR payload -> 'PersonDetailsUpdated' ->> 'subject_id' = $1::text;
```

This query scans `subject_id` fields that are always stored in plaintext (never encrypted).
An index on the JSON path can be added if performance matters:

```sql
CREATE INDEX idx_events_person_captured_subject
    ON events ((payload -> 'PersonCaptured' ->> 'subject_id'))
    WHERE event_type = 'PersonCaptured';

CREATE INDEX idx_events_person_details_subject
    ON events ((payload -> 'PersonDetailsUpdated' ->> 'subject_id'))
    WHERE event_type = 'PersonDetailsUpdated';
```

Alternatively, a lightweight helper trait (e.g. `SubjectJourneyIndex`) can encapsulate this
query so the route handler doesn't embed raw SQL.

---

## Read-Model Changes

### Database Schema

All existing migrations are deleted and replaced with a single clean migration.

```sql
-- events table (unchanged structure from cqrs-es)
CREATE TABLE events
(
    aggregate_type text                         NOT NULL,
    aggregate_id   text                         NOT NULL,
    sequence       bigint CHECK (sequence >= 0) NOT NULL,
    event_type     text                         NOT NULL,
    event_version  text                         NOT NULL,
    payload        json                         NOT NULL,
    metadata       json                         NOT NULL,
    timestamp      timestamp with time zone DEFAULT (CURRENT_TIMESTAMP),
    PRIMARY KEY (aggregate_type, aggregate_id, sequence)
);

-- Journey read model (shared, non-PII data)
CREATE TABLE journey_view
(
    id                  UUID        NOT NULL PRIMARY KEY,
    state               TEXT        NOT NULL CHECK (state IN ('InProgress', 'Complete')),
    shared_data         JSONB       NOT NULL DEFAULT '{}',
    current_step        TEXT,
    version             BIGINT CHECK (version >= 0) NOT NULL DEFAULT 0,
    created_at          TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Workflow decisions (unchanged)
CREATE TABLE journey_workflow_decision
(
    id                  SERIAL      NOT NULL PRIMARY KEY,
    journey_id          UUID        NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    suggested_actions   TEXT[]      NOT NULL,
    created_at          TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_latest           BOOLEAN     NOT NULL DEFAULT TRUE
);

-- Per-person data (one row per person_ref per journey)
CREATE TABLE journey_person
(
    journey_id          UUID        NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    person_ref          TEXT        NOT NULL,
    subject_id          UUID        NOT NULL,
    name                TEXT,
    email               TEXT,
    phone               TEXT,
    details             JSONB       NOT NULL DEFAULT '{}',
    forgotten           BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (journey_id, person_ref)
);

-- Per-subject DEKs (unchanged structure)
CREATE TABLE subject_encryption_keys
(
    key_id      UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id  UUID      NOT NULL UNIQUE,
    wrapped_key BYTEA     NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Indexes
CREATE INDEX idx_journey_shared_data ON journey_view USING GIN (shared_data);
CREATE INDEX idx_journey_workflow_decision_journey_id
    ON journey_workflow_decision(journey_id);
CREATE INDEX idx_journey_workflow_decision_latest
    ON journey_workflow_decision(journey_id, is_latest) WHERE is_latest = TRUE;
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
CREATE INDEX idx_subject_keys_subject_id ON subject_encryption_keys(subject_id);

-- Event store indexes for subject lookup during shredding
CREATE INDEX idx_events_person_captured_subject
    ON events ((payload -> 'PersonCaptured' ->> 'subject_id'))
    WHERE event_type = 'PersonCaptured';
CREATE INDEX idx_events_person_details_subject
    ON events ((payload -> 'PersonDetailsUpdated' ->> 'subject_id'))
    WHERE event_type = 'PersonDetailsUpdated';
```

**Key changes from the old schema:**

- `journey_view.accumulated_data` is renamed to `journey_view.shared_data`.
- `journey_person` is redesigned: composite PK `(journey_id, person_ref)`, holds `subject_id`,
  `name`, `email`, `phone`, `details` (JSONB), and a `forgotten` flag.
- `journey_subject_mapping` table is deleted entirely.

### View Projection

The `JourneyView` struct and its `View<Journey>` implementation change:

```rust
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct JourneyView {
    pub id: Uuid,
    pub state: JourneyState,
    pub shared_data: Value,
    pub current_step: Option<String>,
    pub latest_workflow_decision: Option<WorkflowDecisionView>,
}
```

The `View<Journey>::update` implementation:

- `Started` → initialize `id`, `state`, `shared_data = {}`.
- `Modified` → `json_patch::merge(&mut self.shared_data, &data)`.
- `PersonCaptured` → no-op on `JourneyView` (person data is projected to `journey_person`).
- `PersonDetailsUpdated` → no-op on `JourneyView`.
- `WorkflowEvaluated` → update `latest_workflow_decision`.
- `StepProgressed` → update `current_step`.
- `Completed` → set `state = Complete`.
- `SubjectForgotten` → no-op on `JourneyView`.

The `StructuredJourneyViewRepository` handles person data in its `update_view` method:

- `PersonCaptured { person_ref, subject_id, name, email, phone }` → upsert into
  `journey_person` keyed by `(journey_id, person_ref)`.
- `PersonDetailsUpdated { person_ref, data, .. }` → merge `data` into the existing
  `journey_person.details` JSONB column for `(journey_id, person_ref)`.
- `SubjectForgotten { subject_id }` → for all `journey_person` rows with this `subject_id`
  in this journey: set `name = NULL`, `email = NULL`, `phone = NULL`,
  `details = '{}'`, `forgotten = TRUE`.

---

## Shredding Flow

### API Endpoint

```
DELETE /subjects/{subject_id}
```

Returns `204 No Content`.

### Sequence

1. **Find affected journeys.** Query the event store for distinct `aggregate_id`s that
   contain `PersonCaptured` or `PersonDetailsUpdated` events with the target `subject_id`.

2. **Delete the DEK.** Call `key_store.delete_key(&subject_id)`. From this moment, all
   encrypted payloads for this subject are permanently unreadable.

3. **Emit audit events.** For each affected journey, execute
   `ForgetSubject { subject_id }` through the CQRS framework. This:
   - Emits a `SubjectForgotten` event in each journey's event stream.
   - Triggers the view repository to null out person data in `journey_person`
     and set `forgotten = TRUE`.

### What Survives Shredding

| Data | Survives? | Why |
|------|-----------|-----|
| `journey_view.shared_data` | ✅ Yes | Never encrypted; contains no PII |
| `journey_view.state` | ✅ Yes | Journey lifecycle metadata |
| `journey_view.current_step` | ✅ Yes | Workflow metadata |
| `journey_workflow_decision` | ✅ Yes | Decision engine output |
| `events` — `Started`, `Modified`, `WorkflowEvaluated`, `StepProgressed`, `Completed` | ✅ Yes | Never encrypted |
| `events` — `SubjectForgotten` | ✅ Yes | Audit trail |
| Other subjects' `journey_person` rows | ✅ Yes | Different DEK |
| Other subjects' `PersonCaptured` / `PersonDetailsUpdated` events | ✅ Yes | Different DEK |
| Shredded subject's `journey_person` row | ❌ Nulled | `forgotten = TRUE`, fields cleared |
| Shredded subject's `PersonCaptured` events | ❌ Redacted | DEK gone → `[redacted]` |
| Shredded subject's `PersonDetailsUpdated` events | ❌ Redacted | DEK gone → `data: {}` |

---

## Flight-Booking Example

The flight-booking schema maps onto this model as follows:

### Shared data (via `Capture` command, stored in `Modified` events)

| Schema field | Step | Example value |
|---|---|---|
| `search.tripType` | `search` | `"round-trip"` |
| `search.origin` | `search` | `"LHR"` |
| `search.destination` | `search` | `"JFK"` |
| `search.departureDate` | `search` | `"2025-08-15"` |
| `search.returnDate` | `search` | `"2025-08-22"` |
| `search.passengers` | `search` | `{ "total": 2, "adults": 2, ... }` |
| `searchResults` | `search_results` | `{ "outbound": [...], ... }` |
| `booking.selectedOutboundFlight` | `flight_selection` | `{ "flightId": "BA117", ... }` |
| `booking.selectedReturnFlight` | `flight_selection` | `{ "flightId": "BA178", ... }` |
| `booking.pricing` | `pricing` | `{ "basePrice": 450, ... }` |
| `booking.insurance` | `insurance` | `{ "selected": true, ... }` |
| `booking.payment.status` | `payment` | `"completed"` |
| `booking.payment.method` | `payment` | `"credit_card"` |
| `booking.bookingReference` | `confirmation` | `"PNR123"` |
| `booking.termsAccepted` | `confirmation` | `true` |

### Per-person identity (via `CapturePerson` command)

```bash
# Lead booker (passenger 0)
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "person_ref": "passenger_0",
      "subject_id": "aaaa-...",
      "name": "Alice Smith",
      "email": "alice@example.com",
      "phone": "+44-7700-900000"
    }
  }'

# Second passenger
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "person_ref": "passenger_1",
      "subject_id": "bbbb-...",
      "name": "Bob Jones",
      "email": "bob@example.com",
      "phone": null
    }
  }'
```

### Per-person details (via `CapturePersonDetails` command)

```bash
# Passenger 0 passport details
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePersonDetails": {
      "person_ref": "passenger_0",
      "data": {
        "firstName": "Alice",
        "lastName": "Smith",
        "dateOfBirth": "1990-05-15",
        "passportNumber": "GB123456789",
        "nationality": "GB",
        "passengerType": "adult"
      }
    }
  }'

# Passenger 1 passport details
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePersonDetails": {
      "person_ref": "passenger_1",
      "data": {
        "firstName": "Bob",
        "lastName": "Jones",
        "dateOfBirth": "1988-11-20",
        "passportNumber": "GB987654321",
        "nationality": "GB",
        "passengerType": "adult"
      }
    }
  }'
```

### Shredding passenger 1

```bash
curl -i -X DELETE http://localhost:3030/subjects/bbbb-...
```

**Result:**
- Flight details, pricing, booking reference → intact.
- Alice's name, email, phone, passport → intact (different DEK).
- Bob's name, email, phone, passport → permanently gone.
- `journey_person` row for `passenger_1` → `forgotten = TRUE`, all fields nulled.
- `SubjectForgotten { subject_id: "bbbb-..." }` event appended to the journey stream.

---

## Implementation Plan

This is a clean-slate implementation. All existing migrations are removed and replaced.

### Phase 1: Clean Slate — Database and Migrations

**Files to delete:**
- `migrations/20251218151839_init.up.sql`
- `migrations/20251218151839_init.down.sql`
- `migrations/20260422085557_add_crypto_shredding.up.sql`
- `migrations/20260422085557_add_crypto_shredding.down.sql`

**Files to create:**
- A single new migration with the schema from the [Database Schema](#database-schema) section.

### Phase 2: Domain Model

**Files to modify:**
- `crates/journey_dynamics/src/domain/commands.rs` — new command variants as described in
  [Commands](#commands).
- `crates/journey_dynamics/src/domain/events.rs` — new event variants as described in
  [Events](#events), including the `DomainEvent` impl with the new event type strings.
- `crates/journey_dynamics/src/domain/journey.rs` — new aggregate struct (`shared_data`,
  `persons: BTreeMap`), new `PersonSlot` struct, updated `handle` and `apply`, new error
  variants. Remove old `subject_id: Option<Uuid>` field and accessor. Update `Default` impl.
  Update all tests.

### Phase 3: Crypto Layer

**Files to delete:**
- `crates/journey_dynamics/src/crypto/subject_mapping.rs` — the `SubjectMapping` trait,
  `InMemorySubjectMapping`, and `PostgresSubjectMapping` are no longer needed.

**Files to modify:**
- `crates/journey_dynamics/src/crypto/mod.rs` — remove `pub mod subject_mapping`.
- `crates/journey_dynamics/src/crypto/repository.rs` — major rewrite:
  - Remove `subject_mapping` field from `CryptoShreddingEventRepository`.
  - Remove `subject_mapping` parameter from `new()`.
  - Remove `maybe_encrypt_modified` / `maybe_decrypt_modified` (Modified events are never
    encrypted).
  - Add `encrypt_person_details_updated` and `decrypt_person_details_updated` methods
    mirroring the existing `PersonCaptured` pattern.
  - Update `encrypt_events` to dispatch on the new event type string
    `"PersonDetailsUpdated"`.
  - Update `decrypt_events` similarly.
  - Update `encrypt_person_captured` to include `person_ref` in the plaintext fields.
  - Add constants: `PERSON_DETAILS_UPDATED = "PersonDetailsUpdated"` and
    `PD_KEY = "PersonDetailsUpdated"`.
  - Update all tests.

**Files unchanged:**
- `crates/journey_dynamics/src/crypto/cipher.rs` — AES-256-GCM and AES-256-KWP logic is
  unchanged.
- `crates/journey_dynamics/src/crypto/key_store.rs` — `KeyStore` trait, `InMemoryKeyStore`,
  `PostgresKeyStore` are unchanged.

### Phase 4: Read Model

**Files to modify:**
- `crates/journey_dynamics/src/queries.rs` — rename `accumulated_data` to `shared_data` in
  `JourneyView`. Add no-op arms for `PersonCaptured`, `PersonDetailsUpdated`. Update tests.
- `crates/journey_dynamics/src/view_repository.rs` — rewrite `PersonView` struct, update
  `update_view` to handle `PersonCaptured` (upsert `journey_person`),
  `PersonDetailsUpdated` (merge into `details`), and `SubjectForgotten` (null out person
  rows, set `forgotten = TRUE`). Rename `accumulated_data` column references to
  `shared_data`. Update all SQL queries. Remove the old `SubjectForgotten` handler that
  cleared `accumulated_data`. Update `PersonView` to include `person_ref`, `subject_id`,
  `details`, `forgotten`. Update tests.

### Phase 5: HTTP Layer

**Files to modify:**
- `crates/journey_dynamics/src/route_handler.rs` — update `shred_subject` to query the event
  store for affected journeys instead of using `subject_mapping.get_journeys()`. Remove
  `subject_mapping` usage.
- `crates/journey_dynamics/src/state.rs` — remove `subject_mapping` from `ApplicationState`.
  Update `new_application_state` to stop creating a `PostgresSubjectMapping`. Update the
  `cqrs_framework` call (it no longer takes a `subject_mapping`).
- `crates/journey_dynamics/src/config.rs` — update `cqrs_framework` function signature to
  remove the `subject_mapping` parameter. Update how `CryptoShreddingEventRepository::new`
  is called.
- `crates/journey_dynamics/src/command_extractor.rs` — may need updating if the
  `CapturePersonDetails` command needs special deserialization. In practice, serde's
  default external tagging should handle it without changes.

### Phase 6: Flight-Booking Example Updates

**Files to modify:**
- `examples/flight-booking/src/lib.rs` — the schema structs themselves don't change, but
  we should move `passenger_details` out of `BookingData` into its own type that is clearly
  documented as PII flowing through `CapturePersonDetails`, not `Capture`.
- `examples/flight-booking/src/tests.rs` — update test scenarios to use the new commands.
- `examples/flight-booking/SCHEMA_USAGE.md` — update documentation.

**Files to update:**
- `docs/QUICK_START.md` — update the walkthrough to show multi-subject commands.
- `docs/CRYPTO_SHREDDING_DESIGN.md` — mark as superseded by this document, or update.
- `docs/PERSON_CAPTURE.md` — update with `CapturePersonDetails` reference.
- `README.md` — update API examples.

---

## Testing Strategy

### Unit tests (domain layer)

- `CapturePerson` creates a new person slot; verify `apply` populates `persons`.
- `CapturePerson` with same `person_ref` and same `subject_id` updates identity fields.
- `CapturePerson` with same `person_ref` but different `subject_id` → `PersonRefConflict`.
- `CapturePersonDetails` for existing slot → merges into `details`.
- `CapturePersonDetails` for non-existent slot → `PersonNotFound`.
- `CapturePersonDetails` emits event with correct `subject_id` from the slot.
- `ForgetSubject` sets `forgotten = true` on matching slots only.
- Multiple persons in one journey; verify independent slots.
- `Capture` continues to work as before for shared data.

### Unit tests (crypto layer)

- `PersonCaptured` with `person_ref` is encrypted; `person_ref` and `subject_id` remain
  plaintext.
- `PersonDetailsUpdated` is encrypted; `person_ref` and `subject_id` remain plaintext.
- `Modified` events pass through unmodified (never encrypted).
- Decrypt with valid DEK → original plaintext.
- Decrypt with missing DEK → redacted output.
- Two subjects in one journey; delete one DEK → that subject's events redacted, the other's
  events decryptable, `Modified` events untouched.
- AAD binding: ciphertext cannot be transplanted between events.

### Integration tests

- Full lifecycle: create journey → capture shared data → capture two persons → capture
  details for each → complete → query view → verify `shared_data` and both persons visible.
- Shred one subject → query view → verify `shared_data` intact, shredded person has
  `forgotten = true` and nulled fields, other person intact.
- Shred both subjects → `shared_data` still intact.
- Cross-journey shredding: same `subject_id` in two journeys → shred → both journeys'
  person slots affected, shared data in both untouched.
