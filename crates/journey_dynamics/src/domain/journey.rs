use std::sync::Arc;

use crate::domain::events::JourneyEvent;
use crate::services::schema_validator::SchemaValidator;
use crate::{domain::commands::JourneyCommand, services::decision_engine::DecisionEngine};
use async_trait::async_trait;
use cqrs_es::Aggregate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    accumulated_data: Value,
    current_step: Option<String>,
    latest_workflow_decision: Option<WorkflowDecisionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionState {
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum JourneyState {
    #[default]
    InProgress,
    Complete,
}

#[async_trait]
impl Aggregate for Journey {
    type Command = JourneyCommand;
    type Event = JourneyEvent;
    type Error = JourneyError;
    type Services = JourneyServices;

    // This identifier should be unique to the system.
    fn aggregate_type() -> String {
        "Journey".to_string()
    }

    // The aggregate logic goes here. Note that this will be the _bulk_ of a CQRS system
    // so expect to use helper functions elsewhere to keep the code clean.
    async fn handle(
        &self,
        command: Self::Command,
        services: &Self::Services,
    ) -> Result<Vec<Self::Event>, Self::Error> {
        match command {
            JourneyCommand::Start { id } => {
                if self.id == id {
                    Err(JourneyError::AlreadyStarted)
                } else {
                    Ok(vec![JourneyEvent::Started { id }])
                }
            }
            JourneyCommand::CapturePerson { name, email, phone } => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    Ok(vec![JourneyEvent::PersonCaptured { name, email, phone }])
                }
            }
            JourneyCommand::Capture { step, data } => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    // Validate against schema using the schema validator service
                    if let Err(e) = services.schema_validator().validate(&data) {
                        return Err(JourneyError::InvalidData(e.to_string()));
                    }

                    // Determine if this represents a step transition
                    let is_step_transition = self.current_step.as_ref() != Some(&step);

                    // Prepare journey state for decision engine evaluation
                    let mut journey_for_eval = self.clone();
                    if is_step_transition {
                        journey_for_eval.current_step = Some(step.clone());
                    }

                    let decision = services
                        .decision_engine()
                        .evaluate_next_steps(&journey_for_eval, &step, &data)
                        .await
                        .map_err(|e| JourneyError::DecisionEngineError(e.to_string()))?;

                    let mut events = vec![JourneyEvent::Modified {
                        step: step.clone(),
                        data: data.clone(),
                    }];

                    events.push(JourneyEvent::WorkflowEvaluated {
                        suggested_actions: decision.suggested_actions,
                    });

                    if is_step_transition {
                        events.push(JourneyEvent::StepProgressed {
                            from_step: self.current_step.clone(),
                            to_step: step.clone(),
                        });
                    }

                    Ok(events)
                }
            }
            JourneyCommand::Complete => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    Ok(vec![JourneyEvent::Completed])
                }
            }
        }
    }

    fn apply(&mut self, event: Self::Event) {
        match event {
            JourneyEvent::Started { id } => {
                self.id = id;
                self.state = JourneyState::InProgress;
            }
            JourneyEvent::Modified { step: _, data } => {
                json_patch::merge(&mut self.accumulated_data, &data);
            }
            JourneyEvent::WorkflowEvaluated { suggested_actions } => {
                self.latest_workflow_decision = Some(WorkflowDecisionState { suggested_actions });
            }
            JourneyEvent::PersonCaptured { .. } => {
                // Person data is projected to read model tables
                // No state change needed in the aggregate
            }
            JourneyEvent::StepProgressed {
                from_step: _,
                to_step,
            } => {
                self.current_step = Some(to_step.clone());
            }
            JourneyEvent::Completed => {
                self.state = JourneyState::Complete;
            }
        }
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum JourneyError {
    #[error("Journey not found")]
    NotFound,
    #[error("Journey already opened")]
    AlreadyStarted,
    #[error("Journey already closed")]
    AlreadyCompleted,
    #[error("Decision engine error: {0}")]
    DecisionEngineError(String),
    #[error("Invalid data: {0}")]
    InvalidData(String),
}

pub struct JourneyServices {
    decision_engine: Arc<dyn DecisionEngine>,
    schema_validator: Arc<dyn SchemaValidator>,
}

impl JourneyServices {
    pub fn new(
        decision_engine: Arc<dyn DecisionEngine>,
        schema_validator: Arc<dyn SchemaValidator>,
    ) -> Self {
        Self {
            decision_engine,
            schema_validator,
        }
    }

    #[must_use]
    pub fn decision_engine(&self) -> &Arc<dyn DecisionEngine> {
        &self.decision_engine
    }

    #[must_use]
    pub fn schema_validator(&self) -> &Arc<dyn SchemaValidator> {
        &self.schema_validator
    }
}

impl Journey {
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn state(&self) -> JourneyState {
        self.state
    }

    #[must_use]
    pub fn accumulated_data(&self) -> &Value {
        &self.accumulated_data
    }

    #[must_use]
    pub fn current_step(&self) -> Option<&String> {
        self.current_step.as_ref()
    }

    #[must_use]
    pub fn latest_workflow_decision(&self) -> Option<&WorkflowDecisionState> {
        self.latest_workflow_decision.as_ref()
    }
}

impl Default for Journey {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            accumulated_data: json!({}),
            current_step: None,
            latest_workflow_decision: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]
    use cqrs_es::test::TestFramework;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    use super::*;
    use crate::services::decision_engine::SimpleDecisionEngine;
    use crate::services::schema_validator::JsonSchemaValidator;

    type JourneyTester = TestFramework<Journey>;

    fn create_test_schema_validator() -> Arc<JsonSchemaValidator> {
        let schema = json!({
            "oneOf": [
                {
                    "type": "string"
                },
                {
                    "type": "object",
                    "properties": {
                        "alpha": { "type": "number" },
                        "beta": { "type": "string" },
                        "step": { "type": "string" },
                        "email": { "type": "string", "format": "email" },
                        "name": { "type": "string" },
                        "first_name": { "type": "string" }
                    },
                    "additionalProperties": true
                }
            ]
        });
        Arc::new(JsonSchemaValidator::new(&schema).unwrap())
    }

    #[test]
    fn start_a_journey() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::Start { id })
            .then_expect_events(vec![JourneyEvent::Started { id }]);
    }

    #[test]
    fn modify_journey() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                step: "first_name".to_string(),
                data: json!("Joe"),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    step: "first_name".to_string(),
                    data: json!("Joe"),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "first_name".to_string(),
                },
            ]);
    }

    #[test]
    fn complete_unmodified_journey() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Complete)
            .then_expect_events(vec![JourneyEvent::Completed]);
    }

    #[test]
    fn complete_modified_journey() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    step: "first_name".to_string(),
                    data: json!("Joe"),
                },
            ])
            .when(JourneyCommand::Complete)
            .then_expect_events(vec![JourneyEvent::Completed]);
    }

    #[test]
    fn capture_empty_form_data() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                step: "form_data".to_string(),
                data: json!({}),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    step: "form_data".to_string(),
                    data: json!({}),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "form_data".to_string(),
                },
            ]);
    }

    #[test]
    fn capture_form_data_with_values() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    step: "form_data".to_string(),
                    data: json!({}),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "form_data".to_string(),
                },
            ])
            .when(JourneyCommand::Capture {
                step: "alpha".to_string(),
                data: json!({
                    "alpha": 42,
                    "beta": "hello"
                }),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    step: "alpha".to_string(),
                    data: json!({
                        "alpha": 42,
                        "beta": "hello"
                    }),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                },
                JourneyEvent::StepProgressed {
                    from_step: Some("form_data".to_string()),
                    to_step: "alpha".to_string(),
                },
            ]);
    }

    #[test]
    fn complete_journey_with_form_data() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    step: "alpha".to_string(),
                    data: json!({
                        "alpha": 42,
                        "beta": "hello"
                    }),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                },
                JourneyEvent::StepProgressed {
                    from_step: Some("form_data".to_string()),
                    to_step: "alpha".to_string(),
                },
            ])
            .when(JourneyCommand::Complete)
            .then_expect_events(vec![JourneyEvent::Completed]);
    }

    #[test]
    fn open_already_opened() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Start { id })
            .then_expect_error(JourneyError::AlreadyStarted);
    }

    #[test]
    fn complete_not_started() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::Complete)
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn complete_already_completed() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::Complete)
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn modify_not_started() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::Capture {
                step: "first_name".to_string(),
                data: json!("Joe"),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn modify_already_completed() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::Capture {
                step: "first_name".to_string(),
                data: json!("Joe"),
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn automatic_workflow_evaluation_after_every_event() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                step: "step-1".to_string(),
                data: json!({
                    "step": "personal_info",
                    "email": "user@example.com",
                    "name": "Alice"
                }),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    step: "step-1".to_string(),
                    data: json!({
                        "step": "personal_info",
                        "email": "user@example.com",
                        "name": "Alice"
                    }),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "step-1".to_string(),
                },
            ]);
    }

    #[test]
    fn automatic_workflow_evaluation_for_specific_data() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                step: "step-1".to_string(),
                data: json!({
                    "step": "personal_info",
                    "email": "user@example.com",
                    "first_name": "Alice"
                }),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    step: "step-1".to_string(),
                    data: json!({
                        "step": "personal_info",
                        "email": "user@example.com",
                        "first_name": "Alice"
                    }),
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec!["form_3".to_string()],
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "step-1".to_string(),
                },
            ]);
    }

    #[test]
    fn test_capture_person() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::CapturePerson {
                name: "John Doe".to_string(),
                email: "john@example.com".to_string(),
                phone: Some("+1234567890".to_string()),
            })
            .then_expect_events(vec![JourneyEvent::PersonCaptured {
                name: "John Doe".to_string(),
                email: "john@example.com".to_string(),
                phone: Some("+1234567890".to_string()),
            }]);
    }

    #[test]
    fn test_capture_person_update() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::PersonCaptured {
                    name: "John Doe".to_string(),
                    email: "john@example.com".to_string(),
                    phone: Some("+1234567890".to_string()),
                },
            ])
            .when(JourneyCommand::CapturePerson {
                name: "Jane Smith".to_string(),
                email: "jane@example.com".to_string(),
                phone: None,
            })
            .then_expect_events(vec![JourneyEvent::PersonCaptured {
                name: "Jane Smith".to_string(),
                email: "jane@example.com".to_string(),
                phone: None,
            }]);
    }

    #[test]
    fn test_capture_person_journey_not_started() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::CapturePerson {
                name: "John Doe".to_string(),
                email: "john@example.com".to_string(),
                phone: None,
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn test_capture_person_journey_completed() {
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::CapturePerson {
                name: "John Doe".to_string(),
                email: "john@example.com".to_string(),
                phone: None,
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn test_capture_invalid_data_schema_validation_error() {
        // Test that the JsonSchemaValidator properly rejects data that doesn't match the schema
        // and returns an InvalidData error with validation details
        let services = JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        );
        let id = Uuid::new_v4();

        // Create invalid data that violates the schema
        // The schema expects "alpha" to be a number, but we're providing a string
        let invalid_data = json!({
            "alpha": "this should be a number",
            "beta": 123  // This should be a string
        });

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                step: "test_step".to_string(),
                data: invalid_data,
            })
            .then_expect_error(JourneyError::InvalidData("Schema validation failed: {\"alpha\":\"this should be a number\",\"beta\":123} is not valid under any of the schemas listed in the 'oneOf' keyword".to_string()));
    }
}
