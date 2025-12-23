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
            step: "search_criteria".to_string(),
            data: json!({
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
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
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
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "outbound_flight_selection".to_string(),
                data: json!({
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
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "return_flight_selection".to_string(),
                data: json!({
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

#[test]
fn flight_booking_passenger_details() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "return_flight_selection".to_string(),
                data: json!({
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
                "passenger_details": [
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
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({
                    "passenger_details": [
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

#[test]
fn flight_booking_payment_capture() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
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
            },
        ])
        .when(JourneyCommand::Capture {
            step: "payment".to_string(),
            data: json!({"paymentStatus": "success"}),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "payment".to_string(),
                data: json!({"paymentStatus": "success"}),
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

#[test]
fn flight_booking_modify_search_criteria() {
    let id = Uuid::new_v4();

    JourneyTester::with(create_journey_services())
        .given(vec![
            JourneyEvent::Started { id },
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
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
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "search_criteria".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "search_criteria".to_string(),
            data: json!({
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
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "search_criteria".to_string(),
                data: json!({
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
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["flight_search_results".to_string()],
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
                step: "return_flight_selection".to_string(),
                data: json!({
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
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
            },
            JourneyEvent::StepProgressed {
                from_step: Some("outbound_flight_selection".to_string()),
                to_step: "return_flight_selection".to_string(),
            },
        ])
        .when(JourneyCommand::Capture {
            step: "passenger_details".to_string(),
            data: json!({
                "passenger_details": [
                    {
                        "firstName": "John",
                        "lastName": "Doe",
                        "dateOfBirth": "1985-03-15",
                        "passengerType": "adult"
                    },
                    {
                        "firstName": "Jane",
                        "lastName": "Doe"
                        // Missing dateOfBirth for second passenger
                    }
                ]
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({
                    "passenger_details": [
                        {
                            "firstName": "John",
                            "lastName": "Doe",
                            "dateOfBirth": "1985-03-15",
                            "passengerType": "adult"
                        },
                        {
                            "firstName": "Jane",
                            "lastName": "Doe"
                            // Missing dateOfBirth for second passenger
                        }
                    ]
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                // Should stay on passenger_details since not all passengers are complete
                suggested_actions: vec!["passenger_details".to_string()],
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
                step: "return_flight_selection".to_string(),
                data: json!({
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
                "passenger_details": [
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
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({
                    "passenger_details": [
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
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                // Should progress to seat_selection since all 3 passengers are complete
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

#[test]
fn test_invalid_data_rejected_with_schema_validation() {
    let id = Uuid::new_v4();

    // Test that invalid passenger details (malformed within group) are properly rejected
    JourneyTester::with(create_journey_services())
        .given(vec![JourneyEvent::Started { id }])
        .when(JourneyCommand::Capture {
            step: "passenger_details".to_string(),
            data: json!({
                "passenger_details": [
                    {
                        "firstName": "",
                        "lastName": "Doe"
                    }
                ]
            }),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                step: "passenger_details".to_string(),
                data: json!({
                    "passenger_details": [
                        {
                            "firstName": "",
                            "lastName": "Doe"
                        }
                    ]
                }),
            },
            JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["passenger_details".to_string()],
            },
            JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "passenger_details".to_string(),
            },
        ]);
}
