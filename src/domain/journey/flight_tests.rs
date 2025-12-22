#![allow(clippy::too_many_lines)]
use cqrs_es::test::TestFramework;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::*;
use crate::services::decision_engine::GoRulesDecisionEngine;

type JourneyTester = TestFramework<Journey>;

#[test]
fn flight_booking_search_criteria() {
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
                    }
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
                        }
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
                    }
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
                        }
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
                    "passengers": [
                        {
                            "firstName": "John",
                            "lastName": "Doe",
                            "dateOfBirth": "1985-03-15"
                        },
                        {
                            "firstName": "Jane",
                            "lastName": "Doe",
                            "dateOfBirth": "1987-07-22"
                        }
                    ]
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "dateOfBirth": "1987-07-22"
                            }
                        ]
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();
    JourneyTester::with(services)
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
                    }
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
                        }
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
                    "passengers": [
                        {
                            "firstName": "John",
                            "lastName": "Doe",
                            "dateOfBirth": "1985-03-15"
                        },
                        {
                            "firstName": "Jane",
                            "lastName": "Doe"
                            // Missing dateOfBirth for second passenger
                        }
                    ]
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe"
                            }
                        ]
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
                    "passengers": [
                        {
                            "firstName": "John",
                            "lastName": "Doe",
                            "dateOfBirth": "1985-03-15"
                        },
                        {
                            "firstName": "Jane",
                            "lastName": "Doe",
                            "dateOfBirth": "1987-07-22"
                        },
                        {
                            "firstName": "Jimmy",
                            "lastName": "Doe",
                            "dateOfBirth": "2010-05-10"
                        }
                    ]
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "dateOfBirth": "1987-07-22"
                            },
                            {
                                "firstName": "Jimmy",
                                "lastName": "Doe",
                                "dateOfBirth": "2010-05-10"
                            }
                        ]
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
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    let id = Uuid::new_v4();

    JourneyTester::with(services)
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
            // Then capture detailed passenger info - should replace basic count with rich data
            data: (
                "passenger_details".to_string(),
                json!({
                    "passengers": [
                        {
                            "firstName": "John",
                            "lastName": "Doe",
                            "dateOfBirth": "1985-03-15"
                        },
                        {
                            "firstName": "Jane",
                            "lastName": "Doe",
                            "dateOfBirth": "1987-07-22"
                        }
                    ],
                    "preferences": {
                        "meals": ["vegetarian", "gluten-free"],
                        "seats": "aisle"
                    }
                }),
            ),
        })
        .then_expect_events(vec![
            JourneyEvent::Modified {
                form_data: Some((
                    "passenger_details".to_string(),
                    json!({
                        "passengers": [
                            {
                                "firstName": "John",
                                "lastName": "Doe",
                                "dateOfBirth": "1985-03-15"
                            },
                            {
                                "firstName": "Jane",
                                "lastName": "Doe",
                                "dateOfBirth": "1987-07-22"
                            }
                        ],
                        "preferences": {
                            "meals": ["vegetarian", "gluten-free"],
                            "seats": "aisle"
                        }
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
