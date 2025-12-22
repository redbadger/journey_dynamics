# FlightBooking Schema-Based Data Handling

This document demonstrates how to use the generated JSON schema with `SchemaDataHandler` for robust, standards-compliant data management in the FlightBooking example.

## Overview

The FlightBooking example showcases a modern approach to data handling:

1. **`SchemaDataHandler`** - Generic data handler using JSON Merge Patch (RFC 7386)
2. **FlightBooking Extensions** - Domain-specific functionality via traits
3. **Generated JSON Schema** - Auto-generated schema for validation
4. **Type Safety** - Rust types that generate the schema

## Key Benefits

- **Standards Compliance**: Uses RFC 7386 JSON Merge Patch
- **Type Safety**: Schema generated from Rust types
- **Domain Separation**: Business logic stays in examples, not core framework
- **Schema Validation**: Runtime validation against JSON schema
- **Clean Architecture**: Clear separation between generic and domain-specific code

## Architecture

```
┌─────────────────────────────────────┐
│ journey_dynamics (core framework)   │
│ ├── SchemaDataHandler               │
│ │   ├── merge_form_data()           │
│ │   ├── apply_merge_patch()         │
│ │   ├── get_field()                 │
│ │   └── get_merged_data()           │
│ └── Journey                         │
│     └── data_handler: SchemaDataHandler │
└─────────────────────────────────────┘
                    │
                    │ extends
                    ▼
┌─────────────────────────────────────┐
│ flight-booking (domain example)     │
│ ├── Generated JSON Schema           │
│ ├── FlightBookingDataExt trait      │
│ │   ├── get_flight_booking_data()   │
│ │   ├── validate_requirements()     │
│ │   ├── can_transition_to_status()  │
│ │   └── validate_against_schema()   │
│ └── Domain Types                    │
│     ├── FlightBooking               │
│     ├── BookingStatus               │
│     └── PassengerDetail             │
└─────────────────────────────────────┘
```

## Quick Start

### 1. Generate the Schema

```bash
cd examples/flight-booking
cargo run --bin generate_schema
```

This creates `generated/flight-booking-schema.json` from your Rust types.

### 2. Basic Data Handling

```rust
use journey_dynamics::utils::SchemaDataHandler;
use flight_booking::flight_booking_data::FlightBookingDataExt;
use serde_json::json;

let mut handler = SchemaDataHandler::new();

// Merge search criteria
handler.merge_form_data("search", &json!({
    "tripType": "round-trip",
    "origin": "LHR",
    "destination": "JFK",
    "departureDate": "2024-06-01"
}))?;

// Merge passenger data
handler.merge_form_data("passengers", &json!({
    "passengers": {
        "adults": 2,
        "children": 0,
        "infants": 0,
        "total": 2
    }
}))?;

// Get structured FlightBooking data
let booking_data = handler.get_flight_booking_data();

// Validate business rules
handler.validate_flight_booking_requirements()?;

// Validate against schema
handler.validate_against_flight_booking_schema()?;
```

## JSON Merge Patch Semantics

Uses RFC 7386 standard for predictable merging:

```json
// Initial data
{
    "tripType": "round-trip",
    "passengers": {
        "adults": 1
    }
}

// Apply patch
{
    "origin": "LHR",
    "passengers": {
        "children": 1,
        "total": 2
    }
}

// Result (preserves existing, adds new)
{
    "tripType": "round-trip",  // ← preserved
    "origin": "LHR",           // ← added
    "passengers": {
        "adults": 1,           // ← preserved
        "children": 1,         // ← added
        "total": 2             // ← added
    }
}
```

## FlightBooking Extensions

### Business Logic Validation

```rust
// Check required fields
handler.validate_flight_booking_requirements()?;

// Validate workflow transitions
if handler.can_transition_to_status("passenger_details") {
    // Ready for next step
}
```

### Schema Validation

```rust
// Validate against generated JSON schema
match handler.validate_against_flight_booking_schema() {
    Ok(()) => println!("Valid FlightBooking data"),
    Err(e) => eprintln!("Schema validation failed: {e}"),
}
```

### Status Transitions

The extension provides intelligent status transition logic:

```rust
// Example: round-trip booking flow
handler.can_transition_to_status("flight_search_results");    // needs search criteria
handler.can_transition_to_status("outbound_flight_selection"); // needs search results  
handler.can_transition_to_status("return_flight_selection");   // needs outbound selection
handler.can_transition_to_status("passenger_details");         // needs both flights
```

## Integration with Journey

```rust
use journey_dynamics::domain::journey::Journey;

// Journey uses SchemaDataHandler internally
let journey = Journey::default();

// Access the data handler for domain operations
let handler = journey.get_data_handler();
let booking_data = handler.get_flight_booking_data();

// Check workflow readiness
if handler.can_transition_to_status("booking_confirmation") {
    // Ready to confirm booking
}
```

## Testing

Comprehensive test coverage included:

```bash
# Test core functionality
cargo test schema_data_handler

# Test FlightBooking extensions
cargo test flight_booking_data  

# Test specific features
cargo test test_schema_validation
cargo test test_status_transitions

# Run all tests
cargo test
```

## File Structure

```
examples/flight-booking/
├── src/
│   ├── lib.rs                    # Domain types with JsonSchema derives
│   ├── flight_booking_data.rs    # FlightBookingDataExt trait
│   └── bin/
│       └── generate_schema.rs    # Schema generation utility
├── generated/
│   └── flight-booking-schema.json # Auto-generated JSON schema
└── SCHEMA_USAGE.md              # This documentation
```

## Key Design Principles

1. **Generic Core**: `SchemaDataHandler` has no domain knowledge
2. **Domain Extensions**: Business logic via traits in examples
3. **Type-Driven**: Schema generated from Rust types
4. **Standards-Based**: RFC 7386 JSON Merge Patch
5. **Testable**: Comprehensive test coverage
6. **Maintainable**: Clear separation of concerns

## Dependencies

- `jsonschema = "0.37.4"` - Schema validation
- `json-patch = "4.1.0"` - JSON Merge Patch implementation  
- `schemars = "1.1.0"` - JSON Schema generation
- `serde_json = "1.0"` - JSON handling

This approach provides a robust, maintainable foundation for data handling while keeping the core framework clean and domain-agnostic.