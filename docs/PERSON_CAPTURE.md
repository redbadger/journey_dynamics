# Capturing Person Data in Journeys

This guide explains how to capture structured person data (name, email, phone) in journeys using the `CapturePerson` command.

## Overview

Instead of storing person data as generic JSON blobs, the journey system supports a dedicated `CapturePerson` command that:

1. Emits a `PersonCaptured` event
2. Projects data into a structured `journey_person` database table
3. Enables efficient querying by email or other person attributes

## Database Schema

The `journey_person` table structure:

```sql
CREATE TABLE journey_person
(
    id                  SERIAL                       NOT NULL PRIMARY KEY,
    journey_id          UUID                         NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    name                TEXT                         NOT NULL,
    email               TEXT                         NOT NULL,
    phone               TEXT,
    created_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (journey_id)
);

CREATE INDEX idx_journey_person_journey_id ON journey_person(journey_id);
CREATE INDEX idx_journey_person_email ON journey_person(email);
```

## Usage

### Capturing Person Data

Use the `CapturePerson` command to capture person information:

```rust
use journey_dynamics::domain::commands::JourneyCommand;

// Capture person with phone number
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;

// Capture person without phone number
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        name: "Bob Smith".to_string(),
        email: "bob.smith@example.com".to_string(),
        phone: None,
    },
).await?;
```

### HTTP API Example

Send a POST request to capture person data:

```bash
curl -X POST http://localhost:3030/journey/{journey_id} \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePerson": {
      "name": "Alice Johnson",
      "email": "alice.johnson@example.com",
      "phone": "+1-555-0123"
    }
  }'
```

## Querying Person Data

### Load Person Data for a Journey

```rust
use journey_dynamics::view_repository::StructuredJourneyViewRepository;

let repo = StructuredJourneyViewRepository::new(pool);

// Load person data for a specific journey
let person = repo.load_person(&journey_id).await?;

if let Some(person) = person {
    println!("Name: {}", person.name);
    println!("Email: {}", person.email);
    println!("Phone: {:?}", person.phone);
}
```

### Find Journeys by Email

```rust
// Find all journeys associated with an email address
let journeys = repo.find_by_email("alice.johnson@example.com").await?;

for journey in journeys {
    println!("Journey {}: {:?}", journey.id, journey.state);
}
```

### Load All Persons

```rust
// Get all persons from all journeys
let persons = repo.load_all_persons().await?;

for person in persons {
    println!("{} - {}", person.name, person.email);
}
```

## Event Flow

When a `CapturePerson` command is executed:

1. **Command Handler** validates the journey exists and is not completed
2. **Event Emission** produces a `PersonCaptured` event:
   ```rust
   PersonCaptured {
       name: "Alice Johnson".to_string(),
       email: "alice.johnson@example.com".to_string(),
       phone: Some("+1-555-0123".to_string()),
   }
   ```
3. **Event Store** persists the event to the `events` table
4. **View Projection** updates the `journey_person` table via the repository

## Benefits

### ✅ Type Safety
- Person data has explicit fields with proper types
- No JSON parsing errors at query time
- Database constraints enforce data validity

### ✅ Efficient Queries
- Direct SQL queries on structured columns
- Indexes on email and journey_id for fast lookups
- No need to parse JSON blobs

### ✅ Domain Separation
- Person data is cleanly separated from generic journey data
- Easy to add domain-specific validation
- Clear intent in the code

### ✅ Flexibility
- Can still use generic `Capture` command for other data
- Multiple domain tables can coexist (person, address, payment, etc.)
- Easy to extend with additional fields

## Updating Person Data

Person data can be updated by issuing another `CapturePerson` command for the same journey:

```rust
// Initial capture
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: None,
    },
).await?;

// Update with phone number
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;
```

The database uses `ON CONFLICT (journey_id) DO UPDATE` to handle updates gracefully.

## Business Rules

- Journey must be started before capturing person data
- Journey must not be completed
- One person record per journey (enforced by UNIQUE constraint)
- Email and name are required; phone is optional

## Example: Complete Flow

```rust
use cqrs_es::CqrsFramework;
use journey_dynamics::domain::commands::JourneyCommand;
use uuid::Uuid;

// Start journey
let journey_id = Uuid::new_v4();
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Start { id: journey_id },
).await?;

// Capture person data
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::CapturePerson {
        name: "Alice Johnson".to_string(),
        email: "alice.johnson@example.com".to_string(),
        phone: Some("+1-555-0123".to_string()),
    },
).await?;

// Capture other data (generic)
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Capture {
        data: ("preferences".to_string(), json!({"newsletter": true})),
    },
).await?;

// Complete journey
cqrs.execute(
    &journey_id.to_string(),
    JourneyCommand::Complete,
).await?;

// Query person data
let repo = StructuredJourneyViewRepository::new(pool);
let person = repo.load_person(&journey_id).await?.unwrap();
println!("Journey completed for: {} ({})", person.name, person.email);
```

## Testing

Run the unit tests:

```bash
cargo test test_capture_person
```

Run the integration tests (requires PostgreSQL):

```bash
cargo test test_person_captured_event -- --ignored
cargo test test_find_by_email -- --ignored
cargo test test_person_update -- --ignored
```

Run the example:

```bash
cargo run --example capture_person
```

## Adding More Domain Tables

Follow this pattern to add other domain-specific data (e.g., addresses, payments):

1. Add command variant to `JourneyCommand`
2. Add event variant to `JourneyEvent`
3. Create database table with proper schema
4. Add handler in `Journey::handle()`
5. Add projection in `StructuredJourneyViewRepository::update_view()`
6. Add query methods to the repository

This keeps your codebase clean and domain concepts well-separated while leveraging the same event sourcing infrastructure.