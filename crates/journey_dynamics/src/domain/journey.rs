use std::{collections::BTreeMap, sync::Arc};

use cqrs_es::{Aggregate, event_sink::EventSink};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    domain::{
        AttributePath, AttributeSchema,
        attribute_schema::{PiiClass, classify_changes},
        commands::JourneyCommand,
        events::{JourneyEvent, SecretPartitionData},
        json_path::set_at_path,
        rehydrate,
    },
    services::{decision_engine::DecisionEngine, schema_validator::SchemaValidator},
};

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
    ///
    /// Deprecated: the canonical location for per-person attributes is
    /// `shared_data` under `persons/<ref>/…`. This field is retained as a
    /// back-compat mirror: both legacy `CapturePersonDetails` commands and
    /// new `SetAttributes` commands (via the mirror-write in `apply`) keep
    /// it populated, but external readers should prefer `shared_data`.
    #[deprecated(
        since = "0.3.0",
        note = "read from shared_data under persons/<ref>/… instead"
    )]
    pub details: Value,
    /// Set to `true` when a `SubjectForgotten` event is applied for this
    /// subject. The encrypted event payloads become unreadable at the same
    /// time (DEK deleted), so this is primarily a tombstone for the read model.
    pub forgotten: bool,
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

    #[allow(clippy::too_many_lines, deprecated)]
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
                        // The legacy `Capture` arm never carries a phase label.
                        phase: None,
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
                // The subject_lookup resolves "persons/<ref>" → slot UUID.
                let schema = services.attribute_schema();
                let classification = {
                    let persons = &self.persons;
                    classify_changes(schema, &changes, |subject_path| {
                        subject_path
                            .as_str()
                            .strip_prefix("persons/")
                            .and_then(|person_ref| persons.get(person_ref))
                            .map(|slot| slot.subject_id)
                    })
                };

                // Reject paths that are not registered in the schema at all.
                let truly_unknown: Vec<AttributePath> = classification
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
                        .strip_prefix("persons/")
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
                    json_patch::merge(&mut merged_data, &rehydrate(&classification.plaintext));
                    if let Err(e) = services.schema_validator().validate(&merged_data) {
                        return Err(JourneyError::InvalidData(e.to_string()));
                    }
                }

                // Evaluate the workflow with the full (plaintext + secret) change set.
                let decision = services
                    .decision_engine()
                    .evaluate_attributes(self, &changes)
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

    #[allow(deprecated)]
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
            JourneyEvent::AttributesSet {
                plaintext,
                secret_partitions,
            } => {
                // Apply plaintext changes directly into shared_data.
                for (path, value) in &plaintext {
                    set_at_path(&mut self.shared_data, path, value.clone());
                }
                // Apply secret changes.
                for partition in &secret_partitions {
                    // Write every change at its full path into shared_data.
                    for (path, value) in &partition.changes {
                        set_at_path(&mut self.shared_data, path, value.clone());
                    }
                    // Permanent mirror-write into slot.details using the suffix
                    // path (the part after "persons/<ref>/").  This keeps the
                    // legacy `journey_person.details` column populated for
                    // downstream consumers that still read from it.
                    if let Some(person_ref_str) =
                        partition.role_path.as_str().strip_prefix("persons/")
                        && let Some(slot) = self.persons.get_mut(person_ref_str)
                    {
                        let prefix = format!("{}/", partition.role_path.as_str());
                        for (path, value) in &partition.changes {
                            let suffix =
                                path.as_str().strip_prefix(&prefix).unwrap_or(path.as_str());
                            if let Ok(suffix_path) = suffix.parse::<AttributePath>() {
                                set_at_path(&mut slot.details, &suffix_path, value.clone());
                            }
                        }
                    }
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
    #[error("Unknown attribute paths: {0:?}")]
    UnknownAttributePath(Vec<AttributePath>),
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
    #[deprecated(
        since = "0.3.0",
        note = "read WorkflowEvaluated.phase from shared_data instead"
    )]
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
    #![allow(deprecated)]
    use cqrs_es::test::TestFramework;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    use super::*;
    use crate::domain::{
        AttributePath, AttributeSchema, attribute_schema::PiiClass, events::SecretPartitionData,
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
            "search/origin".parse::<AttributePath>().unwrap(),
            PiiClass::Plaintext,
        );
        paths.insert(
            "persons/passenger_0/passport"
                .parse::<AttributePath>()
                .unwrap(),
            PiiClass::Secret {
                subject: "persons/passenger_0".parse().unwrap(),
            },
        );
        paths.insert(
            "persons/passenger_1/passport"
                .parse::<AttributePath>()
                .unwrap(),
            PiiClass::Secret {
                subject: "persons/passenger_1".parse().unwrap(),
            },
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
                    phase: None,
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
                    phase: None,
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
                    phase: None,
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
                    phase: None,
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
                    phase: None,
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
                    phase: None,
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
                    phase: None,
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

    // ── SetAttributes ──────────────────────────────────────────────────────────

    #[test]
    fn set_attributes_requires_started() {
        let mut changes = BTreeMap::new();
        changes.insert(
            "search/origin".parse::<AttributePath>().unwrap(),
            json!("LHR"),
        );

        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn set_attributes_rejects_after_complete() {
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert(
            "search/origin".parse::<AttributePath>().unwrap(),
            json!("LHR"),
        );

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
        let unknown_path: AttributePath = "mystery/field".parse().unwrap();
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
        // into shared_data via set_at_path.
        let id = Uuid::new_v4();
        let mut plaintext = BTreeMap::new();
        plaintext.insert(
            "search/origin".parse::<AttributePath>().unwrap(),
            json!("LHR"),
        );
        plaintext.insert(
            "search/destination".parse::<AttributePath>().unwrap(),
            json!("JFK"),
        );

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
            "persons/passenger_0/passport"
                .parse::<AttributePath>()
                .unwrap(),
            json!("AB123456"),
        );

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::PersonNotFound("passenger_0".to_string()));
    }

    #[test]
    fn set_attributes_secret_writes_under_slot() {
        // apply() should write secret changes both into shared_data (full path)
        // and into slot.details (suffix path after "persons/<ref>/").
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        let passport_path: AttributePath = "persons/passenger_0/passport".parse().unwrap();
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
                role_path: "persons/passenger_0".parse().unwrap(),
                subject_id,
                changes: secret_changes,
            }],
        });

        // Full path is visible in shared_data (persons is an object keyed by person_ref).
        assert_eq!(
            journey.shared_data()["persons"]["passenger_0"]["passport"],
            json!("AB123456")
        );
        // Suffix path is mirrored into slot.details.
        let slot = journey.persons().get("passenger_0").unwrap();
        assert_eq!(slot.details["passport"], json!("AB123456"));
    }

    #[test]
    fn set_attributes_emits_workflow_evaluated() {
        // Passing `first_name` triggers SimpleDecisionEngine's form_3 action
        // via the evaluate_attributes default impl (current_step = "").
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        changes.insert(
            "first_name".parse::<AttributePath>().unwrap(),
            json!("Alice"),
        );
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

        let path_0: AttributePath = "persons/passenger_0/passport".parse().unwrap();
        let path_1: AttributePath = "persons/passenger_1/passport".parse().unwrap();
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
                            role_path: "persons/passenger_0".parse().unwrap(),
                            subject_id: subject_id_0,
                            changes: changes_0,
                        },
                        SecretPartitionData {
                            role_path: "persons/passenger_1".parse().unwrap(),
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

    #[test]
    fn set_attributes_invalid_data_against_json_schema() {
        // Plaintext changes that violate the JSON Schema must be rejected with
        // InvalidData. The permissive attribute schema classifies every path as
        // Plaintext, so the JSON Schema validator is reached.
        let id = Uuid::new_v4();
        let mut changes = BTreeMap::new();
        // The test schema requires `alpha` to be a number; a string fails.
        changes.insert(
            "alpha".parse::<AttributePath>().unwrap(),
            json!("not_a_number"),
        );

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::SetAttributes { changes })
            .then_expect_error(JourneyError::InvalidData(
                "Schema validation failed: {\"alpha\":\"not_a_number\"} is not valid under any of the schemas listed in the 'oneOf' keyword"
                    .to_string(),
            ));
    }

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
