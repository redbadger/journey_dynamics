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

#### Capture step data

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "Capture": {
      "step": "search",
      "data": {
        "tripType": "round-trip",
        "origin": "LHR",
        "destination": "JFK"
      }
    }
  }'
```

#### Capture person data (PII)

`subject_id` is an opaque identifier for the data subject. Use the same UUID for the same
person across multiple journeys so that a single erasure request shreds all their data.

```bash
curl -X POST http://localhost:3030/journeys/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "subject_id": "'"$SUBJECT_ID"'",
      "name": "Alice Smith",
      "email": "alice@example.com",
      "phone": "+44-7700-900000"
    }
  }'
```

Name, email, and phone are encrypted at rest using Advanced Encryption Standard 256-bit Galois/Counter Mode (AES-256-GCM). The `subject_id` is stored
in plaintext so the correct key can be found on the read path without decrypting anything first.

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

1. Permanently deletes the subject's Data Encryption Key — all ciphertext in the event store
   becomes irrecoverable.
2. Emits a `SubjectForgotten` audit event on every affected journey's event stream.
3. Deletes the `journey_person` row and clears `accumulated_data` for every affected journey
   in the read model.

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
| [`docs/CRYPTO_SHREDDING_DESIGN.md`](docs/CRYPTO_SHREDDING_DESIGN.md) | Full GDPR crypto-shredding design |
| [`docs/PERSON_CAPTURE.md`](docs/PERSON_CAPTURE.md) | `CapturePerson` command reference |
| [`docs/IMPLEMENTATION_SUMMARY.md`](docs/IMPLEMENTATION_SUMMARY.md) | What was built and why |
| [`docs/ARCHITECTURE_REVIEW.md`](docs/ARCHITECTURE_REVIEW.md) | Architecture Review Board (ARB) review document |

---

## Regenerate the flight-booking schema

```bash
# Must be run from the examples/flight-booking directory
cargo run -p flight-booking --bin generate_schema
```
