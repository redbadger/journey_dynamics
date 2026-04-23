# API Tests (Hurl)

This directory contains [Hurl](https://hurl.dev) files for testing the Journey Dynamics API.

All files use a `{{host}}` variable for the base URL, so they can be pointed at any
environment without modification. Hurl picks this up automatically from the `HURL_host`
environment variable — no `--variable` flag needed.

## Prerequisites

```bash
# Install Hurl
brew install hurl  # macOS
# or download from https://hurl.dev

# Start the server (requires DATABASE_URL and JOURNEY_KEK to be set)
cargo run
```

## Files

| File | Description | Self-contained |
|------|-------------|:--------------:|
| `01-create-journey.hurl` | Create a new journey | ✓ |
| `02-capture-data.hurl` | Capture step data (requires `journey_id`) | |
| `03-capture-person.hurl` | Capture person contact data (requires `journey_id`) | |
| `04-query-journey.hurl` | Query journey state (requires `journey_id`) | |
| `05-complete-journey.hurl` | Complete a journey (requires `journey_id`) | |
| `full-flight-booking.hurl` | Complete flight booking flow, end-to-end | ✓ |
| `full-flight-booking_with_shredding.hurl` | Complete flow including GDPR crypto-shredding | ✓ |
| `error-cases.hurl` | Error handling (404s, 400s) | ✓ |

### Step-by-step files (01–05)

These are tutorial examples illustrating each API call individually. Files `02`–`05`
depend on a `journey_id` captured from `01`, so they must be supplied a UUID explicitly:

```bash
export HURL_host=http://localhost:3030

# Step 1 – create a journey and note the UUID printed in the Location header
hurl --include tests/01-create-journey.hurl

# Steps 2–5 – pass the UUID from the previous step
export HURL_journey_id=<UUID>

hurl tests/02-capture-data.hurl
hurl tests/03-capture-person.hurl
hurl tests/04-query-journey.hurl
hurl tests/05-complete-journey.hurl
```

### Automated test suite

The three self-contained files are the ones run in CI (see `just test-hurl`):

```bash
export HURL_host=http://localhost:3030
hurl --test tests/error-cases.hurl tests/full-flight-booking.hurl tests/full-flight-booking_with_shredding.hurl
```

### With an HTML report

```bash
export HURL_host=http://localhost:3030
hurl --test --report-html ./hurl-report \
  tests/error-cases.hurl \
  tests/full-flight-booking.hurl \
  tests/full-flight-booking_with_shredding.hurl
```

## Command Reference

| Command | JSON body | Notes |
|---------|-----------|-------|
| Create journey | _(empty)_ | `POST /journeys` — server generates a UUID |
| Capture | `{"Capture": {"step": "name", "data": {...}}}` | `POST /journeys/{id}` |
| CapturePerson | `{"CapturePerson": {"subject_id": "UUID", "name": "...", "email": "...", "phone": "..."}}` | `POST /journeys/{id}` |
| Complete | `{"Complete": null}` | `POST /journeys/{id}` |
| Shred subject | _(none)_ | `DELETE /subjects/{subject_id}` — erases all PII for that subject |

## Response Codes

| Code | Meaning |
|------|---------|
| `201 Created` | Journey created |
| `204 No Content` | Command accepted |
| `400 Bad Request` | Invalid command or journey not found |
| `404 Not Found` | Journey not found (query only) |
