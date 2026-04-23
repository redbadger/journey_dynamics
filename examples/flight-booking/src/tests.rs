#![allow(clippy::too_many_lines)]
use cqrs_es::test::TestFramework;

use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use journey_dynamics::{
    domain::{
        commands::JourneyCommand,
        events::JourneyEvent,
        journey::{Journey, JourneyError, JourneyServices},
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
    JourneyServices::new(decision_engine, schema_validator)
}

// ── Shared non-PII steps ─────────────────────────────────────────────────────

#[test]
fn flight_booking_search_criteria() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::Capture {
            step: "search_criteria".to_string(),
            data: json!({
                "search": {
                    "tripType": "round-trip",
                    "origin": "LHR",
                    "destination": "JFK",
                    "departureDate": "2024-06-15",
                    "returnDate": "2024-06-22",
                    "passengers": {
                        "total": 2,
                        "adults": 2,
                        "children": 0,
                        "infants": 0
                    }
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ]);
}

#[test]
fn flight_booking_outbound_selection() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "outbound_flight_selection".to_string(),
            data: json!({
                "search": {
                    "tripType": "round-trip",
                    "origin": "LHR",
                    "destination": "JFK",
                    "departureDate": "2024-06-15",
                    "returnDate": "2024-06-22",
                    "passengers": {
                        "total": 2,
                        "adults": 2,
                        "children": 0,
                        "infants": 0
                    }
                },
                "booking": {
                    "selectedOutboundFlight": {
                        "flightId": "BA123",
                        "airline": "British Airways",
                        "price": 450.00,
                        "departure": "08:30",
                        "arrival": "11:45"
                    }
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "outbound_flight_selection".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    },
                    "booking": {
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("search_criteria".to_string()),
                to_step: "outbound_flight_selection".to_string(),
            },
        ]);
}

#[test]
fn flight_booking_return_selection() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "outbound_flight_selection".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    },
                    "booking": {
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("search_criteria".to_string()),
                to_step: "outbound_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "return_flight_selection".to_string(),
            data: json!({
                "search": {
                    "tripType": "round-trip",
                    "origin": "LHR",
                    "destination": "JFK",
                    "departureDate": "2024-06-15",
                    "returnDate": "2024-06-22",
                    "passengers": {
                        "total": 2,
                        "adults": 2,
                        "children": 0,
                        "infants": 0
                    }
                },
                "booking": {
                    "selectedOutboundFlight": {
                        "flightId": "BA123",
                        "airline": "British Airways",
                        "price": 450.00,
                        "departure": "08:30",
                        "arrival": "11:45"
                    },
                    "selectedReturnFlight": {
                        "flightId": "BA456",
                        "airline": "British Airways",
                        "price": 480.00,
                        "departure": "14:20",
                        "arrival": "17:35"
                    }
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "return_flight_selection".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    },
                    "booking": {
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        },
                        "selectedReturnFlight": {
                            "flightId": "BA456",
                            "airline": "British Airways",
                            "price": 480.00,
                            "departure": "14:20",
                            "arrival": "17:35"
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "passenger_details".to_string(),
                    "outbound_flight_selection".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ]);
}

// ── Person capture (PII) ─────────────────────────────────────────────────────

/// Demonstrates the correct way to capture a passenger's identity fields.
/// `CapturePerson` stores name/email/phone encrypted under that subject's DEK.
/// No workflow evaluation is triggered (the decision engine sees only shared data).
#[test]
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
        .then_expect_events(vec![JourneyEvent::PersonCaptured {
            person_ref: "passenger_0".to_string(),
            subject_id,
            name: "Alice Smith".to_string(),
            email: "alice@example.com".to_string(),
            phone: Some("+44-7700-900000".to_string()),
        }]);
}

/// Demonstrates the correct way to capture per-passenger PII details
/// (passport number, date of birth, nationality).
/// `CapturePersonDetails` requires a prior `CapturePerson` for the same `person_ref`.
#[test]
fn flight_booking_capture_person_details() {
    let id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();

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
        .when(JourneyCommand::CapturePersonDetails {
            person_ref: "passenger_0".to_string(),
            data: json!({
                "firstName":      "Alice",
                "lastName":       "Smith",
                "dateOfBirth":    "1990-05-15",
                "passportNumber": "GB123456789",
                "nationality":    "GB",
                "passengerType":  "adult"
            }),
        })
        .then_expect_events(vec![JourneyEvent::PersonDetailsUpdated {
            person_ref: "passenger_0".to_string(),
            subject_id,
            data: json!({
                "firstName":      "Alice",
                "lastName":       "Smith",
                "dateOfBirth":    "1990-05-15",
                "passportNumber": "GB123456789",
                "nationality":    "GB",
                "passengerType":  "adult"
            }),
        }]);
}

/// `CapturePersonDetails` MUST be preceded by `CapturePerson` for the same `person_ref`.
/// Calling it without a prior `CapturePerson` returns `PersonNotFound`.
#[test]
fn flight_booking_capture_person_details_requires_prior_capture_person() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::CapturePersonDetails {
            person_ref: "passenger_0".to_string(),
            data: json!({
                "firstName":   "Alice",
                "lastName":    "Smith",
                "dateOfBirth": "1990-05-15"
            }),
        })
        .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
}

/// Two passengers with independent subject IDs in the same journey.
/// Each `CapturePerson` + `CapturePersonDetails` pair is entirely independent.
#[test]
fn flight_booking_capture_two_passengers() {
    let id = Uuid::new_v4();
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();

    // Capture identity for passenger 0.
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
        .then_expect_events(vec![JourneyEvent::PersonCaptured {
            person_ref: "passenger_1".to_string(),
            subject_id: subject_b,
            name: "Bob Jones".to_string(),
            email: "bob@example.com".to_string(),
            phone: None,
        }]);
}

// ── Passenger-ready workflow signal ──────────────────────────────────────────

/// After collecting each passenger's PII via `CapturePerson` + `CapturePersonDetails`,
/// the application sends a `Capture` with `booking.passengersReady` to signal the decision
/// engine that passenger details are complete. The PII itself never flows through `Capture`.
#[test]
fn flight_booking_passenger_details_ready_signal() {
    let id = Uuid::new_v4();
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "return_flight_selection".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    },
                    "booking": {
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        },
                        "selectedReturnFlight": {
                            "flightId": "BA456",
                            "airline": "British Airways",
                            "price": 480.00,
                            "departure": "14:20",
                            "arrival": "17:35"
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "passenger_details".to_string(),
                    "outbound_flight_selection".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
            // PII captured for passenger 0 (encrypted in the event store).
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id: subject_a,
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
            JourneyEvent::PersonDetailsUpdated {
                person_ref: "passenger_0".to_string(),
                subject_id: subject_a,
                data: json!({
                    "firstName":   "Alice",
                    "lastName":    "Smith",
                    "dateOfBirth": "1990-05-15",
                    "passengerType": "adult"
                }),
            },
            // PII captured for passenger 1 (encrypted under a different DEK).
            JourneyEvent::PersonCaptured {
                person_ref: "passenger_1".to_string(),
                subject_id: subject_b,
                name: "Bob Jones".to_string(),
                email: "bob@example.com".to_string(),
                phone: None,
            },
            JourneyEvent::PersonDetailsUpdated {
                person_ref: "passenger_1".to_string(),
                subject_id: subject_b,
                data: json!({
                    "firstName":   "Bob",
                    "lastName":    "Jones",
                    "dateOfBirth": "1988-11-20",
                    "passengerType": "adult"
                }),
            },
        ])
        // Non-PII signal: both passengers are ready. The decision engine uses this to
        // advance the workflow — it never sees the encrypted PII fields above.
        .when(JourneyCommand::Capture {
            step: "passenger_details".to_string(),
            data: json!({
                "booking": {
                    "passengersReady": 2
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({
                    "booking": {
                        "passengersReady": 2
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("return_flight_selection".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

/// Three passengers: signal `passengersReady: 3` once all details are captured.
#[test]
fn flight_booking_three_passengers_ready_signal() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "return_flight_selection".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 3,
                            "adults": 2,
                            "children": 1,
                            "infants": 0
                        }
                    },
                    "booking": {
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        },
                        "selectedReturnFlight": {
                            "flightId": "BA456",
                            "airline": "British Airways",
                            "price": 480.00,
                            "departure": "14:20",
                            "arrival": "17:35"
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "passenger_details".to_string(),
            data: json!({
                "booking": {
                    "passengersReady": 3
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({
                    "booking": {
                        "passengersReady": 3
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("return_flight_selection".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

/// When `passengersReady` is absent the decision engine keeps the workflow at
/// `passenger_details` — prompting the user to complete passenger capture.
#[test]
fn flight_booking_passenger_details_not_ready() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "return_flight_selection".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    },
                    "booking": {
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        },
                        "selectedReturnFlight": {
                            "flightId": "BA456",
                            "airline": "British Airways",
                            "price": 480.00,
                            "departure": "14:20",
                            "arrival": "17:35"
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "passenger_details".to_string(),
                    "outbound_flight_selection".to_string(),
                ],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ])
        // No passengersReady signal yet — workflow should stay at passenger_details.
        .when(JourneyCommand::Capture {
            step: "passenger_details".to_string(),
            data: json!({
                "booking": {}
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({ "booking": {} }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("return_flight_selection".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

// ── Payment ───────────────────────────────────────────────────────────────────

#[test]
fn flight_booking_payment_capture() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    }
                }),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "payment".to_string(),
            data: json!({
                "booking": {
                    "paymentStatus": "completed"
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "payment".to_string(),
                data: json!({
                    "booking": {
                        "paymentStatus": "completed"
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["booking_confirmation".to_string()],
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "payment".to_string(),
            },
        ]);
}

// ── Search modification ───────────────────────────────────────────────────────

#[test]
fn flight_booking_modify_search_criteria() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
                    "search": {
                        "tripType": "round-trip",
                        "origin": "LHR",
                        "destination": "JFK",
                        "departureDate": "2024-06-15",
                        "returnDate": "2024-06-22",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        }
                    }
                }),
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "search_criteria".to_string(),
            data: json!({
                "search": {
                    "tripType": "one-way",
                    "origin": "LAX",
                    "destination": "NYC",
                    "departureDate": "2024-07-01",
                    "passengers": {
                        "total": 1,
                        "adults": 1,
                        "children": 0,
                        "infants": 0
                    }
                }
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
                    "search": {
                        "tripType": "one-way",
                        "origin": "LAX",
                        "destination": "NYC",
                        "departureDate": "2024-07-01",
                        "passengers": {
                            "total": 1,
                            "adults": 1,
                            "children": 0,
                            "infants": 0
                        }
                    }
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
            },
        ]);
}

// ── Schema validation ─────────────────────────────────────────────────────────

/// Shared booking data with an invalid type is rejected by schema validation.
/// Note: PII (passenger names, DoB, passport numbers) is NOT validated here because
/// it no longer flows through `Capture` — it goes through `CapturePersonDetails`.
#[test]
fn test_invalid_booking_data_rejected_with_schema_validation() {
    let id = Uuid::new_v4();

    // passengersReady must be an integer (or null); sending a string violates the schema.
    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::Capture {
            step: "passenger_details".to_string(),
            data: json!({
                "booking": {
                    "passengersReady": "two"
                }
            }),
        })
        .then_expect_error(JourneyError::InvalidData(
            "Schema validation failed: {\"passengersReady\":\"two\"} is not valid under any of the schemas listed in the 'anyOf' keyword"
                .to_string(),
        ));
}
