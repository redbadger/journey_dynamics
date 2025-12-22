#![allow(clippy::too_many_lines)]
use cqrs_es::test::TestFramework;

use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use journey_dynamics::{
    domain::{
        commands::JourneyCommand,
        events::JourneyEvent,
        journey::{Journey, JourneyServices},
    },
    services::{
        decision_engine::GoRulesDecisionEngine,
        schema_validator::{JsonSchemaValidator, NoOpValidator},
    },
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

fn create_journey_services_with_no_validation() -> JourneyServices {
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let schema_validator = Arc::new(NoOpValidator);
    JourneyServices::new(decision_engine, schema_validator)
}

#[test]
fn flight_booking_search_criteria() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::Capture {
            data: (
                "search_criteria".to_string(),
                json!({
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
                    },
                    "status": "search_criteria"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
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
                        },
                        "status": "search_criteria"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["flight_search_results".to_string()],
                primary_next_step: Some("flight_search_results".to_string()),
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
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
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
                        },
                        "status": "search_criteria"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["flight_search_results".to_string()],
                primary_next_step: None,
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            data: (
                "outbound_flight_selection".to_string(),
                json!({
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
                    },
                    "selectedOutboundFlight": {
                        "flightId": "BA123",
                        "airline": "British Airways",
                        "price": 450.00,
                        "departure": "08:30",
                        "arrival": "11:45"
                    }
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "outbound_flight_selection".to_string(),
                    json!({
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
                        },
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        }
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                primary_next_step: Some("return_flight_selection".to_string()),
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
                form_data: Some((
                    "outbound_flight_selection".to_string(),
                    json!({
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
                        },
                        "selectedOutboundFlight": {
                            "flightId": "BA123",
                            "airline": "British Airways",
                            "price": 450.00,
                            "departure": "08:30",
                            "arrival": "11:45"
                        }
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![
                    "return_flight_selection".to_string(),
                    "flight_search_results".to_string(),
                ],
                primary_next_step: Some("return_flight_selection".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("search_criteria".to_string()),
                to_step: "outbound_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            data: (
                "return_flight_selection".to_string(),
                json!({
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
                    },
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
                    },
                    "status": "return_flight_selection"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "return_flight_selection".to_string(),
                    json!({
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
                        },
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
                        },
                        "status": "return_flight_selection"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![
                    "passenger_details".to_string(),
                    "outbound_flight_selection".to_string(),
                ],
                primary_next_step: Some("passenger_details".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ]);
}

#[test]
fn flight_booking_passenger_details() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                form_data: Some((
                    "return_flight_selection".to_string(),
                    json!({
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
                        },
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
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["passenger_details".to_string()],
                primary_next_step: Some("passenger_details".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            data: (
                "passenger_details".to_string(),
                json!({
                    "passengers": {
                        "details": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15",
                                "passengerType": "adult"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "dateOfBirth": "1987-07-22",
                                "passengerType": "adult"
                            }
                        ]
                    },
                    "status": "passenger_details"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": {
                            "details": [
                                {
                                    "firstName": "John",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1985-03-15",
                                    "passengerType": "adult"
                                },
                                {
                                    "firstName": "Jane",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1987-07-22",
                                    "passengerType": "adult"
                                }
                            ]
                        },
                        "status": "passenger_details"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
                primary_next_step: Some("seat_selection".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("return_flight_selection".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

#[test]
fn flight_booking_payment_capture() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
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
                        },
                        "status": "search_criteria"
                    }),
                )),
            },
        ])
        .when(JourneyCommand::Capture {
            data: ("payment".to_string(), json!({"paymentStatus": "success"})),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some(("payment".to_string(), json!({"paymentStatus": "success"}))),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["booking_confirmation".to_string()],
                primary_next_step: Some("booking_confirmation".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "payment".to_string(),
            },
        ]);
}

#[test]
fn flight_booking_modify_search_criteria() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
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
                    }),
                )),
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            data: (
                "search_criteria".to_string(),
                json!({
                    "tripType": "one-way",
                    "origin": "LAX",
                    "destination": "NYC",
                    "departureDate": "2024-07-01",
                    "passengers": {
                        "total": 1,
                        "adults": 1,
                        "children": 0,
                        "infants": 0
                    },
                    "status": "search_criteria"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
                        "tripType": "one-way",
                        "origin": "LAX",
                        "destination": "NYC",
                        "departureDate": "2024-07-01",
                        "passengers": {
                            "total": 1,
                            "adults": 1,
                            "children": 0,
                            "infants": 0
                        },
                        "status": "search_criteria"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["flight_search_results".to_string()],
                primary_next_step: Some("flight_search_results".to_string()),
            },
        ]);
}

#[test]
fn flight_booking_passenger_details_incomplete() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services_with_no_validation())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                form_data: Some((
                    "return_flight_selection".to_string(),
                    json!({
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
                        },
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
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["passenger_details".to_string()],
                primary_next_step: Some("passenger_details".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            data: (
                "passenger_details".to_string(),
                json!({
                    "passengers": {
                        "details": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15",
                                "passengerType": "adult"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "passengerType": "adult"
                                // Missing dateOfBirth for second passenger
                            }
                        ]
                    },
                    "status": "passenger_details"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": {
                            "details": [
                                {
                                    "firstName": "John",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1985-03-15",
                                    "passengerType": "adult"
                                },
                                {
                                    "firstName": "Jane",
                                    "lastName": "Doe",
                                    "passengerType": "adult"
                                }
                            ]
                        },
                        "status": "passenger_details"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                // Should stay on passenger_details since not all passengers are complete
                available_actions: vec!["passenger_details".to_string()],
                primary_next_step: Some("passenger_details".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("return_flight_selection".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

#[test]
fn flight_booking_passenger_details_three_passengers() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                form_data: Some((
                    "return_flight_selection".to_string(),
                    json!({
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
                        },
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
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
                primary_next_step: Some("seat_selection".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            data: (
                "passenger_details".to_string(),
                json!({
                    "passengers": {
                        "details": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15",
                                "passengerType": "adult"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "dateOfBirth": "1987-07-22",
                                "passengerType": "adult"
                            },
                            {
                                "firstName": "Jimmy",
                                "lastName": "Doe",
                                "dateOfBirth": "2010-05-10",
                                "passengerType": "child"
                            }
                        ]
                    },
                    "status": "passenger_details"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": {
                            "details": [
                                {
                                    "firstName": "John",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1985-03-15",
                                    "passengerType": "adult"
                                },
                                {
                                    "firstName": "Jane",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1987-07-22",
                                    "passengerType": "adult"
                                },
                                {
                                    "firstName": "Jimmy",
                                    "lastName": "Doe",
                                    "dateOfBirth": "2010-05-10",
                                    "passengerType": "child"
                                }
                            ]
                        },
                        "status": "passenger_details"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                // Should progress to seat_selection since all 3 passengers are complete
                available_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
                primary_next_step: Some("seat_selection".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("return_flight_selection".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

#[test]
fn test_generic_data_merging() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            // First, capture basic passenger count info
            JourneyEvent::Modified {
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
                        "tripType": "round-trip",
                        "passengers": {
                            "total": 2,
                            "adults": 2,
                            "children": 0,
                            "infants": 0
                        },
                        "status": "search_criteria"
                    }),
                )),
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            // Then capture detailed passenger info - should replace basic count with rich data
            data: (
                "passenger_details".to_string(),
                json!({
                    "passengers": {
                        "details": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15",
                                "passengerType": "adult"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "dateOfBirth": "1987-07-22",
                                "passengerType": "adult"
                            }
                        ]
                    },
                    "preferences": {
                        "meals": ["vegetarian", "gluten-free"],
                        "seats": "aisle"
                    },
                    "status": "passenger_details"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": {
                            "details": [
                                {
                                    "firstName": "John",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1985-03-15",
                                    "passengerType": "adult"
                                },
                                {
                                    "firstName": "Jane",
                                    "lastName": "Doe",
                                    "dateOfBirth": "1987-07-22",
                                    "passengerType": "adult"
                                }
                            ]
                        },
                        "preferences": {
                            "meals": ["vegetarian", "gluten-free"],
                            "seats": "aisle"
                        },
                        "status": "passenger_details"
                    }),
                )),
            },
            // The generic merging should have replaced passenger count with detailed array
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![
                    "seat_selection".to_string(),
                    "passenger_details".to_string(),
                ],
                primary_next_step: Some("seat_selection".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: Some("search_criteria".to_string()),
                to_step: "passenger_details".to_string(),
            },
        ]);
}

#[test]
fn test_invalid_data_rejected_with_schema_validation() {
    let id = Uuid::new_v4();

    // Test that invalid data (missing required fields) is now properly rejected
    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::Capture {
            data: (
                "search_criteria".to_string(),
                json!({
                    "origin": "LHR",
                    "destination": "JFK"
                    // Missing required fields: tripType, passengers
                }),
            ),
        })
        .then_expect_error(
            journey_dynamics::domain::journey::JourneyError::InvalidData(
                "Schema validation failed: \"tripType\" is a required property, \"passengers\" is a required property, \"status\" is a required property".to_string(),
            ),
        );
}

#[test]
fn test_valid_data_passes_schema_validation() {
    use crate::flight_booking_data::FlightBookingDataExt;

    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::Capture {
            data: (
                "search_criteria".to_string(),
                json!({
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
                    },
                    "status": "search_criteria"
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "search_criteria".to_string(),
                    json!({
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
                        },
                        "status": "search_criteria"
                    }),
                )),
            },
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["flight_search_results".to_string()],
                primary_next_step: Some("flight_search_results".to_string()),
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ]);

    // Verify that the data would pass FlightBooking validation
    let mut handler = journey_dynamics::utils::SchemaDataHandler::new();
    handler
        .merge_form_data(
            "search_criteria",
            &json!({
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
                },
                "status": "search_criteria"
            }),
        )
        .unwrap();

    // This validation should pass
    assert!(handler.validate_flight_booking_requirements().is_ok());
}
