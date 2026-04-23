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
```

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

#### Capture shared step data (non-PII)

`Capture` is for data that is **not** personally identifiable — search criteria, flight
selections, pricing, payment status, booking references, and so on. This data is stored in
plaintext and survives GDPR shredding intact.

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

#### Capture person identity (PII)

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

#### Capture per-person PII details

Free-form PII details (passport number, date of birth, nationality, …) for an existing person
slot. `CapturePerson` must be called first for the same `person_ref`. The `data` blob is
encrypted under the same subject's DEK. Multiple calls for the same `person_ref` are merged.

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
| [`docs/QUICK_START.md`](docs/QUICK_START.md) | Step-by-step walkthrough and crypto-shredding demo |
| [`docs/MULTI_SUBJECT_DESIGN.md`](docs/MULTI_SUBJECT_DESIGN.md) | Multi-subject GDPR crypto-shredding design (current) |
| [`docs/PERSON_CAPTURE.md`](docs/PERSON_CAPTURE.md) | `CapturePerson` and `CapturePersonDetails` command reference |
| [`docs/IMPLEMENTATION_SUMMARY.md`](docs/IMPLEMENTATION_SUMMARY.md) | What was built and why |
| [`docs/ARCHITECTURE_REVIEW.md`](docs/ARCHITECTURE_REVIEW.md) | Architecture Review Board (ARB) review document |

---

## Regenerate the flight-booking schema

```bash
# Must be run from the examples/flight-booking directory
cargo run -p flight-booking --bin generate_schema