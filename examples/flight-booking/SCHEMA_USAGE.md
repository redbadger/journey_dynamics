# Flight Booking Schema Usage

This document explains how the flight-booking example maps onto the
Journey Dynamics path-keyed attribute model with multi-subject GDPR
crypto-shredding.

---

## Core principle: paths classify data, not commands

Every attribute in a journey is stored at a slash-separated path
(e.g. `search/origin`, `persons/passenger_0/passportNumber`). An
`AttributeSchema` maps each path to one of two classes:

| Class | Storage | Encrypted? | Survives shredding? |
|---|---|---|---|
| `Plaintext` | `shared_data` (JSON tree) | Never | Always |
| `Secret { subject }` | Encrypted partition in `AttributesSet` event; mirrored to `persons[].details` | Always (AES-256-GCM, per-subject DEK) | No — irrecoverable once the subject's DEK is deleted |

A single `SetAttributes` command can touch attributes across multiple
classifications and multiple subjects atomically.

---

## Flight-booking path taxonomy

| Path prefix | Classification | Notes |
|---|---|---|
| `search/*` | `Plaintext` | Origin, destination, dates, passenger counts |
| `searchResults/*` | `Plaintext` | Available flights returned by the search |
| `booking/*` | `Plaintext` | Flight selections, pricing, insurance, payment, booking reference |
| `persons/<ref>/passengerType` | `Plaintext` | Not PII; read directly by the decision engine |
| `persons/<ref>/firstName`, `lastName`, `dateOfBirth`, `passportNumber`, `nationality` | `Secret { subject: persons/<ref>/subject_id }` | Encrypted under the subject's DEK |

Each passenger is a separate **data subject** with their own DEK. A
GDPR erasure request for one passenger shreds only that passenger's
encrypted events and read-model row — the booking reference, pricing,
and every other passenger's data survive intact.

---

## Typical booking flow

### 1. Create a journey

```bash
curl -si -X POST http://localhost:3030/journeys
# → 201 Created, Location: /journeys/{JOURNEY_ID}
```

### 2. Set search criteria (non-PII)

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "SetAttributes": {
      "search": {
        "tripType":      "round-trip",
        "origin":        "LHR",
        "destination":   "JFK",
        "departureDate": "2026-09-01",
        "passengers": { "total": 2, "adults": 2, "children": 0, "infants": 0 }
      }
    }
  }'
```

### 3. Register each passenger (identity binding, encrypted)

`CapturePerson` is not deprecated. Call it once per passenger to bind a
stable `subject_id` (from your identity system) to a journey-local slot
name before sending any `persons/<ref>/…` secret attributes.

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"passenger_0\",
      \"subject_id\": \"$SUBJECT_ID_0\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      null
    }
  }"

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"passenger_1\",
      \"subject_id\": \"$SUBJECT_ID_1\",
      \"name\":       \"Bob Jones\",
      \"email\":      \"bob@example.com\",
      \"phone\":      null
    }
  }"
```

### 4. Set passenger PII attributes

Both passengers' details can be submitted in a single `SetAttributes`
command. The aggregate splits them by subject and emits one encrypted
partition per subject in the same `AttributesSet` event — atomically.

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "SetAttributes": {
      "persons": {
        "passenger_0": {
          "firstName":      "Alice",
          "lastName":       "Smith",
          "dateOfBirth":    "1990-05-15",
          "passportNumber": "GB123456789",
          "nationality":    "GB",
          "passengerType":  "adult"
        },
        "passenger_1": {
          "firstName":      "Bob",
          "lastName":       "Jones",
          "dateOfBirth":    "1985-11-23",
          "passportNumber": "US987654321",
          "nationality":    "US",
          "passengerType":  "adult"
        }
      }
    }
  }'
```

The decision engine reads `persons/passenger_0/passengerType` (plaintext)
directly from `shared_data`. You no longer need to copy it into a
summary field on `booking`.

### 5. Set flight selections (non-PII)

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "SetAttributes": {
      "booking": {
        "selectedOutboundFlight": {
          "flightNumber": "BA117",
          "departureTime": "09:00",
          "arrivalTime": "12:00"
        },
        "totalPrice": 850.00,
        "paymentStatus": "pending"
      }
    }
  }'
```

### 6. Complete

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '"Complete"'
```

---

## GDPR erasure example

```bash
# Shred passenger_1's PII only
curl -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID_1"
```

**After shredding:**

- Passenger 1's `PersonCaptured` and `AttributesSet` events (secret
  partition for `passenger_1`) become permanently unreadable (DEK deleted).
- Passenger 1's `journey_person` row is nulled out (`forgotten = true`).
- Passenger 0's data, the booking reference, pricing, and all `search/*`
  and `booking/*` attributes are completely untouched.

---

## `phase` values and the decision engine

The JDM orchestrator computes a `phase` label from the current attribute
bag rather than from explicit step transitions:

| Phase | Condition (illustrative) |
|---|---|
| `collecting_search` | `search/origin` or `search/destination` missing |
| `collecting_passengers` | All search fields present; passenger details incomplete |
| `ready_to_pay` | All passenger details present; `booking/paymentStatus` not `"completed"` |
| `completing` | `booking/paymentStatus` = `"completed"` |

Read the phase from `latest_workflow_decision.phase` on the journey view:

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" \
  | jq '.latest_workflow_decision.phase'
```

---

## Legacy API (deprecated in Phase C)

The commands below still work and are fully supported. They will be
marked `#[deprecated]` in a future release. New integrations should use
`SetAttributes` above.

| Deprecated command | Replacement |
|---|---|
| `Capture { step, data }` | `SetAttributes` with paths under `<step>/…` |
| `CapturePersonDetails { person_ref, data }` | `SetAttributes` with paths under `persons/<ref>/…` |

```bash
# Legacy: capture non-PII data
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{ "Capture": { "step": "search_criteria", "data": { "search": { "origin": "LHR", "destination": "JFK" } } } }'

# Legacy: capture PII details (always encrypts regardless of schema)
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{ "CapturePersonDetails": { "person_ref": "passenger_0", "data": { "firstName": "Alice", "passportNumber": "GB123456789", "passengerType": "adult" } } }'
```

See the [migration guide](../../docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md)
for full before/after recipes.

---

## Regenerate the schema

The JSON schema is generated from the Rust types in `src/lib.rs`:

```bash
cd examples/flight-booking
cargo run --bin generate_schema
```

After changing `BookingData` or any of its dependencies, re-run this
command and commit the updated `schemas/flight-booking-schema.json`.
