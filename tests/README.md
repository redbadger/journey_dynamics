# API Examples (Hurl)

This directory contains [Hurl](https://hurl.dev) files for testing the Journey Dynamics API.

## Prerequisites

```bash
# Install Hurl
brew install hurl  # macOS

# Or download from https://hurl.dev

# Start the server
cargo run
```

## Files

| File | Description |
|------|-------------|
| `01-create-journey.hurl` | Create a new journey |
| `02-capture-data.hurl` | Capture step data |
| `03-capture-person.hurl` | Capture person contact data |
| `04-query-journey.hurl` | Query journey state |
| `05-complete-journey.hurl` | Complete a journey |
| `full-flight-booking.hurl` | Complete flight booking flow (all steps) |
| `error-cases.hurl` | Test error handling |

## Running Examples

### Individual files

```bash
# Create a journey (server auto-generates UUID)
hurl 01-create-journey.hurl

# For subsequent commands, pass the journey_id captured from create
hurl --variable journey_id=YOUR_UUID_HERE 02-capture-data.hurl
```

### Complete flow

```bash
# Run full flight booking - UUID auto-generated and captured between requests
hurl --test full-flight-booking.hurl
```

### All tests

```bash
# Run error cases (don't need a valid journey_id)
hurl --test error-cases.hurl

# Run full flow
hurl --test full-flight-booking.hurl
```

### With output

```bash
# Verbose output
hurl --verbose full-flight-booking.hurl --variable journey_id=$(uuidgen)

# Generate HTML report
hurl --test --report-html ./report full-flight-booking.hurl --variable journey_id=$(uuidgen)
```

## Command Reference

| Command | JSON Format | Notes |
|---------|-------------|-------|
| Start | Empty body OR `{"Start": {"id": "UUID"}}` | Empty body auto-generates UUID |
| Capture | `{"Capture": {"step": "name", "data": {...}}}` | |
| CapturePerson | `{"CapturePerson": {"name": "...", "email": "...", "phone": "..."}}` | |
| Complete | `{"Complete": null}` | |

## Response Codes

| Code | Meaning |
|------|---------|
| `201 Created` | Journey created |
| `204 No Content` | Command executed |
| `400 Bad Request` | Invalid command |
| `404 Not Found` | Journey not found |
