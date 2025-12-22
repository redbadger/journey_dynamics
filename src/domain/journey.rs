use std::sync::Arc;

use crate::domain::events::JourneyEvent;
use crate::utils::DataMerger;
use crate::{domain::commands::JourneyCommand, services::decision_engine::DecisionEngine};
use async_trait::async_trait;
use cqrs_es::Aggregate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    data_capture: Vec<(String, Value)>,
    current_step: Option<String>,
    latest_workflow_decision: Option<WorkflowDecisionState>,
    #[serde(skip)]
    data_merger: DataMerger,
}

impl Default for Journey {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            data_capture: Vec::new(),
            current_step: None,
            latest_workflow_decision: None,
            data_merger: DataMerger::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionState {
    pub available_actions: Vec<String>,
    pub primary_next_step: Option<String>,
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
        _services: &Self::Services,
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
            JourneyCommand::Capture { data } => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    // Determine if the data key represents a step transition
                    let (key, _) = &data;
                    let is_step_transition = key != "capturedData"
                        && key != "currentStep"
                        && Some(key) != self.current_step.as_ref();

                    // If this is a step transition, temporarily update currentStep for evaluation
                    let mut journey_for_eval = self.clone();
                    if is_step_transition {
                        journey_for_eval.current_step = Some(key.clone());
                    }

                    let decision = _services
                        .decision_engine()
                        .evaluate_next_steps(&journey_for_eval, &data)
                        .await
                        .map_err(|e| JourneyError::DecisionEngineError(e.to_string()))?;

                    let mut events = vec![
                        JourneyEvent::Modified {
                            form_data: Some(data.clone()),
                        },
                        JourneyEvent::WorkflowEvaluated {
                            available_actions: decision.available_actions.clone(),
                            primary_next_step: decision.primary_next_step.clone(),
                        },
                    ];

                    // If the data key represents a step, emit StepProgressed event
                    if is_step_transition {
                        events.push(JourneyEvent::StepProgressed {
                            from_step: self.current_step.clone(),
                            to_step: key.clone(),
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
            JourneyEvent::Modified { form_data } => {
                if let Some((key, value)) = form_data.clone() {
                    self.data_capture.push((key.clone(), value.clone()));
                    // Use DataMerger for consistent data handling
                    if let Err(e) = self.data_merger.merge_form_data(&key, &value) {
                        // Log error but don't fail the event application
                        eprintln!("Warning: Failed to merge data for key '{key}': {e}");
                    }
                }
            }
            JourneyEvent::WorkflowEvaluated {
                available_actions,
                primary_next_step,
            } => {
                self.latest_workflow_decision = Some(WorkflowDecisionState {
                    available_actions,
                    primary_next_step,
                });
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
}

pub struct JourneyServices {
    decision_engine: Arc<dyn DecisionEngine>,
}

impl JourneyServices {
    pub fn new(decision_engine: Arc<dyn DecisionEngine>) -> Self {
        Self { decision_engine }
    }

    #[must_use]
    pub fn decision_engine(&self) -> &Arc<dyn DecisionEngine> {
        &self.decision_engine
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
    pub fn data_capture(&self) -> &[(String, Value)] {
        &self.data_capture
    }

    #[must_use]
    pub fn current_step(&self) -> Option<&String> {
        self.current_step.as_ref()
    }

    #[must_use]
    pub fn latest_workflow_decision(&self) -> Option<&WorkflowDecisionState> {
        self.latest_workflow_decision.as_ref()
    }

    /// Get the current merged data state
    #[must_use]
    pub fn get_merged_data(&self) -> &Value {
        self.data_merger.get_merged_data()
    }

    /// Get a specific field from the merged data
    #[must_use]
    pub fn get_field(&self, field_path: &str) -> Option<&Value> {
        self.data_merger.get_field(field_path)
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

    type JourneyTester = TestFramework<Journey>;

    #[test]
    fn start_a_journey() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::Start { id })
            .then_expect_events(vec![JourneyEvent::Started { id }]);
    }

    #[test]
    fn modify_journey() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                data: ("first_name".to_string(), json!("Joe")),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    form_data: Some(("first_name".to_string(), json!("Joe"))),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec![],
                    primary_next_step: None,
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "first_name".to_string(),
                },
            ]);
    }

    #[test]
    fn complete_unmodified_journey() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Complete)
            .then_expect_events(vec![JourneyEvent::Completed]);
    }

    #[test]
    fn complete_modified_journey() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    form_data: Some(("first_name".to_string(), json!("Joe"))),
                },
            ])
            .when(JourneyCommand::Complete)
            .then_expect_events(vec![JourneyEvent::Completed]);
    }

    #[test]
    fn capture_empty_form_data() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                data: ("form_data".to_string(), json!({})),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    form_data: Some(("form_data".to_string(), json!({}))),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec![],
                    primary_next_step: None,
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "form_data".to_string(),
                },
            ]);
    }

    #[test]
    fn capture_form_data_with_values() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    form_data: Some(("form_data".to_string(), json!({}))),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec![],
                    primary_next_step: None,
                },
                JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "form_data".to_string(),
                },
            ])
            .when(JourneyCommand::Capture {
                data: (
                    "alpha".to_string(),
                    json!({
                        "alpha": 42,
                        "beta": "hello"
                    }),
                ),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    form_data: Some((
                        "alpha".to_string(),
                        json!({
                            "alpha": 42,
                            "beta": "hello"
                        }),
                    )),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec![],
                    primary_next_step: None,
                },
                JourneyEvent::StepProgressed {
                    from_step: Some("form_data".to_string()),
                    to_step: "alpha".to_string(),
                },
            ]);
    }

    #[test]
    fn complete_journey_with_form_data() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    form_data: Some((
                        "alpha".to_string(),
                        json!({
                            "alpha": 42,
                            "beta": "hello"
                        }),
                    )),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec![],
                    primary_next_step: None,
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
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Start { id })
            .then_expect_error(JourneyError::AlreadyStarted);
    }

    #[test]
    fn complete_not_started() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::Complete)
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn complete_already_completed() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::Complete)
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn modify_not_started() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));

        JourneyTester::with(services)
            .given_no_previous_events()
            .when(JourneyCommand::Capture {
                data: ("first_name".to_string(), json!("Joe")),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn modify_already_completed() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::Capture {
                data: ("first_name".to_string(), json!("Joe")),
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn automatic_workflow_evaluation_after_every_event() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                data: (
                    "capturedData".to_string(),
                    json!({
                        "step": "personal_info",
                        "email": "user@example.com",
                        "name": "Alice"
                    }),
                ),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    form_data: Some((
                        "capturedData".to_string(),
                        json!({
                            "step": "personal_info",
                            "email": "user@example.com",
                            "name": "Alice"
                        }),
                    )),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec![],
                    primary_next_step: None,
                },
            ]);
    }

    #[test]
    fn automatic_workflow_evaluation_for_specific_data() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
        let id = Uuid::new_v4();

        JourneyTester::with(services)
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                data: (
                    "capturedData".to_string(),
                    json!({
                        "step": "personal_info",
                        "email": "user@example.com",
                        "first_name": "Alice"
                    }),
                ),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    form_data: Some((
                        "capturedData".to_string(),
                        json!({
                            "step": "personal_info",
                            "email": "user@example.com",
                            "first_name": "Alice"
                        }),
                    )),
                },
                JourneyEvent::WorkflowEvaluated {
                    available_actions: vec!["form_3".to_string()],
                    primary_next_step: None,
                },
            ]);
    }

    #[test]
    fn test_capture_person() {
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
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
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
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
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));

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
        let services = JourneyServices::new(Arc::new(SimpleDecisionEngine));
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
}
