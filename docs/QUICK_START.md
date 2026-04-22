# Quick Start & Crypto-Shredding Demo

A complete walkthrough: start a journey, capture personally identifiable information (PII), verify it is encrypted at rest, then
exercise the General Data Protection Regulation (GDPR) right-to-erasure endpoint and confirm the data is gone for good.

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
with a password to match the `DATABASE_URL` used throughout this guide. We connect to
`template1` explicitly — it is guaranteed to exist on every PostgreSQL installation:

```bash
psql -d template1 -c "CREATE ROLE postgres WITH SUPERUSER LOGIN PASSWORD 'postgres';"
```

---

## 3. Environment

Set the two required environment variables before running any of the following steps.
Choose whichever approach suits your shell, or use a `.env` file.

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

# Generate a random 256-bit Key Encryption Key (KEK).
set -x JOURNEY_KEK (openssl rand -base64 32)

echo "KEK: $JOURNEY_KEK"   # copy this somewhere for the demo
```

**.env file** (useful if you use a tool such as [`direnv`](https://direnv.net/) or load it
explicitly with `set -a; source .env; set +a` in bash/zsh — or simply rely on the server
reading it via the `dotenvy` crate)

```ini
DATABASE_URL=postgres://postgres:postgres@localhost:5432/journey_dynamics

# Generate once with: openssl rand -base64 32
JOURNEY_KEK=<paste your generated key here>
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

Open a second terminal for the curl commands below. If you used shell exports, re-export
`DATABASE_URL` and `JOURNEY_KEK` in the new terminal. If you used a `.env` file, source it
again or rely on `direnv` to pick it up automatically.

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

### 6.2 Capture a search step

This step is captured **before** a person is associated with the journey, so its event data
will be stored as **plaintext**.

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "Capture": {
      "step": "search",
      "data": {
        "tripType": "round-trip",
        "origin": "LHR",
        "destination": "JFK",
        "departureDate": "2026-09-01"
      }
    }
  }'
```

### 6.3 Capture person data (PII)

Pick a stable `subject_id` for this person. In production this comes from your identity
system; in the demo we generate one once and reuse it.

**bash / zsh**

```bash
SUBJECT_ID=$(uuidgen | tr '[:upper:]' '[:lower:]')
echo "Subject ID: $SUBJECT_ID"   # save this — you will need it for the erasure step

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
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
echo "Subject ID: $SUBJECT_ID"   # save this — you will need it for the erasure step

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      \"+44-7700-900000\"
    }
  }"
```

### 6.4 Capture a passenger details step

This step is captured **after** the person is associated, so its event data will be
**encrypted at rest**.

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '{
    "Capture": {
      "step": "passenger_details",
      "data": {
        "firstName":      "Alice",
        "lastName":       "Smith",
        "dateOfBirth":    "1990-05-15",
        "passportNumber": "GB12345678",
        "nationality":    "GBR"
      }
    }
  }'
```

### 6.5 Query the journey — data is visible

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" | jq .
```

You should see something like:

```json
{
  "id": "...",
  "state": "InProgress",
  "accumulated_data": {
    "tripType": "round-trip",
    "origin": "LHR",
    "destination": "JFK",
    "departureDate": "2026-09-01",
    "firstName": "Alice",
    "lastName": "Smith",
    "dateOfBirth": "1990-05-15",
    "passportNumber": "GB12345678",
    "nationality": "GBR"
  },
  "current_step": "passenger_details",
  ...
}
```

The person data lives in the `journey_person` table (not in the journey view), so it is not
shown here — but it is in the database, and the name/email/phone in the event store are
**encrypted at rest**.

---

## 7. Verify encryption at rest (optional)

Connect to the database directly and inspect the raw event payloads.

### 7.1 PersonCaptured — always encrypted

The `PersonCaptured` event stores name/email/phone as AES-256-GCM ciphertext. Only
`subject_id` remains readable:

```bash
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'PersonCaptured';"
```

Expected output (values abbreviated):

```
  event_type   |                          payload
---------------+----------------------------------------------------------
 PersonCaptured | {"PersonCaptured": {"subject_id": "...",
                |   "encrypted_pii": "8f3aK...", "nonce": "mNq2..."}}
```

### 7.2 Modified (pre-person) — plaintext

The search step was captured **before** `CapturePerson`, so no encryption key existed yet.
Its payload is stored as plain JSON:

```bash
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'JourneyModified'
   ORDER BY sequence
   LIMIT 1;"
```

```
 event_type     |                          payload
----------------+----------------------------------------------------------
 JourneyModified | {"Modified": {"step": "search",
                 |   "data": {"search": {"tripType": "round-trip",
                 |     "origin": "LHR", "destination": "JFK",
                 |     "departureDate": "2026-09-01"}}}}
```

This is expected and legal: before a subject is identified, the data is not linked to a
natural person and is therefore not personal data under GDPR. See the
[design doc](CRYPTO_SHREDDING_DESIGN.md) for the full rationale. For maximum safety, journey
designs should call `CapturePerson` as early as possible — before any `Capture` commands that
include sensitive form data.

### 7.3 Modified (post-person) — encrypted

The passenger details step was captured **after** `CapturePerson`, so it is encrypted with
the subject's Data Encryption Key:

```bash
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'JourneyModified'
   ORDER BY sequence
   LIMIT 1
   OFFSET 1;"
```

```
 event_type     |                          payload
----------------+----------------------------------------------------------
 JourneyModified | {"Modified": {"step": "passenger_details",
                 |   "data": {"encrypted_data": "xT7pR...", "nonce": "..."}}}
```

---

## 8. Crypto-shredding demo — General Data Protection Regulation (GDPR) right to erasure

### 8.1 Send the erasure request

```bash
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID"
```

Expected response:

```
HTTP/1.1 204 No Content
```

This single call:

1. **Deletes the Data Encryption Key** for this subject from `subject_encryption_keys`. The
   Advanced Encryption Standard 256-bit Galois/Counter Mode (AES-256-GCM) ciphertext in the
   event store is now permanently unreadable — no key, no data.
2. **Emits a `SubjectForgotten` audit event** on every journey that belongs to this subject,
   creating a tamper-evident record of the erasure without containing any PII.
3. **Clears the read model**: the `journey_person` row is deleted and `accumulated_data` in
   `journey_view` is reset to `{}`.

### 8.2 Query the journey — data is gone

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" | jq .
```

```json
{
  "id": "...",
  "state": "InProgress",
  "accumulated_data": {},
  "current_step": "passenger_details",
  ...
}
```

`accumulated_data` is now `{}`. The trip details and passenger information are gone.

### 8.3 Verify the event store — ciphertext is still there, key is not

The raw event payloads have not been touched — the encrypted blobs are still in the `events`
table exactly as they were written. What changed is that the key is gone:

```bash
# The PersonCaptured encrypted_pii blob still exists in the event store...
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'PersonCaptured';"
```

```
  event_type   |                          payload
---------------+----------------------------------------------------------
 PersonCaptured | {"PersonCaptured": {"subject_id": "...",
                |   "encrypted_pii": "8f3aK...", "nonce": "mNq2..."}}
```

```bash
# ...as does the encrypted passenger details blob...
psql -h localhost -U postgres journey_dynamics -c \
  "SELECT event_type, payload FROM events
   WHERE aggregate_id = '$JOURNEY_ID'
     AND event_type   = 'JourneyModified'
   ORDER BY sequence
   LIMIT 1
   OFFSET 1;"
```

```
 event_type     |                          payload
----------------+----------------------------------------------------------
 JourneyModified | {"Modified": {"step": "passenger_details",
                 |   "data": {"encrypted_data": "xT7pR...", "nonce": "..."}}}
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

### 8.4 Aggregate rehydration after shredding

To prove the aggregate still loads correctly after shredding, send a `Complete` command.
The server must rehydrate the journey by replaying every event from the store — including
the encrypted `PersonCaptured` and `Modified` events whose key is now gone. The crypto
layer substitutes safe sentinels for the unreadable payloads and the command succeeds:

```bash
curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID" \
  -H "Content-Type: application/json" \
  -d '"Complete"'
```

No error response means the aggregate rehydrated and the command was accepted. Query the
journey to confirm the state transition happened and the PII is still absent:

```bash
curl -s "http://localhost:3030/journeys/$JOURNEY_ID" | jq .
```

```json
{
  "id": "...",
  "state": "Complete",
  "accumulated_data": {},
  "current_step": "passenger_details",
  ...
}
```

`state` is now `"Complete"`. The sentinels applied during rehydration were:

- `PersonCaptured` → `name: "[redacted]"`, `email: "[redacted]"`, `phone: null`
- `Modified` events captured **after** `CapturePerson` (e.g. passenger details) → `data: {}`
- `Modified` events captured **before** `CapturePerson` (e.g. search) → data is unchanged;
  it was never encrypted

Structural history — which steps were taken, workflow decisions, completion status — is fully
preserved. Only the personal data is gone.

---

## 9. Multi-journey shredding

The same `subject_id` can be used across many journeys (e.g. a returning customer who starts
multiple bookings). A single erasure request shreds all of them at once because all DEKs for
that subject share the same key:

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
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      null
    }
  }"

# One DELETE shreds both journeys
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID"
```

**fish**

```fish
# Start a second journey for the same subject
set JOURNEY2_LOCATION (curl -si -X POST http://localhost:3030/journeys \
  | grep -i '^location:' | tr -d '\r' | awk '{print $2}')
set JOURNEY_ID_2 (echo $JOURNEY2_LOCATION | sed 's|/journeys/||')

curl -s -X POST "http://localhost:3030/journeys/$JOURNEY_ID_2" \
  -H "Content-Type: application/json" \
  -d "{
    \"CapturePerson\": {
      \"subject_id\": \"$SUBJECT_ID\",
      \"name\":       \"Alice Smith\",
      \"email\":      \"alice@example.com\",
      \"phone\":      null
    }
  }"

# One DELETE shreds both journeys
curl -si -X DELETE "http://localhost:3030/subjects/$SUBJECT_ID"
```

---

## 10. What is NOT shredded

| Data | Shredded? | Reason |
|---|---|---|
| `name`, `email`, `phone` in `PersonCaptured` | ✅ Yes (ciphertext, key deleted) | Direct PII |
| `data` field in `Modified` events captured *after* `CapturePerson` | ✅ Yes (ciphertext, key deleted) | Personal data once subject is identified |
| `journey_person` row | ✅ Yes (row deleted) | Read-model PII cache |
| `journey_view.accumulated_data` | ✅ Yes (reset to `{}`) | Read-model PII cache |
| `Started`, `Completed` events | ❌ No | No personal data |
| `StepProgressed`, `WorkflowEvaluated` events | ❌ No | Workflow metadata only |
| `SubjectForgotten` event | ❌ No | Audit trail — contains only `subject_id`, not PII |
| `Modified` events captured *before* `CapturePerson` | ❌ No | Not yet linked to a person; not personal data under GDPR |
| `subject_id` in `PersonCaptured` payload | ❌ No | Opaque identifier — not PII |

---

## Further reading

- [`CRYPTO_SHREDDING_DESIGN.md`](CRYPTO_SHREDDING_DESIGN.md) — full design rationale,
  encryption scheme details, and future considerations
- [`PERSON_CAPTURE.md`](PERSON_CAPTURE.md) — `CapturePerson` command reference
- [`IMPLEMENTATION_SUMMARY.md`](IMPLEMENTATION_SUMMARY.md) — what was built across the four
  implementation phases
