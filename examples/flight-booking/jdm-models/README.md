# Flight Booking Journey — JDM Model

This directory contains the JSON Decision Model (JDM) for the GoRules ZEN
engine that orchestrates the flight booking journey.

## flight-booking-orchestrator.jdm.json

**Main orchestrator that manages the entire booking journey flow.**

- **Input**: The full `shared_data` bag (merged plaintext + decrypted secret
  attributes), presented as a flat top-level object:
  ```json
  {
    "search": { "origin": "LHR", "destination": "JFK", ... },
    "booking": { "selectedOutboundFlight": { ... }, ... },
    "persons": {
      "passenger_0": { "firstName": "Alice", "passengerType": "adult", ... },
      "passenger_1": { "firstName": "Bob",   "passengerType": "adult", ... }
    }
  }
  ```
- **Processing**: Derives computed flags from the data (e.g. whether all
  passengers are complete) and determines the current phase and suggested
  actions.
- **Output**: `{ suggestedActions: [...], phase: "..." }`

### How passengers are consumed

`persons` is a **keyed object** (not an array). The orchestrator converts it
to an array for counting and validation:

```zen
persons_list = persons != null ? values(persons) : []
passenger_counts.found = len(persons_list)
computed.passengersComplete = ... and all(persons_list, #.passengerType != null)
```

The keys (`passenger_0`, `passenger_1`, …) are opaque to the rules — only the
values (passenger objects) matter.
