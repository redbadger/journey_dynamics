# Quick Start: Person Capture

## What It Does

Captures structured person data (name, email, phone) in journeys and stores it in a dedicated database table for efficient querying.

## Basic Example

```rust
use journey_dynamics::domain::commands::JourneyCommand;
use uuid::Uuid;

// 1. Start a journey
let journey_id = Uuid::new_v4();
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Start { id: journey_id },
).await?;

// 2. Capture person data
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        name: "Alice Johnson".to_string(),
        email: "alice@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;

// 3. Query the data
let repo = StructuredJourneyViewRepository::new(pool);
let person = repo.load_person(&journey_id).await?;
```

## HTTP API

```bash
# Capture person
curl -X POST http://localhost:3030/journey/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "name": "Alice Johnson",
      "email": "alice@example.com",
      "phone": "+1-555-0123"
    }
  }'
```

## Query Methods

```rust
// Get person for a journey
let person = repo.load_person(&journey_id).await?;

// Find journeys by email
let journeys = repo.find_by_email("alice@example.com").await?;

// Get all persons
let all_persons = repo.load_all_persons().await?;
```

## Database Schema

```sql
CREATE TABLE journey_person (
    id          SERIAL      PRIMARY KEY,
    journey_id  UUID        NOT NULL UNIQUE,
    name        TEXT        NOT NULL,
    email       TEXT        NOT NULL,
    phone       TEXT,
    created_at  TIMESTAMP   DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMP   DEFAULT CURRENT_TIMESTAMP
);
```

## Key Features

- ✅ **Type-safe**: No JSON parsing
- ✅ **Indexed**: Fast queries by email
- ✅ **Upserts**: Can update person data
- ✅ **Optional phone**: Not always required
- ✅ **Event-sourced**: Full history preserved

## Run Example

```bash
cargo run --example capture_person
```

## Run Tests

```bash
# Unit tests
cargo test test_capture_person

# Integration tests (requires PostgreSQL)
cargo test test_person_captured_event -- --ignored
```

## Learn More

- Full documentation: [PERSON_CAPTURE.md](./PERSON_CAPTURE.md)
- Implementation details: [IMPLEMENTATION_SUMMARY.md](./IMPLEMENTATION_SUMMARY.md)