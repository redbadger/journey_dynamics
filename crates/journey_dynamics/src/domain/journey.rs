use std::{collections::BTreeMap, sync::Arc};

use cqrs_es::{Aggregate, event_sink::EventSink};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    domain::{
        AttributeSchema, assign_all,
        commands::JourneyCommand,
        events::{JourneyEvent, SecretPartitionData},
    },
    services::{decision_engine::DecisionEngine, schema_validator::SchemaValidator},
};
use es_capture::{
    capture::{CaptureError, capture},
    subject_registry::{SubjectError, SubjectRegistry},
};
use jsonptr::PointerBuf;

pub use es_capture::subject_registry::SubjectRegistration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    /// Accumulated journey attributes (plaintext and decrypted secret values).
    /// Never encrypted at rest. Fully intact after any shredding operation.
    shared_data: Value,
    /// Registered subjects and their role-path → subject bindings.
    #[serde(flatten)]
    registry: SubjectRegistry,
    latest_workflow_decision: Option<WorkflowDecisionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionState {
    pub suggested_actions: Vec<String>,
    /// Phase label from the decision engine.
    /// `None` until the `WorkflowEvaluated` event carries `phase` (step B1).
    pub phase: Option<String>,
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

            JourneyCommand::SetAttributes { changes } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                if changes.is_empty() {
                    return Err(JourneyError::InvalidData("no changes".to_string()));
                }

                // Run the domain-agnostic capture pipeline: classify → validate
                // → (optionally) evaluate the decision engine. The journey
                // aggregate turns the outcome into its own events below.
                let outcome = capture(
                    services.attribute_schema(),
                    &self.registry,
                    self.shared_data(),
                    &changes,
                    services.schema_validator().as_ref(),
                    services.decision_engine().map(|engine| &**engine),
                )
                .await?;

                let secret_partitions: Vec<SecretPartitionData> = outcome
                    .secret
                    .into_iter()
                    .map(|slice| SecretPartitionData {
                        role_path: slice.role_path,
                        subject_id: slice.subject_id,
                        changes: slice.changes,
                    })
                    .collect();

                sink.write(
                    JourneyEvent::AttributesSet {
                        plaintext: outcome.plaintext,
                        secret_partitions,
                    },
                    self,
                )
                .await;

                if let Some(decision) = outcome.decision {
                    sink.write(
                        JourneyEvent::WorkflowEvaluated {
                            suggested_actions: decision.suggested_actions,
                            phase: decision.phase,
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

            JourneyCommand::RegisterSubject { subject_id, email } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                // Idempotent: skip if already registered with the same email.
                if !self.registry.needs_registration(&subject_id, &email) {
                    return Ok(());
                }
                sink.write(JourneyEvent::SubjectRegistered { subject_id, email }, self)
                    .await;
                Ok(())
            }

            JourneyCommand::BindSubject {
                role_path,
                subject_id,
            } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                if !self.registry.is_registered(&subject_id) {
                    return Err(JourneyError::SubjectNotRegistered);
                }
                match self.registry.check_binding(&role_path, &subject_id) {
                    Err(e) => return Err(e.into()),
                    Ok(false) => return Ok(()), // same subject — idempotent
                    Ok(true) => {}
                }
                sink.write(
                    JourneyEvent::SubjectBound {
                        role_path,
                        subject_id,
                    },
                    self,
                )
                .await;
                Ok(())
            }

            JourneyCommand::RegisterAndBindSubject {
                role_path,
                subject_id,
                email,
            } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                // Validate the binding upfront before emitting any events.
                self.registry.check_binding(&role_path, &subject_id)?;
                // Emit SubjectRegistered if new or email changed.
                if self.registry.needs_registration(&subject_id, &email) {
                    sink.write(JourneyEvent::SubjectRegistered { subject_id, email }, self)
                        .await;
                }
                // Emit SubjectBound if not already bound.
                if self.registry.binding(&role_path).is_none() {
                    sink.write(
                        JourneyEvent::SubjectBound {
                            role_path,
                            subject_id,
                        },
                        self,
                    )
                    .await;
                }
                Ok(())
            }

            JourneyCommand::ForgetSubject { subject_id } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                // Only emit SubjectForgotten if the subject is still active.
                // This keeps the shredding endpoint idempotent.
                if self.registry.needs_forgetting(&subject_id) {
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
            JourneyEvent::AttributesSet {
                plaintext,
                secret_partitions,
            } => {
                // Apply plaintext changes directly into shared_data.
                assign_all(&mut self.shared_data, &plaintext).unwrap();
                // Apply secret changes.
                for partition in &secret_partitions {
                    // Write every change at its full path into shared_data.
                    assign_all(&mut self.shared_data, &partition.changes)
                        .expect("events should have valid JSON pointers");
                }
            }

            JourneyEvent::WorkflowEvaluated {
                suggested_actions,
                phase,
            } => {
                self.latest_workflow_decision = Some(WorkflowDecisionState {
                    suggested_actions,
                    phase,
                });
            }
            JourneyEvent::Completed => {
                self.state = JourneyState::Complete;
            }
            JourneyEvent::SubjectForgotten { subject_id } => {
                self.registry.forget(&subject_id);
            }
            JourneyEvent::SubjectRegistered { subject_id, email } => {
                self.registry.register(subject_id, email);
            }
            JourneyEvent::SubjectBound {
                role_path,
                subject_id,
            } => {
                self.registry.bind(role_path, subject_id);
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
    #[error("Unknown attribute paths: {0:?}")]
    UnknownAttributePath(Vec<PointerBuf>),
    #[error("Invalid JSON pointer: {0}")]
    InvalidJsonPointer(#[from] jsonptr::assign::Error),
    #[error("Subject not registered — call RegisterSubject first")]
    SubjectNotRegistered,
    #[error("Role path '{0}' is already bound to a different subject")]
    RolePathConflict(PointerBuf),
}

impl From<SubjectError> for JourneyError {
    fn from(err: SubjectError) -> Self {
        match err {
            SubjectError::NotRegistered => Self::SubjectNotRegistered,
            SubjectError::RolePathConflict(role_path) => Self::RolePathConflict(role_path),
        }
    }
}

impl From<CaptureError> for JourneyError {
    fn from(err: CaptureError) -> Self {
        match err {
            CaptureError::UnknownAttributePath(paths) => Self::UnknownAttributePath(paths),
            CaptureError::SubjectNotResolved(role_path) => {
                // Preserve the historical PersonNotFound message: the short
                // person ref is the role path with the "/persons/" prefix
                // stripped.
                let person_ref = role_path
                    .as_str()
                    .strip_prefix("/persons/")
                    .unwrap_or(role_path.as_str())
                    .to_string();
                Self::PersonNotFound(person_ref)
            }
            CaptureError::InvalidJsonPointer(e) => Self::InvalidJsonPointer(e),
            CaptureError::InvalidData(msg) => Self::InvalidData(msg),
            CaptureError::DecisionEngine(msg) => Self::DecisionEngineError(msg),
        }
    }
}

pub struct JourneyServices {
    /// The decision engine is optional: when absent, capture still runs but no
    /// `WorkflowEvaluated` event is emitted.
    decision_engine: Option<Arc<dyn DecisionEngine>>,
    schema_validator: Arc<dyn SchemaValidator>,
    attribute_schema: Arc<AttributeSchema>,
}

impl JourneyServices {
    /// Construct with a decision engine (the common case).
    pub fn new(
        decision_engine: Arc<dyn DecisionEngine>,
        schema_validator: Arc<dyn SchemaValidator>,
        attribute_schema: Arc<AttributeSchema>,
    ) -> Self {
        Self {
            decision_engine: Some(decision_engine),
            schema_validator,
            attribute_schema,
        }
    }

    /// Construct without a decision engine — `SetAttributes` accumulates and
    /// validates attributes but emits no `WorkflowEvaluated` event.
    #[must_use]
    pub fn without_decision_engine(
        schema_validator: Arc<dyn SchemaValidator>,
        attribute_schema: Arc<AttributeSchema>,
    ) -> Self {
        Self {
            decision_engine: None,
            schema_validator,
            attribute_schema,
        }
    }

    #[must_use]
    pub fn decision_engine(&self) -> Option<&Arc<dyn DecisionEngine>> {
        self.decision_engine.as_ref()
    }

    #[must_use]
    pub fn schema_validator(&self) -> &Arc<dyn SchemaValidator> {
        &self.schema_validator
    }

    #[must_use]
    pub const fn attribute_schema(&self) -> &Arc<AttributeSchema> {
        &self.attribute_schema
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
    pub const fn latest_workflow_decision(&self) -> Option<&WorkflowDecisionState> {
        self.latest_workflow_decision.as_ref()
    }

    #[must_use]
    pub const fn subjects(&self) -> &BTreeMap<Uuid, SubjectRegistration> {
        self.registry.subjects()
    }

    #[must_use]
    pub const fn bindings(&self) -> &BTreeMap<PointerBuf, Uuid> {
        self.registry.bindings()
    }
}

impl Default for Journey {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            shared_data: json!({}),
            registry: SubjectRegistry::default(),
            latest_workflow_decision: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]
    #![allow(deprecated)]
    use cqrs_es::test::TestFramework;
    use serde_json::json;
    use std::assert_matches;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    use super::*;
    use crate::domain::{
        AttributeSchema,
        attribute_schema::{AttributeEntry, PiiClass},
        events::SecretPartitionData,
    };
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
                        "first_name": { "type": "string" },
                        "nicknames":  { "type": "array", "items": { "type": "string" }}
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
            Arc::new(AttributeSchema::permissive()),
        )
    }

    fn services_without_engine() -> JourneyServices {
        JourneyServices::without_decision_engine(
            create_test_schema_validator(),
            Arc::new(AttributeSchema::permissive()),
        )
    }

    /// A non-permissive attribute schema for tests that need explicit path
    /// classification. Registers two paths:
    /// - `search/origin` → Plaintext
    /// - `persons/passenger_0/passport` → Secret (subject = `persons/passenger_0`)
    /// - `persons/passenger_1/passport` → Secret (subject = `persons/passenger_1`)
    fn explicit_attribute_schema() -> AttributeSchema {
        let mut paths = BTreeMap::new();
        paths.insert(
            "/search/origin".parse().unwrap(),
            AttributeEntry::new(PiiClass::Plaintext),
        );
        paths.insert(
            "/persons/passenger_0/passport".parse().unwrap(),
            AttributeEntry::new(PiiClass::Secret {
                subject: "/persons/passenger_0".parse().unwrap(),
            }),
        );
        paths.insert(
            "/persons/passenger_1/passport".parse().unwrap(),
            AttributeEntry::new(PiiClass::Secret {
                subject: "/persons/passenger_1".parse().unwrap(),
            }),
        );
        AttributeSchema::new(paths, None)
    }

    fn services_with_attribute_schema(schema: AttributeSchema) -> JourneyServices {
        JourneyServices::new(
            Arc::new(SimpleDecisionEngine),
            create_test_schema_validator(),
            Arc::new(schema),
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
                JourneyEvent::AttributesSet {
                    plaintext: BTreeMap::from([("/first_name".parse().unwrap(), json!("Joe"))]),
                    secret_partitions: vec![],
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

    // ── ForgetSubject ──────────────────────────────────────────────────────────

    #[test]
    fn test_forget_subject() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
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
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
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
                JourneyEvent::SubjectRegistered {
                    subject_id: subject_a,
                    email: "alice@example.com".to_string(),
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
            JourneyEvent::SubjectRegistered {
                subject_id: subject_a,
                email: "alice@example.com".to_string(),
            },
            JourneyEvent::SubjectRegistered {
                subject_id: subject_b,
                email: "bob@example.com".to_string(),
            },
            JourneyEvent::SubjectForgotten {
                subject_id: subject_a,
            },
        ] {
            journey.apply(event);
        }

        let subjects = journey.subjects();
        assert!(
            subjects.get(&subject_a).is_some_and(|r| r.forgotten),
            "subject_a should be forgotten"
        );
        assert!(
            subjects.get(&subject_b).is_some_and(|r| !r.forgotten),
            "subject_b should NOT be forgotten"
        );
    }

    // ── RegisterSubject / BindSubject / RegisterAndBindSubject ────────────────

    #[test]
    fn register_subject_emits_subject_registered() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::RegisterSubject {
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![JourneyEvent::SubjectRegistered {
                subject_id,
                email: "alice@example.com".to_string(),
            }]);
    }

    #[test]
    fn register_subject_is_idempotent_with_same_email() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
            ])
            .when(JourneyCommand::RegisterSubject {
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![]);
    }

    #[test]
    fn register_subject_updates_email() {
        // Re-capturing with a different email must emit a new SubjectRegistered.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "old@example.com".to_string(),
                },
            ])
            .when(JourneyCommand::RegisterSubject {
                subject_id,
                email: "new@example.com".to_string(),
            })
            .then_expect_events(vec![JourneyEvent::SubjectRegistered {
                subject_id,
                email: "new@example.com".to_string(),
            }]);
    }

    #[test]
    fn register_subject_requires_started() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::RegisterSubject {
                subject_id: Uuid::new_v4(),
                email: "alice@example.com".to_string(),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn register_subject_rejects_after_complete() {
        let id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::RegisterSubject {
                subject_id: Uuid::new_v4(),
                email: "alice@example.com".to_string(),
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn bind_subject_emits_subject_bound() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
            ])
            .when(JourneyCommand::BindSubject {
                role_path: role_path.clone(),
                subject_id,
            })
            .then_expect_events(vec![JourneyEvent::SubjectBound {
                role_path,
                subject_id,
            }]);
    }

    #[test]
    fn bind_subject_is_idempotent() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id,
                },
            ])
            .when(JourneyCommand::BindSubject {
                role_path,
                subject_id,
            })
            .then_expect_events(vec![]);
    }

    #[test]
    fn bind_subject_rejects_role_path_conflict() {
        // Binding a different subject to an already-bound role path must fail.
        let id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id: subject_a,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectRegistered {
                    subject_id: subject_b,
                    email: "bob@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id: subject_a,
                },
            ])
            .when(JourneyCommand::BindSubject {
                role_path: role_path.clone(),
                subject_id: subject_b,
            })
            .then_expect_error(JourneyError::RolePathConflict(role_path));
    }

    #[test]
    fn bind_subject_rejects_unregistered_subject() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::BindSubject {
                role_path: "/persons/passenger_0".parse().unwrap(),
                subject_id,
            })
            .then_expect_error(JourneyError::SubjectNotRegistered);
    }

    #[test]
    fn register_and_bind_subject_emits_both_events() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::RegisterAndBindSubject {
                role_path: role_path.clone(),
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path,
                    subject_id,
                },
            ]);
    }

    #[test]
    fn register_and_bind_subject_is_idempotent() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id,
                },
            ])
            .when(JourneyCommand::RegisterAndBindSubject {
                role_path,
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![]);
    }

    #[test]
    fn register_and_bind_subject_rejects_role_path_conflict() {
        let id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id: subject_a,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id: subject_a,
                },
            ])
            .when(JourneyCommand::RegisterAndBindSubject {
                role_path: role_path.clone(),
                subject_id: subject_b,
                email: "bob@example.com".to_string(),
            })
            .then_expect_error(JourneyError::RolePathConflict(role_path));
    }

    #[test]
    fn forget_subject_via_subjects_map() {
        // ForgetSubject must work for subjects registered via RegisterSubject
        // (not just the legacy PersonCaptured path).
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
            ])
            .when(JourneyCommand::ForgetSubject { subject_id })
            .then_expect_events(vec![JourneyEvent::SubjectForgotten { subject_id }]);
    }

    #[test]
    fn forget_subject_via_subjects_map_is_idempotent() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectForgotten { subject_id },
            ])
            .when(JourneyCommand::ForgetSubject { subject_id })
            .then_expect_events(vec![]);
    }

    // ── apply() — shared_data accumulation ───────────────────────────────────

    #[test]
    fn test_apply_merges_shared_data() {
        let id = Uuid::new_v4();
        let mut journey = Journey::default();
        journey.apply(JourneyEvent::Started { id });
        journey.apply(JourneyEvent::AttributesSet {
            plaintext: BTreeMap::from([
                ("/origin".parse().unwrap(), json!("LHR")),
                ("/destination".parse().unwrap(), json!("JFK")),
            ]),
            secret_partitions: vec![],
        });
        journey.apply(JourneyEvent::AttributesSet {
            plaintext: BTreeMap::from([("/totalPrice".parse().unwrap(), json!(450.00))]),
            secret_partitions: vec![],
        });

        assert_eq!(journey.shared_data()["origin"], json!("LHR"));
        assert_eq!(journey.shared_data()["destination"], json!("JFK"));
        assert_eq!(journey.shared_data()["totalPrice"], json!(450.00));
    }

    // ── Schema validation ────────────────────────────────────────────────────

    // ── SetAttributes ──────────────────────────────────────────────────────────

    #[test]
    fn set_attributes_requires_started() {
        let mut changes = BTreeMap::new();
        changes.insert("/search/origin".parse().unwrap(), json!("LHR"));

        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn set_attributes_rejects_after_complete() {
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert("/search/origin".parse().unwrap(), json!("LHR"));

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn set_attributes_rejects_empty_changes() {
        let id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes {
                changes: BTreeMap::new(),
            })
            .then_expect_error(JourneyError::InvalidData("no changes".to_string()));
    }

    #[test]
    fn set_attributes_rejects_unknown_path() {
        let id = Uuid::new_v4();
        // Use the explicit (non-permissive) schema; `mystery/field` is not in it.
        let unknown_path: PointerBuf = "/mystery/field".parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(unknown_path.clone(), json!("value"));

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::UnknownAttributePath(vec![unknown_path]));
    }

    #[test]
    fn set_attributes_plaintext_merges_into_shared_data() {
        // Test the apply() side directly: AttributesSet writes path-keyed values
        // into shared_data via assign_all.
        let id = Uuid::new_v4();
        let mut plaintext = BTreeMap::new();
        plaintext.insert("/search/origin".parse().unwrap(), json!("LHR"));
        plaintext.insert("/search/destination".parse().unwrap(), json!("JFK"));

        let mut journey = Journey::default();
        journey.apply(JourneyEvent::Started { id });
        journey.apply(JourneyEvent::AttributesSet {
            plaintext,
            secret_partitions: vec![],
        });

        assert_eq!(journey.shared_data()["search"]["origin"], json!("LHR"));
        assert_eq!(journey.shared_data()["search"]["destination"], json!("JFK"));
    }

    #[test]
    fn set_attributes_secret_requires_person_captured() {
        // The person slot must exist before a secret path targeting it is accepted.
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert(
            "/persons/passenger_0/passport".parse().unwrap(),
            json!("AB123456"),
        );

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
    }

    #[test]
    fn set_attributes_secret_writes_into_shared_data() {
        // apply() writes secret changes into shared_data at their full path.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        let passport_path: PointerBuf = "/persons/passenger_0/passport".parse().unwrap();
        let mut secret_changes = BTreeMap::new();
        secret_changes.insert(passport_path, json!("AB123456"));

        let mut journey = Journey::default();
        journey.apply(JourneyEvent::Started { id });
        journey.apply(JourneyEvent::AttributesSet {
            plaintext: BTreeMap::new(),
            secret_partitions: vec![SecretPartitionData {
                role_path: "/persons/passenger_0".parse().unwrap(),
                subject_id,
                changes: secret_changes,
            }],
        });

        assert_eq!(
            journey.shared_data()["persons"]["passenger_0"]["passport"],
            json!("AB123456")
        );
    }

    #[test]
    fn set_attributes_emits_workflow_evaluated() {
        // Passing `first_name` triggers SimpleDecisionEngine's form_3 action
        // via the evaluate_attributes default impl (current_step = "").
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert("/first_name".parse().unwrap(), json!("Alice"));
        let expected_plaintext = changes.clone();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_events(vec![
                JourneyEvent::AttributesSet {
                    plaintext: expected_plaintext,
                    secret_partitions: vec![],
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec!["form_3".to_string()],
                    phase: None,
                },
            ]);
    }

    #[test]
    fn set_attributes_without_engine_emits_no_workflow_evaluated() {
        // With no decision engine configured, SetAttributes still accumulates
        // attributes but emits only AttributesSet — no WorkflowEvaluated.
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert("/first_name".parse().unwrap(), json!("Alice"));
        let expected_plaintext = changes.clone();

        JourneyTester::with(services_without_engine())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_events(vec![JourneyEvent::AttributesSet {
                plaintext: expected_plaintext,
                secret_partitions: vec![],
            }]);
    }

    #[test]
    fn set_attributes_multi_subject_produces_one_partition_per_subject() {
        // A single SetAttributes touching two subjects' secret paths must emit
        // one SecretPartitionData per subject, sorted by person_ref.
        let id = Uuid::new_v4();
        let subject_id_0 = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let subject_id_1 = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();

        let path_0: PointerBuf = "/persons/passenger_0/passport".parse().unwrap();
        let path_1: PointerBuf = "/persons/passenger_1/passport".parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(path_0.clone(), json!("AB111111"));
        changes.insert(path_1.clone(), json!("CD222222"));

        let mut changes_0 = BTreeMap::new();
        changes_0.insert(path_0, json!("AB111111"));
        let mut changes_1 = BTreeMap::new();
        changes_1.insert(path_1, json!("CD222222"));

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id: subject_id_0,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: "/persons/passenger_0".parse().unwrap(),
                    subject_id: subject_id_0,
                },
                JourneyEvent::SubjectRegistered {
                    subject_id: subject_id_1,
                    email: "bob@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: "/persons/passenger_1".parse().unwrap(),
                    subject_id: subject_id_1,
                },
            ])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_events(vec![
                JourneyEvent::AttributesSet {
                    plaintext: BTreeMap::new(),
                    secret_partitions: vec![
                        SecretPartitionData {
                            role_path: "/persons/passenger_0".parse().unwrap(),
                            subject_id: subject_id_0,
                            changes: changes_0,
                        },
                        SecretPartitionData {
                            role_path: "/persons/passenger_1".parse().unwrap(),
                            subject_id: subject_id_1,
                            changes: changes_1,
                        },
                    ],
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                    phase: None,
                },
            ]);
    }

    // ── SetAttributes via bindings (new path) ─────────────────────────────

    #[test]
    fn set_attributes_resolves_subject_via_bindings() {
        // A secret attribute whose role path exists in `self.bindings` (registered
        // via RegisterAndBindSubject) must be encrypted successfully.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: PointerBuf = "/persons/passenger_0".parse().unwrap();

        let passport_path: PointerBuf = "/persons/passenger_0/passport".parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(passport_path.clone(), json!("AB123456"));

        let mut expected_secret = BTreeMap::new();
        expected_secret.insert(passport_path, json!("AB123456"));

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id,
                },
            ])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_events(vec![
                JourneyEvent::AttributesSet {
                    plaintext: BTreeMap::new(),
                    secret_partitions: vec![SecretPartitionData {
                        role_path,
                        subject_id,
                        changes: expected_secret,
                    }],
                },
                JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec![],
                    phase: None,
                },
            ]);
    }

    #[test]
    fn set_attributes_rejects_secret_path_when_subject_forgotten_via_bindings() {
        // A forgotten subject's role path must not be usable in SetAttributes —
        // their DEK has been deleted and encryption would fail.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        let mut changes = BTreeMap::new();
        changes.insert(
            "/persons/passenger_0/passport".parse().unwrap(),
            json!("AB123456"),
        );

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectRegistered {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: "/persons/passenger_0".parse().unwrap(),
                    subject_id,
                },
                JourneyEvent::SubjectForgotten { subject_id },
            ])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
    }

    #[test]
    fn set_attributes_invalid_data_against_json_schema() {
        // Plaintext changes that violate the JSON Schema must be rejected with
        // InvalidData. The permissive attribute schema classifies every path as
        // Plaintext, so the JSON Schema validator is reached.
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        // The test schema requires `alpha` to be a number; a string fails.
        changes.insert("/alpha".parse().unwrap(), json!("not_a_number"));

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::InvalidData(
                "Schema validation failed: {\"alpha\":\"not_a_number\"} is not valid under any of the schemas listed in the 'oneOf' keyword"
                    .to_string(),
            ));
    }

    #[test]
    fn set_attributes_non_numeric_array_index() {
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert("/nicknames/0".parse().unwrap(), json!("Joey"));
        changes.insert("/nicknames/one".parse().unwrap(), json!("Jimbob"));

        let result = JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .inspect_result();

        assert_matches!(
            result,
            Err(JourneyError::InvalidJsonPointer(
                jsonptr::assign::Error::FailedToParseIndex { .. }
            ))
        );
    }

    #[test]
    fn set_attributes_array_index_out_of_range() {
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert("/nicknames/0".parse().unwrap(), json!("Joey"));
        changes.insert("/nicknames/2".parse().unwrap(), json!("Jimbob"));

        let result = JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .inspect_result();

        assert_matches!(
            result,
            Err(JourneyError::InvalidJsonPointer(
                jsonptr::assign::Error::OutOfBounds { .. }
            ))
        );
    }
}
