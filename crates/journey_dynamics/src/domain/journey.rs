use std::{collections::BTreeMap, sync::Arc};

use cqrs_es::{Aggregate, event_sink::EventSink};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    domain::{
        AttributeSchema, assign_all,
        attribute_schema::{PiiClass, classify_changes},
        commands::JourneyCommand,
        events::{JourneyEvent, SecretPartitionData},
    },
    services::{decision_engine::DecisionEngine, schema_validator::SchemaValidator},
};
use jsonptr::PointerBuf;

/// Registration record for a data subject captured via [`JourneyCommand::RegisterSubject`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectRegistration {
    /// Contact email — used for GDPR erasure lookup.
    pub email: String,
    /// Set to `true` once a `SubjectForgotten` event is applied.
    pub forgotten: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    /// Accumulated journey attributes (plaintext and decrypted secret values).
    /// Never encrypted at rest. Fully intact after any shredding operation.
    shared_data: Value,
    /// Registered subjects, keyed by subject UUID.
    subjects: BTreeMap<Uuid, SubjectRegistration>,
    /// Role-path → subject-UUID bindings established by `BindSubject`.
    bindings: BTreeMap<PointerBuf, Uuid>,
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

                // Classify every path against the attribute schema.
                // The subject_lookup resolves a role path to its bound subject
                // UUID via `self.bindings`.  Forgotten subjects return `None` so
                // their paths land in `unknown` and the command is rejected.
                let schema = services.attribute_schema();
                let classification = {
                    let bindings = &self.bindings;
                    let subjects = &self.subjects;
                    classify_changes(schema, &changes, |subject_path| {
                        let uuid = *bindings.get(subject_path)?;
                        if subjects.get(&uuid).is_some_and(|r| r.forgotten) {
                            return None;
                        }
                        Some(uuid)
                    })
                };

                // Reject paths that are not registered in the schema at all.
                let truly_unknown: Vec<PointerBuf> = classification
                    .unknown
                    .iter()
                    .filter(|p| schema.classify(p).is_none())
                    .cloned()
                    .collect();
                if !truly_unknown.is_empty() {
                    return Err(JourneyError::UnknownAttributePath(truly_unknown));
                }

                // Reject secret paths whose person slot hasn't been created yet.
                for path in &classification.unknown {
                    let Some(cls) = schema.classify(path) else {
                        continue;
                    };
                    let PiiClass::Secret { subject } = cls.as_ref() else {
                        continue;
                    };
                    let person_ref = subject
                        .as_str()
                        .strip_prefix("/persons/")
                        .unwrap_or(subject.as_str())
                        .to_string();
                    return Err(JourneyError::PersonNotFound(person_ref));
                }

                // Build one SecretPartitionData per role path, sorted
                // deterministically.  The role path and UUID flow directly from
                // the classification; no reverse map needed.
                let mut secret_partitions: Vec<SecretPartitionData> = classification
                    .secret_by_subject
                    .into_iter()
                    .map(
                        |(role_path, (subject_id, secret_changes))| SecretPartitionData {
                            role_path,
                            subject_id,
                            changes: secret_changes,
                        },
                    )
                    .collect();
                secret_partitions.sort_by(|a, b| a.role_path.cmp(&b.role_path));

                // Validate plaintext changes merged with current shared_data.
                if !classification.plaintext.is_empty() {
                    let mut merged_data = self.shared_data.clone();

                    assign_all(&mut merged_data, &classification.plaintext)?;

                    if let Err(e) = services.schema_validator().validate(&merged_data) {
                        return Err(JourneyError::InvalidData(e.to_string()));
                    }
                }

                // Evaluate the workflow with the full (plaintext + secret) change set.
                let decision = services
                    .decision_engine()
                    .evaluate_attributes(self.shared_data(), &changes)
                    .await
                    .map_err(|e| JourneyError::DecisionEngineError(e.to_string()))?;

                sink.write(
                    JourneyEvent::AttributesSet {
                        plaintext: classification.plaintext,
                        secret_partitions,
                    },
                    self,
                )
                .await;

                sink.write(
                    JourneyEvent::WorkflowEvaluated {
                        suggested_actions: decision.suggested_actions,
                        phase: decision.phase,
                    },
                    self,
                )
                .await;

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
                if self
                    .subjects
                    .get(&subject_id)
                    .is_some_and(|reg| reg.email == email)
                {
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
                if !self.subjects.contains_key(&subject_id) {
                    return Err(JourneyError::SubjectNotRegistered);
                }
                match self.bindings.get(&role_path) {
                    Some(&existing) if existing != subject_id => {
                        return Err(JourneyError::RolePathConflict(role_path));
                    }
                    Some(_) => return Ok(()), // same subject — idempotent
                    None => {}
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
                if let Some(&existing) = self.bindings.get(&role_path)
                    && existing != subject_id
                {
                    return Err(JourneyError::RolePathConflict(role_path));
                }
                // Emit SubjectRegistered if new or email changed.
                if self
                    .subjects
                    .get(&subject_id)
                    .is_none_or(|reg| reg.email != email)
                {
                    sink.write(JourneyEvent::SubjectRegistered { subject_id, email }, self)
                        .await;
                }
                // Emit SubjectBound if not already bound.
                if !self.bindings.contains_key(&role_path) {
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
                let needs_forgetting = self
                    .subjects
                    .get(&subject_id)
                    .is_some_and(|reg| !reg.forgotten);
                if needs_forgetting {
                    sink.write(JourneyEvent::SubjectForgotten { subject_id }, self)
                        .await;
                }
                Ok(())
            }
        }
    }

    #[allow(deprecated, clippy::too_many_lines)]
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
                email,
                ..
            } => {
                // Populate subjects/bindings so that SetAttributes and
                // ForgetSubject resolve correctly when replaying legacy events.
                let role_path: PointerBuf = PointerBuf::parse(format!("/persons/{person_ref}"))
                    .unwrap_or_else(|_| {
                        PointerBuf::parse("/persons/unknown").expect("static fallback")
                    });
                self.subjects
                    .entry(subject_id)
                    .and_modify(|reg| reg.email.clone_from(&email))
                    .or_insert_with(|| SubjectRegistration {
                        email: email.clone(),
                        forgotten: false,
                    });
                self.bindings.insert(role_path, subject_id);
            }
            JourneyEvent::PersonDetailsUpdated { .. } | JourneyEvent::StepProgressed { .. } => {
                // Legacy events. Projected directly from event payloads by
                // StructuredJourneyViewRepository; no write-side state to update.
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
                if let Some(reg) = self.subjects.get_mut(&subject_id) {
                    reg.forgotten = true;
                }
            }
            JourneyEvent::SubjectRegistered { subject_id, email } => {
                self.subjects
                    .entry(subject_id)
                    .and_modify(|reg| reg.email.clone_from(&email))
                    .or_insert_with(|| SubjectRegistration {
                        email,
                        forgotten: false,
                    });
            }
            JourneyEvent::SubjectBound {
                role_path,
                subject_id,
            } => {
                self.bindings.insert(role_path, subject_id);
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

pub struct JourneyServices {
    decision_engine: Arc<dyn DecisionEngine>,
    schema_validator: Arc<dyn SchemaValidator>,
    attribute_schema: Arc<AttributeSchema>,
}

impl JourneyServices {
    pub fn new(
        decision_engine: Arc<dyn DecisionEngine>,
        schema_validator: Arc<dyn SchemaValidator>,
        attribute_schema: Arc<AttributeSchema>,
    ) -> Self {
        Self {
            decision_engine,
            schema_validator,
            attribute_schema,
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
        &self.subjects
    }

    #[must_use]
    pub const fn bindings(&self) -> &BTreeMap<PointerBuf, Uuid> {
        &self.bindings
    }
}

impl Default for Journey {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            shared_data: json!({}),
            subjects: BTreeMap::new(),
            bindings: BTreeMap::new(),
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
                JourneyEvent::Modified {
                    step: "first_name".to_string(),
                    data: json!("Joe"),
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
        journey.apply(JourneyEvent::PersonCaptured {
            person_ref: "passenger_0".to_string(),
            subject_id,
            name: "Alice Smith".to_string(),
            email: "alice@example.com".to_string(),
            phone: None,
        });
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
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_0".to_string(),
                    subject_id: subject_id_0,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_1".to_string(),
                    subject_id: subject_id_1,
                    name: "Bob Jones".to_string(),
                    email: "bob@example.com".to_string(),
                    phone: None,
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

    #[test]
    fn set_attributes_rejects_secret_path_when_subject_forgotten_via_person_captured() {
        // A PersonCaptured event followed by SubjectForgotten must prevent
        // SetAttributes from using that subject's secret paths. This validates
        // that PersonCaptured.apply() correctly populates self.subjects (not
        // just self.bindings) so that the forgotten check in SetAttributes fires.
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
                JourneyEvent::PersonCaptured {
                    person_ref: "passenger_0".to_string(),
                    subject_id,
                    name: "Alice Smith".to_string(),
                    email: "alice@example.com".to_string(),
                    phone: None,
                },
                JourneyEvent::SubjectForgotten { subject_id },
            ])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
    }
}
