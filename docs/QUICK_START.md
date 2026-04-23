# Quick Start & Crypto-Shredding Demo

A complete walkthrough: start a journey, capture non-PII shared data, capture PII via the
dedicated person commands, verify encryption at rest, then exercise the GDPR right-to-erasure
endpoint and confirm only the target subject's data is gone — shared journey data survives.

---

## 1. Prerequisites

- Rust (stable toolchain)
- Docker **or** Homebrew (see step 2)
- `sqlx-cli` (`cargo install sqlx-cli`)
- `curl`, `jq`, and `psql` (for the demo commands)

---

## 2. Start Postgres

Choose one of the two approaches below.

### Option A — Docker

```bash
docker-compose up -d
```

### Option B — Homebrew (macOS)

Install and start the server:

```bash
brew install postgresql@18
brew services start postgresql@18
```

If `psql` is not yet on your `PATH`, add the brew-managed binaries:

**bash / zsh**

```bash
export PATH="$(brew --prefix postgresql@18)/bin:$PATH"
```

**fish**

```fish
fish_add_path (brew --prefix postgresql@18)/bin
```

Homebrew creates a superuser named after your macOS login user. Create a `postgres` role
with a password to match the `DATABASE_URL` used throughout this guide:

```bash
psql -d template1 -c "CREATE ROLE postgres WITH SUPERUSER LOGIN PASSWORD 'postgres';"
```

---

## 3. Environment

Set the two required environment variables before running any of the following steps.

**bash / zsh**

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/journey_dynamics

# Generate a random 256-bit Key Encryption Key (KEK).
# This wraps every per-subject Data Encryption Key (DEK) stored in the database.
# Store it somewhere safe — losing it makes all encrypted PII permanently irrecoverable.
export JOURNEY_KEK=$(openssl rand -base64 32)

echo "KEK: $JOURNEY_KEK"   # copy this somewhere for the demo
```

**fish**

```fish
set -x DATABASE_URL postgres://postgres:postgres@localhost:5432/journey_dynamics
set -x JOURNEY_KEK (openssl rand -base64 32)

echo "KEK: $JOURNEY_KEK"
```

---

## 4. Create the database and run migrations

```bash
cargo sqlx database create
cargo sqlx migrate run
```

---

## 5. Start the server

```bash
cargo run -p journey_dynamics
# Listening on 0.0.0.0:3030
```

Open a second terminal for the curl commands below.

---

## 6. Basic journey walkthrough

### 6.1 Create a journey

**bash / zsh**

```bash
JOURNEY_LOCATION=$(curl -si -X POST http://localhost:3030/journeys \
  | grep -i '^location:' | tr -d '\r' | awk '{print $2}')

JOURNEY_ID=$(echo "$JOURNEY_LOCATION" | sed 's|/journeys/||')

echo "Journey ID: $JOURNEY_ID"
```

**fish**

```fish
set JOURNEY_LOCATION (curl -si -X POST http://localhost:3030/journeys \
  | grep -i '^location:' | tr -d '\r' | awk '{print $2}')

set JOURNEY_ID (echo $JOURNEY_LOCATION | sed 's|/journeys/||')

echo "Journey ID: $JOURNEY_ID"
```

### 6.2 Capture shared (non-PII) data

`Capture` is for data that is **not** personally identifiable — search criteria, flight
selections, pricing, payment status, booking references, and so on.  This data is stored in
plaintext and is **never encrypted**.  It survives GDPR shredding completely intact.

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "Capture": {
      "step": "search",
      "data": {
        "search": {
          "tripType":      "round-trip",
          "origin":        "LHR",
          "destination":   "JFK",
          "departureDate": "2026-09-01",
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

### 6.3 Capture person identity (PII)

`CapturePerson` registers a named person slot (`person_ref`) within the journey.
`person_ref` is a journey-local slot name — it is not PII and is stored in plaintext.
`subject_id` is a stable UUID from your identity system; reuse it for the same person across
multiple journeys so a single erasure request covers all of them.

Name, email, and phone are encrypted at rest using AES-256-GCM under a per-subject DEK.

Pick a stable `subject_id` for this person:

**bash / zsh**

```bash
SUBJECT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "Subject ID: $SUBJECT_ID"   # save this — you will need it for the erasure step

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"lead_booker\",
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      \"+44-7700-900000\"
    }
  }"
```

**fish**

```fish
set SUBJECT_ID (uuidgen | tr '[:upper:]' '[:lower:]')
echo "Subject ID: $SUBJECT_ID"

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"lead_booker\",
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      \"+44-7700-900000\"
    }
  }"
```

### 6.4 Capture per-person PII details

`CapturePersonDetails` captures free-form PII (passport, date of birth, nationality, …) for
an existing person slot.  The `data` blob is encrypted under the same subject's DEK.
`CapturePerson` must be called first for the same `person_ref`.

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "CapturePersonDetails": {
      "person_ref": "lead_booker",
      "data": {
        "dateOfBirth":    "1990-05-15",
        "passportNumber": "GB12345678",
        "nationality":    "GB",
        "passengerType":  "adult"
      }
    }
  }'
```

Multiple `CapturePersonDetails` calls for the same `person_ref` are merged (JSON
merge-patch), so you can split the data across several requests if needed.

### 6.5 Query the journey — shared data visible, PII in separate table

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" | jq .
```

You should see something like:

```json
{
  "id": "...",
  "state": "InProgress",
  "shared_data": {
    "search": {
      "tripType":      "round-trip",
      "origin":        "LHR",
      "destination":   "JFK",
      "departureDate": "2026-09-01",
      "passengers": {
        "total":    1,
        "adults":   1,
        "children": 0,
        "infants":  0
      }
    }
  },
  "current_step": "search",
  "latest_workflow_decision": null,
  "persons": [
    {
      "journey_id":  "...",
      "person_ref":  "lead_booker",
      "subject_id":  "...",
      "name":        "Alice Smith",
      "email":       "alice@example.com",
      "phone":       "+44-7700-900000",
      "details": {
        "dateOfBirth":    "1990-05-15",
        "passportNumber": "GB12345678",
        "nationality":    "GB",
        "passengerType":  "adult"
      },
      "forgotten": false
    }
  ]
}
```

`shared_data` contains only the non-PII data from `Capture` commands.  The `persons` array
contains every person slot associated with the journey — identity fields from `CapturePerson`
and free-form details from `CapturePersonDetails`, both decrypted on the read path.  The
underlying event payloads in the event store are AES-256-GCM ciphertext; what you see here
is the decrypted projection.

---

## 7. Verify encryption at rest (optional)

Connect to the database directly and inspect the raw event payloads.

### 7.1 PersonCaptured — always encrypted

The `PersonCaptured` event stores name/email/phone as AES-256-GCM ciphertext.
`person_ref` and `subject_id` remain in plaintext so the read path can find the right DEK:

```bash
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'PersonCaptured';"
```

Expected output:

```
  event_type    |                              payload
----------------+--------------------------------------------------------------
 PersonCaptured | {"PersonCaptured": {"person_ref": "lead_booker",
                |   "subject_id": "...", "encrypted_pii": "8f3aK...",
                |   "nonce": "mNq2..."}}
```

### 7.2 PersonDetailsUpdated — always encrypted

The `PersonDetailsUpdated` event stores the entire `data` blob as ciphertext:

```bash
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'PersonDetailsUpdated';"
```

```
     event_type       |                              payload
----------------------+--------------------------------------------------------------
 PersonDetailsUpdated | {"PersonDetailsUpdated": {"person_ref": "lead_booker",
                      |   "subject_id": "...", "encrypted_data": "xT7pR...",
                      |   "nonce": "..."}}
```

### 7.3 Modified — always plaintext

`Modified` events carry only shared, non-PII journey data and are **never** encrypted,
regardless of whether a person has been captured on the journey.

```bash
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'JourneyModified'
   ORDER BY sequence;"
```

```
  event_type    |                              payload
----------------+--------------------------------------------------------------
 JourneyModified | {"Modified": {"step": "search",
                 |   "data": {"search": {"tripType": "round-trip",
                 |     "origin": "LHR", "destination": "JFK",
                 |     "departureDate": "2026-09-01"}}}}
```

---

## 8. Crypto-shredding demo — GDPR right to erasure

### 8.1 Send the erasure request

```bash
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID"
```

Expected response:

```
HTTP/1.1 204 No Content
```

This single call:

1. **Deletes the DEK** for this subject from `subject_encryption_keys`.  The AES-256-GCM
   ciphertext in the event store is now permanently unreadable — no key, no data.
2. **Emits a `SubjectForgotten` audit event** on every journey that contains this subject,
   creating a tamper-evident record without containing any PII.
3. **Nulls the `journey_person` row** for this subject: `name`, `email`, `phone`, and
   `details` are cleared; `forgotten` is set to `true`.
4. **Leaves `shared_data` completely intact** — the search criteria, flight selections, and
   any other non-PII journey data are untouched.

### 8.2 Query the journey — shared data still visible

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" | jq .
```

```json
{
  "id": "...",
  "state": "InProgress",
  "shared_data": {
    "search": {
      "tripType":      "round-trip",
      "origin":        "LHR",
      "destination":   "JFK",
      "departureDate": "2026-09-01",
      "passengers": {
        "total":    1,
        "adults":   1,
        "children": 0,
        "infants":  0
      }
    }
  },
  "current_step": "search",
  "latest_workflow_decision": null,
  "persons": [
    {
      "journey_id":  "...",
      "person_ref":  "lead_booker",
      "subject_id":  "...",
      "name":        null,
      "email":       null,
      "phone":       null,
      "details":     {},
      "forgotten":   true
    }
  ]
}
```

`shared_data` is completely intact — the search criteria survive because they were never
encrypted.  The `persons` array still contains the slot, but it is now a tombstone:
`forgotten` is `true` and all PII fields (`name`, `email`, `phone`, `details`) are null or
empty.  The `subject_id` and `person_ref` are retained so the slot remains identifiable for
audit purposes.

### 8.3 Verify the event store — ciphertext is still there, key is not

The raw event payloads have not been touched — the encrypted blobs are still in the `events`
table exactly as they were written.  What changed is that the key is gone:

```bash
# The PersonCaptured encrypted_pii blob still exists...
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'PersonCaptured';"
```

```
  event_type    |                              payload
----------------+--------------------------------------------------------------
 PersonCaptured | {"PersonCaptured": {"person_ref": "lead_booker",
                |   "subject_id": "...", "encrypted_pii": "8f3aK...",
                |   "nonce": "mNq2..."}}
```

```bash
# ...as does the PersonDetailsUpdated encrypted_data blob...
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'PersonDetailsUpdated';"
```

```
     event_type       |                              payload
----------------------+--------------------------------------------------------------
 PersonDetailsUpdated | {"PersonDetailsUpdated": {"person_ref": "lead_booker",
                      |   "subject_id": "...", "encrypted_data": "xT7pR...",
                      |   "nonce": "..."}}
```

```bash
# ...but the key is gone from the key store
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT subject_id FROM subject_encryption_keys
   WHERE subject_id = '$SUBJECT_ID';"
```

```
 subject_id
-----------
(0 rows)
```

```bash
# And a SubjectForgotten audit event was appended to the journey's stream
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'SubjectForgotten';"
```

```
   event_type    |                     payload
-----------------+--------------------------------------------------
 SubjectForgotten | {"SubjectForgotten": {"subject_id": "..."}}
```

### 8.4 Aggregate re-hydration after shredding

To prove the aggregate still loads correctly after shredding, send a `Complete` command.
The server must rehydrate the journey by replaying every event from the store — including
the encrypted `PersonCaptured` and `PersonDetailsUpdated` events whose key is now gone.
The crypto layer substitutes safe sentinels for the unreadable payloads and the command
succeeds:

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '"Complete"'
```

Query the journey to confirm the state transition happened and shared data is preserved:

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" | jq .
```

```json
{
  "id": "...",
  "state": "Complete",
  "shared_data": {
    "search": {
      "tripType":      "round-trip",
      "origin":        "LHR",
      "destination":   "JFK",
      "departureDate": "2026-09-01",
      "passengers": {
        "total":    1,
        "adults":   1,
        "children": 0,
        "infants":  0
      }
    }
  },
  "current_step": "search",
  "latest_workflow_decision": null,
  "persons": [
    {
      "journey_id":  "...",
      "person_ref":  "lead_booker",
      "subject_id":  "...",
      "name":        null,
      "email":       null,
      "phone":       null,
      "details":     {},
      "forgotten":   true
    }
  ]
}
```

`state` is now `"Complete"`, `shared_data` is fully intact, and `persons` still shows the
tombstone slot.  The sentinels applied during rehydration were:

- `PersonCaptured` → `name: "[redacted]"`, `email: "[redacted]"`, `phone: null`
- `PersonDetailsUpdated` → `data: {}`
- `Modified` events → **unchanged** (they were never encrypted)

Structural history — which steps were taken, workflow decisions, completion status — is fully
preserved.  Only the personal data is gone.

---

## 9. Multi-journey shredding

The same `subject_id` can be used across many journeys (e.g. a returning customer who starts
multiple bookings).  A single erasure request shreds all of them at once because a subject
has exactly one DEK regardless of how many journeys they appear in:

**bash / zsh**

```bash
# Start a second journey for the same subject
JOURNEY2_LOCATION=$(curl -si -X POST http://localhost:3030/journeys \
  | grep -i '^location:' | tr -d '\r' | awk '{print $2}')
JOURNEY_ID_2=$(echo "$JOURNEY2_LOCATION" | sed 's|/journeys/||')

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID_2" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"lead_booker\",
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      null
    }
  }"

# One DELETE shreds both journeys' PersonCaptured events for this subject
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID"
```

**fish**

```fish
set JOURNEY2_LOCATION (curl -si -X POST http://localhost:3030/journeys \
  | grep -i '^location:' | tr -d '\r' | awk '{print $2}')
set JOURNEY_ID_2 (echo $JOURNEY2_LOCATION | sed 's|/journeys/||')

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID_2" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"lead_booker\",
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      null
    }
  }"

curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID"
```

---

## 10. Multi-subject journeys

A single journey can contain multiple data subjects — for example, all passengers in a
flight booking.  Each subject is an independent slot; shredding one leaves all others intact.

**bash / zsh**

```bash
# Capture a second passenger in the same journey
SUBJECT_ID_2=$(uuidgen | tr '[:upper:]' '[:lower:]')

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"passenger_1\",
      \"subject_id\": \"$SUBJECT_ID_2\",
      \"name\":       \"Bob Jones\",
      \"email\":      \"bob@example.com\",
      \"phone\":      null
    }
  }"

# Shred only the second passenger — Alice's data and all shared_data survive
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID_2"
```

**fish**

```fish
set SUBJECT_ID_2 (uuidgen | tr '[:upper:]' '[:lower:]')

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"person_ref\": \"passenger_1\",
      \"subject_id\": \"$SUBJECT_ID_2\",
      \"name\":       \"Bob Jones\",
      \"email\":      \"bob@example.com\",
      \"phone\":      null
    }
  }"

# Shred only the second passenger — Alice's data and all shared_data survive
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID_2"
```

---

## 11. What is NOT shredded

| Data | Shredded? | Reason |
|---|---|---|
| `name`, `email`, `phone` in `PersonCaptured` | ✅ Yes (ciphertext, key deleted) | Direct PII |
| `data` in `PersonDetailsUpdated` | ✅ Yes (ciphertext, key deleted) | Direct PII |
| `journey_person` row for target subject | ✅ Yes (fields nulled, `forgotten = true`) | Read-model PII cache |
| Other persons' `journey_person` rows | ❌ No | Different DEK — independent subject |
| `journey_view.shared_data` | ❌ No | Never contained PII; never encrypted |
| `Started`, `Completed` events | ❌ No | No personal data |
| `StepProgressed`, `WorkflowEvaluated` events | ❌ No | Workflow metadata only |
| `Modified` events | ❌ No | Non-PII data only; never encrypted |
| `SubjectForgotten` event | ❌ No | Audit trail — contains only `subject_id`, not PII |
| `subject_id` in event payloads | ❌ No | Opaque identifier — not PII |

---

## Further reading

- [`MULTI_SUBJECT_DESIGN.md`](MULTI_SUBJECT_DESIGN.md) — full multi-subject GDPR
  crypto-shredding design (current)
- [`PERSON_CAPTURE.md`](PERSON_CAPTURE.md) — `CapturePerson` and `CapturePersonDetails`
  command reference
- [`IMPLEMENTATION_SUMMARY.md`](IMPLEMENTATION_SUMMARY.md) — what was built across the
  implementation phases