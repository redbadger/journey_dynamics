# Journey Dynamics

A backend service that orchestrates adaptive, forms-based user journeys using event sourcing and Command Query Responsibility Segregation (CQRS). Journey routes are determined dynamically by a GoRules decision engine, and all personally identifiable information (PII) is protected by General Data Protection Regulation (GDPR) crypto-shredding.

---

## Setup

### Prerequisites

- Rust (stable)
- Docker (for PostgreSQL)
- `sqlx-cli`

```bash
cargo install sqlx-cli
```

### Database

```bash
# Start Postgres
docker-compose up -d

# Create the database and run migrations
cargo sqlx database create
cargo sqlx migrate run
```

### Environment

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/journey_dynamics

# 256-bit Key Encryption Key for GDPR crypto-shredding (required)
export JOURNEY_KEK=$(openssl rand -base64 32)

# Path to the AttributeSchema JSON that classifies each attribute path as
# plaintext or per-subject secret (optional). When unset, the service runs
# with a permissive schema that treats every path as plaintext.
export JOURNEY_ATTRIBUTE_SCHEMA_PATH=./attribute_schema.json
```

> **`JOURNEY_ATTRIBUTE_SCHEMA_PATH`** controls how `SetAttributes` routes each
> path. The permissive default is convenient for local development, but in
> production you should supply a schema so that PII paths are encrypted under
> the right subject's DEK. See the
> [migration guide](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md#configuring-your-attributeschema)
> for the file format (`permissive`, `plaintext_prefixes`, `namespace_patterns`, exact `paths`).

> **Keep `JOURNEY_KEK` safe.** It wraps every per-subject Data Encryption Key stored in the
> database. Losing it makes all encrypted PII permanently irrecoverable. In production, load it
> from a secrets manager (AWS Secrets Manager, HashiCorp Vault, etc.) rather than an environment
> variable.

---

## Run

```bash
cargo run -p journey_dynamics
# Listening on 0.0.0.0:3030
```

---

## API

### Journeys

#### Create a journey

```bash
curl -i -X POST http://localhost:3030/journeys
```

Returns `201 Created` with a `Location: /journeys/{journey_id}` header.

#### Query a journey

```bash
curl http://localhost:3030/journeys/{journey_id}
```

The response includes `shared_data` (the merged path-keyed attribute bag),
`persons`, and `latest_workflow_decision`. The decision now carries a
`phase` label alongside `suggested_actions` — read `phase` to drive UI
state. The top-level `current_step` field is deprecated (it is still
populated for legacy `StepProgressed` events); prefer
`latest_workflow_decision.phase`.

#### Set attributes (recommended)

`SetAttributes` accepts a flat map of path → value (or the nested sugar form below) and
routes each attribute to plaintext storage or per-subject encrypted storage based on your
[`AttributeSchema`](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md#configuring-your-attributeschema).
A single submission can touch attributes for multiple data subjects atomically.

The **nested sugar form** (server-side flattened) is the most ergonomic on the wire:

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "SetAttributes": {
      "search": {
        "tripType":      "round-trip",
        "origin":        "LHR",
        "destination":   "JFK",
        "departureDate": "2025-08-15",
        "passengers": {
          "total":    1,
          "adults":   1,
          "children": 0,
          "infants":  0
        }
      }
    }
  }'
```

The canonical **flat form** is also accepted:

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "SetAttributes": {
      "changes": {
        "search/origin":      "LHR",
        "search/destination": "JFK"
      }
    }
  }'
```

For per-person PII (passport number, date of birth, …) call `CapturePerson` first to
bind a `subject_id` to the person slot, then use paths under `persons/<ref>/…`:

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "SetAttributes": {
      "persons": {
        "lead_booker": {
          "dateOfBirth":    "1990-05-15",
          "passportNumber": "GB123456789",
          "nationality":    "GB",
          "passengerType":  "adult"
        }
      }
    }
  }'
```

> **See also:** [`docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md)
> for schema configuration, migration recipes, and a comparison with the legacy commands.

---

#### Capture person identity (PII)

`CapturePerson` is **not** deprecated — it is still the way to bind a `subject_id` to a
person slot before you write per-person attributes with `SetAttributes`.

`person_ref` is a journey-local slot name (e.g. `"lead_booker"`, `"passenger_0"`). It is not
PII and is stored in plaintext. `subject_id` is a stable UUID from your identity system — reuse
it for the same person across multiple journeys so a single erasure request covers all of them.

Name, email, and phone are encrypted at rest using AES-256-GCM under a per-subject Data
Encryption Key (DEK).

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "person_ref": "passenger_0",
      "subject_id": "'"$SUBJECT_ID"'",
      "name": "Alice Smith",
      "email": "alice@example.com",
      "phone": "+44-7700-900000"
    }
  }'
```

---

#### Legacy API (deprecated, pending the 0.3.0 release)

The commands below still work and replay correctly, but they are now annotated
`#[deprecated]` in the domain model (`since = "0.3.0"`, which is not yet published).
New integrations should use `SetAttributes` above; see the
[migration guide](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md) for a
command-by-command mapping. They remain fully functional until an explicit
removal RFC is accepted.

##### Capture shared step data (non-PII) — legacy

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "Capture": {
      "step": "search",
      "data": {
        "search": {
          "tripType": "round-trip",
          "origin": "LHR",
          "destination": "JFK",
          "departureDate": "2025-08-15",
          "passengers": {
            "total":    1,
            "adults":   1,
            "children": 0,
            "infants":  0
          }
        }
      }
    }
  }'
```

##### Capture per-person PII details — legacy

Free-form PII details for an existing person slot. Always encrypts regardless of schema.
Multiple calls for the same `person_ref` are merged.

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePersonDetails": {
      "person_ref": "passenger_0",
      "data": {
        "dateOfBirth":    "1990-05-15",
        "passportNumber": "GB123456789",
        "nationality":    "GB",
        "passengerType":  "adult"
      }
    }
  }'
```

#### Complete a journey

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '"Complete"'
```

### GDPR — Right to erasure

```bash
curl -i -X DELETE http://localhost:3030/subjects/{subject_id}
```

Returns `204 No Content`. This:

1. Permanently deletes the subject's Data Encryption Key — all ciphertext belonging to that
   subject in the event store becomes irrecoverable.
2. Emits a `SubjectForgotten` audit event on every affected journey's event stream.
3. Nulls out the subject's `journey_person` row(s) and sets `forgotten = true`.
4. Leaves all other persons' data and the journey's shared (non-PII) data completely intact.

See [`docs/QUICK_START.md`](docs/QUICK_START.md) for a full walkthrough including a
crypto-shredding demo.

For guidance on how to mint or resolve `subject_id` values — including the additional-passenger
case where no identity-system UUID is available — see
[`docs/SUBJECT_ID_STRATEGIES.md`](docs/SUBJECT_ID_STRATEGIES.md).

---

## Tests

```bash
# Unit and integration tests (requires a running Postgres)
cargo test

# Lint
cargo clippy -- --no-deps -Dclippy::pedantic -Dwarnings
```

---

## Documentation

| Document | Description |
|---|---|
| [`CHANGELOG.md`](CHANGELOG.md) | What changed, including the path-keyed attributes work and the list of deprecated commands/events/fields |
| [`docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md) | **Start here** — migrating to `SetAttributes` / `AttributesSet` (path-keyed attributes) |
| [`docs/PATH_KEYED_ATTRIBUTES_DESIGN.md`](docs/PATH_KEYED_ATTRIBUTES_DESIGN.md) | Design proposal and rationale behind path-keyed attributes |
| [`docs/QUICK_START.md`](docs/QUICK_START.md) | Step-by-step walkthrough and crypto-shredding demo |
| [`docs/MULTI_SUBJECT_DESIGN.md`](docs/MULTI_SUBJECT_DESIGN.md) | Multi-subject GDPR crypto-shredding design (current) |
| [`docs/PERSON_CAPTURE.md`](docs/PERSON_CAPTURE.md) | `CapturePerson` command reference (not deprecated); legacy `CapturePersonDetails` reference |
| [`docs/IMPLEMENTATION_SUMMARY.md`](docs/IMPLEMENTATION_SUMMARY.md) | What was built and why |
| [`docs/ARCHITECTURE_REVIEW.md`](docs/ARCHITECTURE_REVIEW.md) | Architecture Review Board (ARB) review document |
| [`docs/SUBJECT_ID_STRATEGIES.md`](docs/SUBJECT_ID_STRATEGIES.md) | How to mint or resolve `subject_id` values (authenticated users, additional passengers, GDPR erasure by email) |

---

## Examples

The [`examples/`](examples) directory contains standalone projects that exercise
the event-sourcing foundation:

| Example | Description |
|---|---|
| [`examples/flight-booking`](examples/flight-booking) | The flight-booking domain (schema, attribute classification, and decision rules) layered on the `journey_dynamics` crate. |
| [`examples/hr`](examples/hr) | A two-aggregate (`Person` + `Employment`) HR domain built **directly** on the [`es-capture`](crates/es-capture) spine. Demonstrates a shared data subject and **cross-aggregate crypto-shredding** — a single key deletion erases PII in both aggregates. Runs entirely in memory (no Postgres). See its [README](examples/hr/README.md). |

```bash
# Run the in-memory HR demo (hire → read → erase → read)
cargo run -p hr
```

---

## Regenerate the flight-booking schema

```bash
# Must be run from the examples/flight-booking directory
cargo run -p flight-booking --bin generate_schema
```