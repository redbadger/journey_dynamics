# Flight Booking Schema Usage

This document explains how the flight-booking example maps onto the
Journey Dynamics multi-subject GDPR model.

---

## Core principle: PII never flows through `Capture`

Journey Dynamics enforces a hard separation between shared journey data and
personally identifiable information (PII):

| Data | Command | Event | Encrypted? |
|---|---|---|---|
| Search criteria, flight selections, pricing, payment status, booking reference | `Capture` | `Modified` | Never |
| Passenger identity (name, email, phone) | `CapturePerson` | `PersonCaptured` | Always |
| Passport, date of birth, nationality | `CapturePersonDetails` | `PersonDetailsUpdated` | Always |

Each passenger is a separate **data subject** with their own DEK.  A GDPR
erasure request for one passenger shreds only that passenger's encrypted events
and read-model row — the booking reference, pricing, and every other
passenger's data survive intact.

---

## Schema types

### Shared (non-PII) — flows through `Capture`

| Type | Notes |
|---|---|
| `SearchCriteria` | Origin, destination, dates, passenger counts |
| `SearchResults` | Available flights returned by search |
| `BookingData` | Flight selections, pricing, insurance, payment, booking reference |

`BookingData` deliberately omits per-passenger PII.  Two non-PII workflow
signals live there instead:

| Field | Type | Purpose |
|---|---|---|
| `passengersReady` | `Option<u32>` | Count of passengers whose details have been submitted via `CapturePersonDetails`. Set by the app after capturing all passengers; tells the decision engine to advance to seat selection. |
| `hasUnaccompaniedMinors` | `Option<bool>` | Set to `true` by the app when any passenger is an unaccompanied minor. Routes the workflow to the unaccompanied-minor-services step. |

### PII — flows through `CapturePersonDetails`

`PassengerDetail` is **not** part of the `Capture` data schema.  It describes
the JSON body sent in a `CapturePersonDetails` command:

```rust
pub struct PassengerDetail {
    pub first_name: String,        // PII
    pub last_name: String,         // PII
    pub date_of_birth: String,     // PII — ISO 8601
    pub passport_number: Option<String>, // PII
    pub nationality: Option<String>,     // PII — ISO 3166-1 alpha-2
    pub passenger_type: PassengerType,   // not PII
}
```

---

## Typical booking flow

```
1. Capture search criteria (non-PII)
   POST /journeys/{id}
   { "Capture": { "step": "search_criteria", "data": { "search": { ... } } } }

2. Capture flight selections (non-PII)
   POST /journeys/{id}
   { "Capture": { "step": "outbound_flight_selection", "data": { "booking": { "selectedOutboundFlight": { ... } } } } }

3. For each passenger:

   a. Register identity (encrypted, per-subject DEK)
      POST /journeys/{id}
      { "CapturePerson": { "person_ref": "passenger_0", "subject_id": "...", "name": "Alice Smith", "email": "alice@example.com", "phone": null } }

   b. Capture PII details (encrypted, per-subject DEK)
      POST /journeys/{id}
      { "CapturePersonDetails": { "person_ref": "passenger_0", "data": { "firstName": "Alice", "lastName": "Smith", "dateOfBirth": "1990-05-15", "passportNumber": "GB123456789", "nationality": "GB", "passengerType": "adult" } } }

4. Signal the decision engine that all passengers are ready (non-PII)
   POST /journeys/{id}
   { "Capture": { "step": "passenger_details", "data": { "booking": { "passengersReady": 2 } } } }
   → WorkflowEvaluated: ["seat_selection", "passenger_details"]

5. Capture payment status (non-PII)
   POST /journeys/{id}
   { "Capture": { "step": "payment", "data": { "booking": { "paymentStatus": "completed" } } } }

6. Complete
   POST /journeys/{id}  "Complete"
```

---

## GDPR erasure example

```bash
# Shred passenger 1's PII only
curl -X DELETE http://localhost:3030/subjects/{passenger_1_subject_id}
```

**After shredding:**

- Passenger 1's `PersonCaptured` and `PersonDetailsUpdated` events in the store become permanently unreadable (DEK deleted).
- Passenger 1's `journey_person` row is nulled out (`forgotten = true`).
- The booking reference, pricing, flight selections, and all other passengers are completely untouched.

---

## Regenerate the schema

The JSON schema is generated from the Rust types in `src/lib.rs`:

```bash
cd examples/flight-booking
cargo run --bin generate_schema
```

After changing `BookingData` or any of its dependencies, re-run this command
and commit the updated `schemas/flight-booking-schema.json`.