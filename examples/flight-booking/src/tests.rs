#![allow(clippy::too_many_lines)]
use std::collections::BTreeMap;
use std::sync::Arc;

use cqrs_es::test::TestFramework;
use serde_json::json;
use uuid::Uuid;

use journey_dynamics::{
    domain::{
        commands::JourneyCommand,
        events::{JourneyEvent, SecretPartitionData},
        flatten,
        journey::{Journey, JourneyError, JourneyServices},
        AttributePath,
    },
    services::{decision_engine::GoRulesDecisionEngine, schema_validator::JsonSchemaValidator},
};

type JourneyTester = TestFramework<Journey>;

fn create_journey_services() -> JourneyServices {
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/flight-booking-schema.json")).unwrap();
    let schema_validator = Arc::new(JsonSchemaValidator::new(&schema).unwrap());
    JourneyServices::new(
        decision_engine,
        schema_validator,
        Arc::new(crate::attribute_schema()),
    )
}

/// Build a `SetAttributes` command from a nested JSON value by flattening it.
fn set_attrs(data: &serde_json::Value) -> JourneyCommand {
    JourneyCommand::SetAttributes {
        changes: flatten(data),
    }
}

/// Build an `AttributesSet` plaintext event from a nested JSON value.
fn attrs_set(data: &serde_json::Value) -> JourneyEvent {
    JourneyEvent::AttributesSet {
        plaintext: flatten(data),
        secret_partitions: vec![],
    }
}

// ── Search criteria ───────────────────────────────────────────────────────────

#[test]
fn flight_booking_search_criteria() {
    let id = Uuid::new_v4();
    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(set_attrs(&search))
        .then_expect_events(vec![
            attrs_set(&search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
        ]);
}

// ── Outbound flight selection ─────────────────────────────────────────────────

#[test]
fn flight_booking_outbound_selection() {
    let id = Uuid::new_v4();
    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });
    let outbound = json!({
        "booking": {
            "selectedOutboundFlight": {
                "flightId": "BA123",
                "airline": "British Airways",
                "price": 450.00,
                "departure": "08:30",
                "arrival": "11:45"
            }
        }
    });

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            attrs_set(&search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
        ])
        .when(set_attrs(&outbound))
        .then_expect_events(vec![
            attrs_set(&outbound),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                phase: Some("selecting_return".to_string()),
            },
        ]);
}

// ── Return flight selection ───────────────────────────────────────────────────

#[test]
fn flight_booking_return_selection() {
    let id = Uuid::new_v4();
    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });
    let outbound_flight = json!({
        "flightId": "BA123", "airline": "British Airways",
        "price": 450.00, "departure": "08:30", "arrival": "11:45"
    });
    let return_flight = json!({
        "flightId": "BA456", "airline": "British Airways",
        "price": 480.00, "departure": "14:20", "arrival": "17:35"
    });
    let return_data = json!({ "booking": { "selectedReturnFlight": return_flight } });

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            attrs_set(&search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedOutboundFlight": outbound_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                phase: Some("selecting_return".to_string()),
            },
        ])
        .when(set_attrs(&return_data))
        .then_expect_events(vec![
            attrs_set(&return_data),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
                phase: Some("collecting_passengers".to_string()),
            },
        ]);
}

// ── Person capture (PII) ──────────────────────────────────────────────────────

/// `CapturePerson` stores name/email/phone encrypted under that subject's DEK.
/// No workflow evaluation is triggered (the decision engine sees only shared data).
#[test]
#[allow(deprecated)]
fn flight_booking_capture_person() {
    let id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::CapturePerson {
            person_ref: "passenger_0".to_string(),
            subject_id,
            name: "Alice Smith".to_string(),
            email: "alice@example.com".to_string(),
            phone: Some("+44-7700-900000".to_string()),
        })
        .then_expect_events(vec![
            JourneyEvent::SubjectCaptured {
                subject_id,
                email: "alice@example.com".to_string(),
            },
            JourneyEvent::SubjectBound {
                role_path: "persons/passenger_0".parse().unwrap(),
                subject_id,
            },
        ]);
}

/// `SetAttributes` for person details encrypts secret fields and stores
/// `passengerType` as plaintext under `persons/<ref>/passengerType`.
#[test]
fn flight_booking_capture_person_details() {
    let id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();

    let path = |s: &str| -> AttributePath { s.parse().unwrap() };

    let expected_secret = {
        let mut m = BTreeMap::new();
        m.insert(path("persons/passenger_0/firstName"), json!("Alice"));
        m.insert(path("persons/passenger_0/lastName"), json!("Smith"));
        m.insert(path("persons/passenger_0/dateOfBirth"), json!("1990-05-15"));
        m.insert(
            path("persons/passenger_0/passportNumber"),
            json!("GB123456789"),
        );
        m.insert(path("persons/passenger_0/nationality"), json!("GB"));
        m
    };
    let expected_plaintext = {
        let mut m = BTreeMap::new();
        m.insert(path("persons/passenger_0/passengerType"), json!("adult"));
        m
    };

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id,
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
        ])
        .when(JourneyCommand::SetAttributes {
            changes: {
                let mut m = BTreeMap::new();
                m.insert(path("persons/passenger_0/firstName"), json!("Alice"));
                m.insert(path("persons/passenger_0/lastName"), json!("Smith"));
                m.insert(path("persons/passenger_0/dateOfBirth"), json!("1990-05-15"));
                m.insert(
                    path("persons/passenger_0/passportNumber"),
                    json!("GB123456789"),
                );
                m.insert(path("persons/passenger_0/nationality"), json!("GB"));
                m.insert(path("persons/passenger_0/passengerType"), json!("adult"));
                m
            },
        })
        .then_expect_events(vec![
            JourneyEvent::AttributesSet {
                plaintext: expected_plaintext,
                secret_partitions: vec![SecretPartitionData {
                    role_path: "persons/passenger_0".parse().unwrap(),
                    subject_id,
                    changes: expected_secret,
                }],
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![],
                phase: Some("collecting_search".to_string()),
            },
        ]);
}

/// `SetAttributes` for a secret person field without a prior `CapturePerson`
/// returns `PersonNotFound`.
#[test]
fn flight_booking_capture_person_details_requires_prior_capture_person() {
    let id = Uuid::new_v4();
    let path = |s: &str| -> AttributePath { s.parse().unwrap() };

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::SetAttributes {
            changes: {
                let mut m = BTreeMap::new();
                m.insert(path("persons/passenger_0/firstName"), json!("Alice"));
                m
            },
        })
        .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
}

/// Two passengers with independent subject IDs.
#[test]
#[allow(deprecated)]
fn flight_booking_capture_two_passengers() {
    let id = Uuid::new_v4();
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id: subject_a,
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
        ])
        .when(JourneyCommand::CapturePerson {
            person_ref: "passenger_1".to_string(),
            subject_id: subject_b,
            name: "Bob Jones".to_string(),
            email: "bob@example.com".to_string(),
            phone: None,
        })
        .then_expect_events(vec![
            JourneyEvent::SubjectCaptured {
                subject_id: subject_b,
                email: "bob@example.com".to_string(),
            },
            JourneyEvent::SubjectBound {
                role_path: "persons/passenger_1".parse().unwrap(),
                subject_id: subject_b,
            },
        ]);
}

// ── Passenger readiness ───────────────────────────────────────────────────────

/// Once all passengers have `passengerType` set via `SetAttributes`, the JDM
/// advances to `collecting_payment` without any application-computed signal.
#[test]
fn flight_booking_passenger_details_ready_signal() {
    let id = Uuid::new_v4();
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();

    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });
    let outbound_flight = json!({
        "flightId": "BA123", "airline": "British Airways",
        "price": 450.00, "departure": "08:30", "arrival": "11:45"
    });
    let return_flight = json!({
        "flightId": "BA456", "airline": "British Airways",
        "price": 480.00, "departure": "14:20", "arrival": "17:35"
    });
    let passenger_types = json!({
        "persons": {
            "passenger_0": { "passengerType": "adult" },
            "passenger_1": { "passengerType": "adult" }
        }
    });

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            attrs_set(&search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedOutboundFlight": outbound_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                phase: Some("selecting_return".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedReturnFlight": return_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
                phase: Some("collecting_passengers".to_string()),
            },
            // PII captured for each passenger (encrypted at rest).
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id: subject_a,
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_1".to_string(),
                subject_id: subject_b,
                name: "Bob Jones".to_string(),
                email: "bob@example.com".to_string(),
                phone: None,
            },
        ])
        // Signal passenger readiness by setting passengerType for both via SetAttributes.
        // The JDM reads persons.*.passengerType directly — no application-computed count needed.
        .when(set_attrs(&passenger_types))
        .then_expect_events(vec![
            attrs_set(&passenger_types),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
                phase: Some("collecting_payment".to_string()),
            },
        ]);
}

/// Three passengers: all three must have `passengerType` set.
#[test]
fn flight_booking_three_passengers_ready_signal() {
    let id = Uuid::new_v4();
    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 1, "infants": 0 }
        }
    });
    let outbound_flight = json!({
        "flightId": "BA123", "airline": "British Airways",
        "price": 450.00, "departure": "08:30", "arrival": "11:45"
    });
    let return_flight = json!({
        "flightId": "BA456", "airline": "British Airways",
        "price": 480.00, "departure": "14:20", "arrival": "17:35"
    });
    let passenger_types = json!({
        "persons": {
            "passenger_0": { "passengerType": "adult" },
            "passenger_1": { "passengerType": "adult" },
            "passenger_2": { "passengerType": "child" }
        }
    });

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            attrs_set(&search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedOutboundFlight": outbound_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                phase: Some("selecting_return".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedReturnFlight": return_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
                phase: Some("collecting_passengers".to_string()),
            },
        ])
        .when(set_attrs(&passenger_types))
        .then_expect_events(vec![
            attrs_set(&passenger_types),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
                phase: Some("collecting_payment".to_string()),
            },
        ]);
}

/// Only one of two passengers has submitted their type — workflow stays at
/// `collecting_passengers`.
#[test]
fn flight_booking_passenger_details_not_ready() {
    let id = Uuid::new_v4();
    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });
    let outbound_flight = json!({
        "flightId": "BA123", "airline": "British Airways",
        "price": 450.00, "departure": "08:30", "arrival": "11:45"
    });
    let return_flight = json!({
        "flightId": "BA456", "airline": "British Airways",
        "price": 480.00, "departure": "14:20", "arrival": "17:35"
    });
    // Only one of two passengers has submitted their type.
    let partial_passengers = json!({
        "persons": { "passenger_0": { "passengerType": "adult" } }
    });

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            attrs_set(&search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedOutboundFlight": outbound_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                phase: Some("selecting_return".to_string()),
            },
            attrs_set(&json!({ "booking": { "selectedReturnFlight": return_flight } })),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
                phase: Some("collecting_passengers".to_string()),
            },
        ])
        .when(set_attrs(&partial_passengers))
        .then_expect_events(vec![
            attrs_set(&partial_passengers),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
                phase: Some("collecting_passengers".to_string()),
            },
        ]);
}

// ── Payment ───────────────────────────────────────────────────────────────────

#[test]
fn flight_booking_payment_capture() {
    let id = Uuid::new_v4();
    let search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });
    let payment = json!({ "booking": { "paymentStatus": "completed" } });

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }, attrs_set(&search)])
        .when(set_attrs(&payment))
        .then_expect_events(vec![
            attrs_set(&payment),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["booking_confirmation".to_string()],
                phase: Some("booking_confirmed".to_string()),
            },
        ]);
}

// ── Search modification ───────────────────────────────────────────────────────

#[test]
fn flight_booking_modify_search_criteria() {
    let id = Uuid::new_v4();
    let original_search = json!({
        "search": {
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-15",
            "returnDate": "2024-06-22",
            "passengers": { "adults": 2, "children": 0, "infants": 0 }
        }
    });
    let updated_search = json!({
        "search": {
            "tripType": "one-way",
            "origin": "LAX",
            "destination": "NYC",
            "departureDate": "2024-07-01",
            "passengers": { "adults": 1, "children": 0, "infants": 0 }
        }
    });

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            attrs_set(&original_search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
        ])
        .when(set_attrs(&updated_search))
        .then_expect_events(vec![
            attrs_set(&updated_search),
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
                phase: Some("selecting_outbound".to_string()),
            },
        ]);
}

// ── Secret partitions — multi-subject demonstration ───────────────────────────

/// Demonstrates that setting Secret person attributes (firstName, passportNumber)
/// via `SetAttributes` produces one encrypted partition per subject.
/// Requires prior `CapturePerson` so the subject lookup can resolve.
#[test]
fn flight_booking_set_person_secret_attributes() {
    let id = Uuid::new_v4();
    let subject_a = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
    let subject_b = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();

    let path = |s: &str| -> AttributePath { s.parse().unwrap() };

    let mut expected_secret_a = BTreeMap::new();
    expected_secret_a.insert(
        path("persons/passenger_0/passportNumber"),
        json!("GB111111"),
    );
    let mut expected_secret_b = BTreeMap::new();
    expected_secret_b.insert(
        path("persons/passenger_1/passportNumber"),
        json!("GB222222"),
    );

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id: subject_a,
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_1".to_string(),
                subject_id: subject_b,
                name: "Bob Jones".to_string(),
                email: "bob@example.com".to_string(),
                phone: None,
            },
        ])
        .when(JourneyCommand::SetAttributes {
            changes: {
                let mut m = BTreeMap::new();
                m.insert(
                    path("persons/passenger_0/passportNumber"),
                    json!("GB111111"),
                );
                m.insert(
                    path("persons/passenger_1/passportNumber"),
                    json!("GB222222"),
                );
                m
            },
        })
        .then_expect_events(vec![
            JourneyEvent::AttributesSet {
                plaintext: BTreeMap::new(),
                secret_partitions: vec![
                    SecretPartitionData {
                        role_path: "persons/passenger_0".parse().unwrap(),
                        subject_id: subject_a,
                        changes: expected_secret_a,
                    },
                    SecretPartitionData {
                        role_path: "persons/passenger_1".parse().unwrap(),
                        subject_id: subject_b,
                        changes: expected_secret_b,
                    },
                ],
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![],
                phase: Some("collecting_search".to_string()),
            },
        ]);
}

// ── Schema validation ─────────────────────────────────────────────────────────

/// `SetAttributes` with data that violates the JSON Schema is rejected.
#[test]
fn test_invalid_booking_data_rejected_with_schema_validation() {
    let id = Uuid::new_v4();

    // `paymentStatus` must be one of the PaymentStatus enum values; a free
    // string violates the schema.
    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(set_attrs(&json!({ "booking": { "paymentStatus": "not_a_valid_status" } })))
        .then_expect_error(JourneyError::InvalidData(
            "Schema validation failed: {\"paymentStatus\":\"not_a_valid_status\"} is not valid under any of the schemas listed in the 'anyOf' keyword"
                .to_string(),
        ));
}
