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

/// Registration record for a data subject captured via [`JourneyCommand::CaptureSubject`].
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
    /// Shared, non-PII data accumulated from `Capture` commands.
    /// Never encrypted. Fully intact after any shredding operation.
    shared_data: Value,
    /// Per-person slots, keyed by client-assigned `person_ref`.
    /// Retained for backward-compat replay of `PersonCaptured` events.
    persons: BTreeMap<String, PersonSlot>,
    /// Registered subjects, keyed by subject UUID.
    subjects: BTreeMap<Uuid, SubjectRegistration>,
    /// Role-path → subject-UUID bindings established by `BindSubject`.
    bindings: BTreeMap<AttributePath, Uuid>,
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
                email,
                // name and phone were identity fields on the legacy PersonCaptured
                // event; they are not carried forward in the new subject model.
                name: _,
                phone: _,
            } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                // Derive the role path and check for a conflicting binding.
                let role_path: AttributePath = format!("persons/{person_ref}")
                    .parse()
                    .map_err(|_| JourneyError::PersonRefConflict(person_ref.clone()))?;
                if let Some(&existing) = self.bindings.get(&role_path)
                    && existing != subject_id
                {
                    return Err(JourneyError::PersonRefConflict(person_ref));
                }
                // Emit SubjectCaptured if the subject is new or email changed.
                if self
                    .subjects
                    .get(&subject_id)
                    .is_none_or(|reg| reg.email != email)
                {
                    sink.write(JourneyEvent::SubjectCaptured { subject_id, email }, self)
                        .await;
                }
                // Emit SubjectBound if the role path is not yet bound.
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

            JourneyCommand::CapturePersonDetails { person_ref, data } => {
                if self.id == Uuid::default() {
                    return Err(JourneyError::NotFound);
                }
                if JourneyState::Complete == self.state {
                    return Err(JourneyError::AlreadyCompleted);
                }
                // Resolve subject_id: check bindings (new path) then persons (legacy).
                let subject_id = {
                    let role_path: AttributePath = format!("persons/{person_ref}")
                        .parse()
                        .map_err(|_| JourneyError::PersonNotFound(person_ref.clone()))?;
                    self.bindings
                        .get(&role_path)
                        .copied()
                        .or_else(|| self.persons.get(&person_ref).map(|slot| slot.subject_id))
                        .ok_or_else(|| JourneyError::PersonNotFound(person_ref.clone()))?
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
                //
                // subject_lookup resolves a role path (e.g. `"persons/passenger_0"`)
                // to the subject UUID whose DEK encrypts that role's secret fields.
                //
                // Resolution order:
                //   1. `self.bindings` — role paths established by `BindSubject` /
                //      `CaptureAndBindSubject` (new path).
                //   2. `self.persons` — legacy fallback for journeys that still use
                //      `CapturePerson` (backward compat until Layer 6).
                //
                // Forgotten subjects return `None` so their paths land in `unknown`
                // and the command is rejected.
                let schema = services.attribute_schema();
                let classification = {
                    let bindings = &self.bindings;
                    let subjects = &self.subjects;
                    let persons = &self.persons;
                    classify_changes(schema, &changes, |subject_path| {
                        // 1. New path: role-path binding.
                        if let Some(&uuid) = bindings.get(subject_path) {
                            if subjects.get(&uuid).is_some_and(|r| r.forgotten) {
                                return None;
                            }
                            return Some(uuid);
                        }
                        // 2. Legacy fallback: strip "persons/" and look in persons map.
                        subject_path
                            .as_str()
                            .strip_prefix("persons/")
                            .and_then(|person_ref| persons.get(person_ref))
                            .filter(|slot| !slot.forgotten)
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

            JourneyCommand::CaptureSubject { subject_id, email } => {
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
                sink.write(JourneyEvent::SubjectCaptured { subject_id, email }, self)
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

            JourneyCommand::CaptureAndBindSubject {
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
                // Emit SubjectCaptured if new or email changed.
                if self
                    .subjects
                    .get(&subject_id)
                    .is_none_or(|reg| reg.email != email)
                {
                    sink.write(JourneyEvent::SubjectCaptured { subject_id, email }, self)
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
                // Only emit SubjectForgotten if the subject is still active
                // in either the new subjects map or the legacy persons slots.
                // This keeps the shredding endpoint idempotent.
                let needs_forgetting = self
                    .subjects
                    .get(&subject_id)
                    .is_some_and(|reg| !reg.forgotten)
                    || self
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
                name,
                email,
                phone,
            } => {
                // Populate the new subjects/bindings maps so that SetAttributes
                // resolves correctly for journeys using the legacy CapturePerson path.
                let role_path: AttributePath =
                    format!("persons/{person_ref}").parse().unwrap_or_else(|_| {
                        // person_ref values stored in old events are always valid
                        // path segments; this branch exists only for safety.
                        AttributePath::new("persons/unknown").expect("static fallback")
                    });
                self.subjects
                    .entry(subject_id)
                    .and_modify(|reg| reg.email.clone_from(&email))
                    .or_insert_with(|| SubjectRegistration {
                        email: email.clone(),
                        forgotten: false,
                    });
                self.bindings.insert(role_path, subject_id);
                // Also maintain the legacy persons map for backward compat.
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
                // Mark forgotten in the new subjects map.
                if let Some(reg) = self.subjects.get_mut(&subject_id) {
                    reg.forgotten = true;
                }
                // Also mark legacy person slots for backward compat.
                for slot in self.persons.values_mut() {
                    if slot.subject_id == subject_id {
                        slot.forgotten = true;
                    }
                }
            }
            JourneyEvent::SubjectCaptured { subject_id, email } => {
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
    UnknownAttributePath(Vec<AttributePath>),
    #[error("Subject not registered — call CaptureSubject first")]
    SubjectNotRegistered,
    #[error("Role path '{0}' is already bound to a different subject")]
    RolePathConflict(AttributePath),
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

    #[must_use]
    pub const fn subjects(&self) -> &BTreeMap<Uuid, SubjectRegistration> {
        &self.subjects
    }

    #[must_use]
    pub const fn bindings(&self) -> &BTreeMap<AttributePath, Uuid> {
        &self.bindings
    }
}

impl Default for Journey {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            shared_data: json!({}),
            persons: BTreeMap::new(),
            subjects: BTreeMap::new(),
            bindings: BTreeMap::new(),
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
            .then_expect_events(vec![
                JourneyEvent::SubjectCaptured {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: "persons/passenger_0".parse().unwrap(),
                    subject_id,
                },
            ]);
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
            // email changed → SubjectCaptured; binding already exists → no SubjectBound
            .then_expect_events(vec![JourneyEvent::SubjectCaptured {
                subject_id,
                email: "alice.new@example.com".to_string(),
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
            .then_expect_events(vec![
                JourneyEvent::SubjectCaptured {
                    subject_id: subject_b,
                    email: "bob@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: "persons/passenger_1".parse().unwrap(),
                    subject_id: subject_b,
                },
            ]);
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

    // ── CaptureSubject / BindSubject / CaptureAndBindSubject ────────────────

    #[test]
    fn capture_subject_emits_subject_captured() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::CaptureSubject {
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![JourneyEvent::SubjectCaptured {
                subject_id,
                email: "alice@example.com".to_string(),
            }]);
    }

    #[test]
    fn capture_subject_is_idempotent_with_same_email() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
            ])
            .when(JourneyCommand::CaptureSubject {
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![]);
    }

    #[test]
    fn capture_subject_updates_email() {
        // Re-capturing with a different email must emit a new SubjectCaptured.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
                    subject_id,
                    email: "old@example.com".to_string(),
                },
            ])
            .when(JourneyCommand::CaptureSubject {
                subject_id,
                email: "new@example.com".to_string(),
            })
            .then_expect_events(vec![JourneyEvent::SubjectCaptured {
                subject_id,
                email: "new@example.com".to_string(),
            }]);
    }

    #[test]
    fn capture_subject_requires_started() {
        JourneyTester::with(services())
            .given_no_previous_events()
            .when(JourneyCommand::CaptureSubject {
                subject_id: Uuid::new_v4(),
                email: "alice@example.com".to_string(),
            })
            .then_expect_error(JourneyError::NotFound);
    }

    #[test]
    fn capture_subject_rejects_after_complete() {
        let id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }, JourneyEvent::Completed])
            .when(JourneyCommand::CaptureSubject {
                subject_id: Uuid::new_v4(),
                email: "alice@example.com".to_string(),
            })
            .then_expect_error(JourneyError::AlreadyCompleted);
    }

    #[test]
    fn bind_subject_emits_subject_bound() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
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
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
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
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
                    subject_id: subject_a,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectCaptured {
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
                role_path: "persons/passenger_0".parse().unwrap(),
                subject_id,
            })
            .then_expect_error(JourneyError::SubjectNotRegistered);
    }

    #[test]
    fn capture_and_bind_subject_emits_both_events() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![JourneyEvent::Started { id }])
            .when(JourneyCommand::CaptureAndBindSubject {
                role_path: role_path.clone(),
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![
                JourneyEvent::SubjectCaptured {
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
    fn capture_and_bind_subject_is_idempotent() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id,
                },
            ])
            .when(JourneyCommand::CaptureAndBindSubject {
                role_path,
                subject_id,
                email: "alice@example.com".to_string(),
            })
            .then_expect_events(vec![]);
    }

    #[test]
    fn capture_and_bind_subject_rejects_role_path_conflict() {
        let id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
                    subject_id: subject_a,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: role_path.clone(),
                    subject_id: subject_a,
                },
            ])
            .when(JourneyCommand::CaptureAndBindSubject {
                role_path: role_path.clone(),
                subject_id: subject_b,
                email: "bob@example.com".to_string(),
            })
            .then_expect_error(JourneyError::RolePathConflict(role_path));
    }

    #[test]
    fn forget_subject_via_subjects_map() {
        // ForgetSubject must work for subjects registered via CaptureSubject
        // (not just the legacy PersonCaptured path).
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        JourneyTester::with(services())
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
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
                JourneyEvent::SubjectCaptured {
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

    // ── SetAttributes via bindings (new path) ─────────────────────────────

    #[test]
    fn set_attributes_resolves_subject_via_bindings() {
        // A secret attribute whose role path exists in `self.bindings` (registered
        // via CaptureAndBindSubject) must be encrypted successfully.
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let role_path: AttributePath = "persons/passenger_0".parse().unwrap();

        let passport_path: AttributePath = "persons/passenger_0/passport".parse().unwrap();
        let mut changes = BTreeMap::new();
        changes.insert(passport_path.clone(), json!("AB123456"));

        let mut expected_secret = BTreeMap::new();
        expected_secret.insert(passport_path, json!("AB123456"));

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
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
            "persons/passenger_0/passport"
                .parse::<AttributePath>()
                .unwrap(),
            json!("AB123456"),
        );

        JourneyTester::with(services_with_attribute_schema(explicit_attribute_schema()))
            .given(vec![
                JourneyEvent::Started { id },
                JourneyEvent::SubjectCaptured {
                    subject_id,
                    email: "alice@example.com".to_string(),
                },
                JourneyEvent::SubjectBound {
                    role_path: "persons/passenger_0".parse().unwrap(),
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
