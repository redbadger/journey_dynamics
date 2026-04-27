use std::collections::BTreeMap;
use std::sync::Arc;

use crate::domain::events::JourneyEvent;
use crate::services::schema_validator::SchemaValidator;
use crate::{domain::commands::JourneyCommand, services::decision_engine::DecisionEngine};
use cqrs_es::Aggregate;
use cqrs_es::event_sink::EventSink;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    /// Shared, non-PII data accumulated from `Capture` commands.
    /// Never encrypted. Fully intact after any shredding operation.
    shared_data: Value,
    /// Per-person slots, keyed by client-assigned `person_ref`.
    persons: BTreeMap<String, PersonSlot>,
    current_step: Option<String>,
    latest_workflow_decision: Option<WorkflowDecisionState>,
}

/// One data subject's slot within a journey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonSlot {
    /// Cross-journey identity key — used to look up the DEK in the key store.
    pub subject_id: Uuid,
    /// Identity fields captured by `CapturePerson`. Encrypted at rest.
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Free-form PII details (passport, `DoB`, nationality, …) captured by
    /// `CapturePersonDetails`. Encrypted at rest.
    pub details: Value,
    /// Set to `true` when a `SubjectForgotten` event is applied for this
    /// subject. The encrypted event payloads become unreadable at the same
    /// time (DEK deleted), so this is primarily a tombstone for the read model.
    pub forgotten: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionState {
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum JourneyState {
    #[default]
    InProgress,
    Complete,
}

impl Aggregate for Journey {
    type Command = JourneyCommand;
    type Event = JourneyEvent;
    type Error = JourneyError;
    type Services = JourneyServices;

    const TYPE: &'static str = "Journey";

    #[allow(clippy::too_many_lines)]
    async fn handle(
        &mut self,
        command: Self::Command,
        services: &Self::Services,
        sink: &EventSink<Self>,
    ) -> Result<(), Self::Error> {
        match command {
            JourneyCommand::Start { id } => {
                if self.id == id {
                    Err(JourneyError::AlreadyStarted)
                } else {
                    sink.write(JourneyEvent::Started { id }, self).await;
                    Ok(())
                }
            }

            JourneyCommand::CapturePerson {
                person_ref,
                subject_id,
                name,
                email,
                phone,
            } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                // If the slot already exists, the subject_id must match.
                if let Some(slot) = self.persons.get(&person_ref)
                    && slot.subject_id != subject_id
                {
                    return Err(JourneyError::PersonRefConflict(person_ref));
                }
                sink.write(
                    JourneyEvent::PersonCaptured {
                        person_ref,
                        subject_id,
                        name,
                        email,
                        phone,
                    },
                    self,
                )
                .await;
                Ok(())
            }

            JourneyCommand::CapturePersonDetails { person_ref, data } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                // The slot must already exist so we know which subject_id to use.
                let subject_id = match self.persons.get(&person_ref) {
                    Some(slot) => slot.subject_id,
                    None => return Err(JourneyError::PersonNotFound(person_ref)),
                };
                sink.write(
                    JourneyEvent::PersonDetailsUpdated {
                        person_ref,
                        subject_id,
                        data,
                    },
                    self,
                )
                .await;
                Ok(())
            }

            JourneyCommand::Capture { step, data } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }

                if let Err(e) = services.schema_validator().validate(&data) {
                    return Err(JourneyError::InvalidData(e.to_string()));
                }

                let is_step_transition = self.current_step.as_ref() != Some(&step);

                let mut journey_for_eval = self.clone();
                if is_step_transition {
                    journey_for_eval.current_step = Some(step.clone());
                }

                let decision = services
                    .decision_engine()
                    .evaluate_next_steps(&journey_for_eval, &step, &data)
                    .await
                    .map_err(|e| JourneyError::DecisionEngineError(e.to_string()))?;

                let from_step = self.current_step.clone();

                sink.write(
                    JourneyEvent::Modified {
                        step: step.clone(),
                        data: data.clone(),
                    },
                    self,
                )
                .await;

                sink.write(
                    JourneyEvent::WorkflowEvaluated {
                        suggested_actions: decision.suggested_actions,
                    },
                    self,
                )
                .await;

                if is_step_transition {
                    sink.write(
                        JourneyEvent::StepProgressed {
                            from_step,
                            to_step: step.clone(),
                        },
                        self,
                    )
                    .await;
                }

                Ok(())
            }

            JourneyCommand::Complete => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    sink.write(JourneyEvent::Completed, self).await;
                    Ok(())
                }
            }

            JourneyCommand::ForgetSubject { subject_id } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                // Only emit SubjectForgotten if the subject has at least one
                // non-forgotten slot in this journey.  This makes the shredding
                // endpoint idempotent: a second erasure request for the same
                // subject (or one issued after the journey is already complete)
                // does not produce a duplicate audit event.
                let needs_forgetting = self
                    .persons
                    .values()
                    .any(|slot| slot.subject_id == subject_id && !slot.forgotten);
                if needs_forgetting {
                    sink.write(JourneyEvent::SubjectForgotten { subject_id }, self)
                        .await;
                }
                Ok(())
            }
        }
    }

    fn apply(&mut self, event: Self::Event) {
        match event {
            JourneyEvent::Started { id } => {
                self.id = id;
                self.state = JourneyState::InProgress;
            }
            JourneyEvent::Modified { data, .. } => {
                json_patch::merge(&mut self.shared_data, &data);
            }
            JourneyEvent::PersonCaptured {
                person_ref,
                subject_id,
                name,
                email,
                phone,
            } => {
                // `or_insert_with` creates the slot on first capture; on subsequent
                // captures (same person_ref, same subject_id) it returns the existing
                // slot so we can update the identity fields in place.
                let slot = self
                    .persons
                    .entry(person_ref)
                    .or_insert_with(|| PersonSlot {
                        subject_id,
                        name: None,
                        email: None,
                        phone: None,
                        details: json!({}),
                        forgotten: false,
                    });
                slot.name = Some(name);
                slot.email = Some(email);
                slot.phone = phone;
            }
            JourneyEvent::PersonDetailsUpdated {
                person_ref, data, ..
            } => {
                if let Some(slot) = self.persons.get_mut(&person_ref) {
                    json_patch::merge(&mut slot.details, &data);
                }
            }
            JourneyEvent::WorkflowEvaluated { suggested_actions } => {
                self.latest_workflow_decision = Some(WorkflowDecisionState { suggested_actions });
            }
            JourneyEvent::StepProgressed { to_step, .. } => {
                self.current_step = Some(to_step);
            }
            JourneyEvent::Completed => {
                self.state = JourneyState::Complete;
            }
            JourneyEvent::SubjectForgotten { subject_id } => {
                for slot in self.persons.values_mut() {
                    if slot.subject_id == subject_id {
                        slot.forgotten = true;
                    }
                }
            }
        }
    }
}

#[derive(Error, Debug, PartialEq, Eq)]
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
    #[error("Person slot '{0}' is already bound to a different subject")]
    PersonRefConflict(String),
    #[error("Person slot '{0}' does not exist — call CapturePerson first")]
    PersonNotFound(String),
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
    pub const fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub const fn state(&self) -> JourneyState {
        self.state
    }

    #[must_use]
    pub const fn shared_data(&self) -> &Value {
        &self.shared_data
    }

    #[must_use]
    pub const fn current_step(&self) -> Option<&String> {
        self.current_step.as_ref()
    }

    #[must_use]
    pub const fn latest_workflow_decision(&self) -> Option<&WorkflowDecisionState> {
        self.latest_workflow_decision.as_ref()
    }

    #[must_use]
    pub const fn persons(&self) -> &BTreeMap<String, PersonSlot> {
        &self.persons
    }
}

impl Default for Journey {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            shared_data: json!({}),
            persons: BTreeMap::new(),
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
                { "type": "string" },
                {
                    "type": "object",
                    "properties": {
                        "alpha":      { "type": "number" },
                        "beta":       { "type": "string" },
                        "step":       { "type": "string" },
                        "email":      { "type": "string", "format": "email" },
                        "name":       { "type": "string" },
                        "first_name": { "type": "string" }
                    },
                    "additionalProperties": true
                }
            ]
        });
        Arc::new(JsonSchemaValidator::new(&schema).unwrap())
    }

    fn services() -> JourneyServices {
        JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
        )
    }

    // ── Journey lifecycle ────────────────────────────────────────────────────

    #[test]
    fn start_a_journey() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::Start { id })
            .then_expect_events(vec![JourneyEvent::Started { id }]);
    }

    #[test]
    fn modify_journey() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
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
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Complete)
            .then_expect_events(vec![JourneyEvent::Completed]);
    }

    #[test]
    fn complete_modified_journey() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
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
        let id = Uuid::new_v4();
        JourneyTester::with(services())
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
        let id = Uuid::new_v4();
        JourneyTester::with(services())
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
                data: json!({ "alpha": 42, "beta": "hello" }),
            })
            .then_expect_events(vec![
                JourneyEvent::Modified {
                    step: "alpha".to_string(),
                    data: json!({ "alpha": 42, "beta": "hello" }),
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
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::Modified {
                    step: "alpha".to_string(),
                    data: json!({ "alpha": 42, "beta": "hello" }),
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
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Start { id })
            .then_expect_error(JourneyError::AlreadyStarted);
    }

    #[test]
    fn complete_not_started() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::Complete)
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn complete_already_completed() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::Complete)
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn modify_not_started() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::Capture {
                step: "first_name".to_string(),
                data: json!("Joe"),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn modify_already_completed() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::Capture {
                step: "first_name".to_string(),
                data: json!("Joe"),
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    // ── Workflow evaluation ──────────────────────────────────────────────────

    #[test]
    fn automatic_workflow_evaluation_after_every_event() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
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
        let id = Uuid::new_v4();
        JourneyTester::with(services())
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

    // ── CapturePerson ────────────────────────────────────────────────────────

    #[test]
    fn test_capture_person() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
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

    #[test]
    fn test_capture_person_updates_identity_fields_for_same_subject() {
        // Calling CapturePerson again with the same person_ref and subject_id
        // is allowed — it updates the identity fields in place.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
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
            .when(JourneyCommand::CapturePerson {
                person_ref: "passenger_0".to_string(),
                subject_id, // same subject_id — update allowed
                name: "Alice J. Smith".to_string(),
                email: "alice.new@example.com".to_string(),
                phone: Some("+44-7700-900001".to_string()),
            })
            .then_expect_events(vec![JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id,
                name: "Alice J. Smith".to_string(),
                email: "alice.new@example.com".to_string(),
                phone: Some("+44-7700-900001".to_string()),
            }]);
    }

    #[test]
    fn test_capture_person_conflict_rejects_different_subject_for_same_ref() {
        // Reusing a person_ref with a different subject_id is an error.
        let id = Uuid::new_v4();
        let subject_id_a = Uuid::new_v4();
        let subject_id_b = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_0".to_string(),
                    subject_id: subject_id_a,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
            ])
            .when(JourneyCommand::CapturePerson {
                person_ref: "passenger_0".to_string(),
                subject_id: subject_id_b, // different subject — must be rejected
                name: "Bob Jones".to_string(),
                email: "bob@example.com".to_string(),
                phone: None,
            })
            .then_expect_error(JourneyError::PersonRefConflict("passenger_0".to_string()));
    }

    #[test]
    fn test_capture_multiple_persons_independently() {
        // Two different passengers in the same journey.
        let id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        JourneyTester::with(services())
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

    #[test]
    fn test_capture_person_journey_not_started() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::CapturePerson {
                person_ref: "passenger_0".to_string(),
                subject_id: Uuid::new_v4(),
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn test_capture_person_journey_completed() {
        let id = Uuid::new_v4();
        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::CapturePerson {
                person_ref: "passenger_0".to_string(),
                subject_id: Uuid::new_v4(),
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    // ── CapturePersonDetails ─────────────────────────────────────────────────

    #[test]
    fn test_capture_person_details() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
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
                    "passportNumber": "GB123456789",
                    "dateOfBirth":    "1990-05-15",
                    "nationality":    "GB"
                }),
            })
            .then_expect_events(vec![JourneyEvent::PersonDetailsUpdated {
                person_ref: "passenger_0".to_string(),
                subject_id, // copied from the slot by the aggregate
                data: json!({
                    "passportNumber": "GB123456789",
                    "dateOfBirth":    "1990-05-15",
                    "nationality":    "GB"
                }),
            }]);
    }

    #[test]
    fn test_capture_person_details_uses_subject_id_from_slot() {
        // The emitted event carries the subject_id from the existing slot, not
        // one supplied by the caller — CapturePersonDetails has no subject_id field.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::PersonCaptured {
                    person_ref: "lead_booker".to_string(),
                    subject_id,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
            ])
            .when(JourneyCommand::CapturePersonDetails {
                person_ref: "lead_booker".to_string(),
                data: json!({ "dateOfBirth": "1990-01-01" }),
            })
            .then_expect_events(vec![JourneyEvent::PersonDetailsUpdated {
                person_ref: "lead_booker".to_string(),
                subject_id, // must match the subject captured above
                data: json!({ "dateOfBirth": "1990-01-01" }),
            }]);
    }

    #[test]
    fn test_capture_person_details_not_started() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::CapturePersonDetails {
                person_ref: "passenger_0".to_string(),
                data: json!({ "passportNumber": "GB123456789" }),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn test_capture_person_details_journey_completed() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_0".to_string(),
                    subject_id,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
                JourneyEvent::Completed,
            ])
            .when(JourneyCommand::CapturePersonDetails {
                person_ref: "passenger_0".to_string(),
                data: json!({ "passportNumber": "GB123456789" }),
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn test_capture_person_details_slot_not_found() {
        // CapturePersonDetails requires CapturePerson to have been called first.
        let id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::CapturePersonDetails {
                person_ref: "passenger_0".to_string(),
                data: json!({ "passportNumber": "GB123456789" }),
            })
            .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
    }

    #[test]
    fn test_capture_person_details_multiple_calls_merge() {
        // Successive CapturePersonDetails calls for the same slot each produce
        // their own PersonDetailsUpdated event; the aggregate merges them via
        // json_patch::merge in apply().
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_0".to_string(),
                    subject_id,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
                JourneyEvent::PersonDetailsUpdated {
                    person_ref: "passenger_0".to_string(),
                    subject_id,
                    data: json!({ "passportNumber": "GB123456789" }),
                },
            ])
            .when(JourneyCommand::CapturePersonDetails {
                person_ref: "passenger_0".to_string(),
                data: json!({ "nationality": "GB", "dateOfBirth": "1990-05-15" }),
            })
            .then_expect_events(vec![JourneyEvent::PersonDetailsUpdated {
                person_ref: "passenger_0".to_string(),
                subject_id,
                data: json!({ "nationality": "GB", "dateOfBirth": "1990-05-15" }),
            }]);
    }

    // ── ForgetSubject ────────────────────────────────────────────────────────

    #[test]
    fn test_forget_subject() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
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
            .when(JourneyCommand::ForgetSubject { subject_id })
            .then_expect_events(vec![JourneyEvent::SubjectForgotten { subject_id }]);
    }

    #[test]
    fn test_forget_subject_already_forgotten_is_noop() {
        // A second ForgetSubject for the same subject must not emit another
        // SubjectForgotten event — shredding is idempotent.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_0".to_string(),
                    subject_id,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
                // The subject was already forgotten in a prior shredding call.
                JourneyEvent::SubjectForgotten { subject_id },
            ])
            .when(JourneyCommand::ForgetSubject { subject_id })
            .then_expect_events(vec![]);
    }

    #[test]
    fn test_forget_subject_for_subject_not_in_journey_is_noop() {
        // ForgetSubject for a subject that never appeared in this journey
        // must not emit SubjectForgotten.
        let id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        JourneyTester::with(services())
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
            // subject_b has no slot in this journey.
            .when(JourneyCommand::ForgetSubject {
                subject_id: subject_b,
            })
            .then_expect_events(vec![]);
    }

    #[test]
    fn test_forget_subject_journey_not_found() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::ForgetSubject {
                subject_id: Uuid::new_v4(),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn test_forget_subject_only_affects_target_slot() {
        // After forgetting passenger_0, the aggregate should mark only that
        // slot as forgotten; passenger_1's slot must be unaffected.
        let id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        // Build the aggregate state by replaying events directly via apply().
        let mut journey = Journey::default();
        for event in [
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
            JourneyEvent::SubjectForgotten {
                subject_id: subject_a,
            },
        ] {
            journey.apply(event);
        }

        let p0 = journey.persons().get("passenger_0").unwrap();
        assert!(p0.forgotten, "passenger_0 should be forgotten");

        let p1 = journey.persons().get("passenger_1").unwrap();
        assert!(!p1.forgotten, "passenger_1 should NOT be forgotten");
    }

    // ── apply() — shared_data accumulation ───────────────────────────────────

    #[test]
    fn test_apply_merges_shared_data() {
        let id = Uuid::new_v4();
        let mut journey = Journey::default();
        journey.apply(JourneyEvent::Started { id });
        journey.apply(JourneyEvent::Modified {
            step: "search".to_string(),
            data: json!({ "origin": "LHR", "destination": "JFK" }),
        });
        journey.apply(JourneyEvent::Modified {
            step: "pricing".to_string(),
            data: json!({ "totalPrice": 450.00 }),
        });

        assert_eq!(journey.shared_data()["origin"], json!("LHR"));
        assert_eq!(journey.shared_data()["destination"], json!("JFK"));
        assert_eq!(journey.shared_data()["totalPrice"], json!(450.00));
    }

    #[test]
    fn test_apply_person_details_merges_into_slot() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let mut journey = Journey::default();
        journey.apply(JourneyEvent::Started { id });
        journey.apply(JourneyEvent::PersonCaptured {
            person_ref: "passenger_0".to_string(),
            subject_id,
            name: "Alice Smith".to_string(),
            email: "alice@example.com".to_string(),
            phone: None,
        });
        journey.apply(JourneyEvent::PersonDetailsUpdated {
            person_ref: "passenger_0".to_string(),
            subject_id,
            data: json!({ "passportNumber": "GB123456789" }),
        });
        journey.apply(JourneyEvent::PersonDetailsUpdated {
            person_ref: "passenger_0".to_string(),
            subject_id,
            data: json!({ "dateOfBirth": "1990-05-15" }),
        });

        let slot = journey.persons().get("passenger_0").unwrap();
        assert_eq!(slot.details["passportNumber"], json!("GB123456789"));
        assert_eq!(slot.details["dateOfBirth"], json!("1990-05-15"));
    }

    // ── Schema validation ────────────────────────────────────────────────────

    #[test]
    fn test_capture_invalid_data_schema_validation_error() {
        let id = Uuid::new_v4();
        let invalid_data = json!({
            "alpha": "this should be a number",
            "beta": 123  // should be a string
        });

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::Capture {
                step: "test_step".to_string(),
                data: invalid_data,
            })
            .then_expect_error(JourneyError::InvalidData(
                "Schema validation failed: {\"alpha\":\"this should be a number\",\"beta\":123} \
                 is not valid under any of the schemas listed in the 'oneOf' keyword"
                    .to_string(),
            ));
    }
}
