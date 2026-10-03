//! The generic capture aggregate.
//!
//! [`CaptureAggregate`] is a ready-made `cqrs_es::Aggregate` implementing the
//! progressive-capture spine: start, path-keyed `SetAttributes`, the subject
//! lifecycle (register / bind / register-and-bind / rebind / forget), and
//! complete. A new domain instantiates it with a [`CaptureConfig`] (which
//! supplies the aggregate `TYPE`) plus [`CaptureServices`] (attribute schema,
//! validator, and an optional decision engine) — no aggregate code of its own.
//!
//! The command, event and error enums are shared across domains: a domain's
//! specificity lives in its attribute schema, JSON schema, optional rules, and
//! views — not in new aggregate variants.

use std::{collections::BTreeMap, marker::PhantomData, sync::Arc};

use cqrs_es::{Aggregate, DomainEvent, event_sink::EventSink};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use jsonptr::PointerBuf;

use crate::{
    attribute_schema::AttributeSchema,
    capture::{CaptureError as PipelineError, capture},
    decision_engine::DecisionEngine,
    json_path::assign_all,
    schema_validator::SchemaValidator,
    subject_registry::{SubjectError, SubjectRegistration, SubjectRegistry},
};

/// Per-domain configuration for a [`CaptureAggregate`].
///
/// Supplies the `aggregate_type` string used by the event store. A domain
/// implements this on a zero-sized marker type.
pub trait CaptureConfig: Send + Sync + 'static {
    /// The `cqrs_es::Aggregate::TYPE` for this domain's capture aggregate.
    const TYPE: &'static str;
}

/// Lifecycle state of a capture aggregate.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CaptureState {
    /// Open for attribute capture.
    #[default]
    InProgress,
    /// Closed; further attribute changes are rejected.
    Complete,
}

/// The latest decision-engine result accumulated on the aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionState {
    /// Actions suggested by the decision engine.
    pub suggested_actions: Vec<String>,
    /// Optional phase label from the decision engine.
    pub phase: Option<String>,
}

/// Per-role secret data carried by a [`CaptureEvent::AttributesSet`] event.
///
/// Each entry corresponds to one role path (e.g. `/persons/passenger_0`) whose
/// secret attributes were touched. The `changes` map is encrypted under the
/// subject's DEK; `role_path` doubles as the crypto label (AAD).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretPartitionData {
    /// Full schema path at which the subject is bound.
    pub role_path: PointerBuf,
    /// The subject's identity key — used to look up the DEK.
    pub subject_id: Uuid,
    /// Path → value changes, encrypted under `subject_id`'s DEK.
    #[serde(default)]
    pub changes: BTreeMap<PointerBuf, Value>,
}

/// Commands accepted by the capture aggregate.
#[derive(Debug, Deserialize)]
pub enum CaptureCommand {
    /// Create a new aggregate instance.
    Start {
        /// The aggregate id.
        id: Uuid,
    },
    /// Set one or more attributes in a single command (path-keyed).
    SetAttributes {
        /// Flat map of JSON Pointer → value.
        changes: BTreeMap<PointerBuf, Value>,
    },
    /// Register a data subject (email → `subject_id`).
    RegisterSubject {
        /// Subject identity.
        subject_id: Uuid,
        /// Contact email, for GDPR erasure lookup.
        email: String,
    },
    /// Bind a registered subject to a role path.
    BindSubject {
        /// The role path, e.g. `/persons/passenger_0`.
        role_path: PointerBuf,
        /// The subject to bind.
        subject_id: Uuid,
    },
    /// Register a subject and bind it to a role path in one command.
    RegisterAndBindSubject {
        /// The role path to bind.
        role_path: PointerBuf,
        /// Subject identity.
        subject_id: Uuid,
        /// Contact email.
        email: String,
    },
    /// Move a role path to a different subject, registering it if needed.
    ///
    /// For repairing a role bound to the wrong subject. Idempotent when the role
    /// is already bound to `subject_id`, and on an unbound role it behaves as
    /// [`RegisterAndBindSubject`](Self::RegisterAndBindSubject) — nothing can
    /// have been encrypted under a binding that never existed.
    ///
    /// Refused with [`CaptureError::RebindRefused`] once this aggregate has
    /// written a secret partition under the role's current subject. That guard
    /// is **per-aggregate only**: ciphertext written under the same subject by
    /// any *other* aggregate is invisible to it, and state restored from a
    /// snapshot that predates the guard reads as having written no secrets. The
    /// caller must confirm in its key store that the current subject holds no
    /// DEK at all before issuing this.
    RebindSubject {
        /// The role path to move.
        role_path: PointerBuf,
        /// The subject to bind it to.
        subject_id: Uuid,
        /// Contact email, recorded if the subject is new or its email changed.
        email: String,
    },
    /// Mark the aggregate complete.
    Complete,
    /// Emit a `SubjectForgotten` audit event (called after the DEK is deleted).
    ForgetSubject {
        /// The subject that was forgotten.
        subject_id: Uuid,
    },
}

/// Events emitted by the capture aggregate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CaptureEvent {
    /// The aggregate was started.
    Started {
        /// The aggregate id.
        id: Uuid,
    },
    /// A decision engine produced a result.
    WorkflowEvaluated {
        /// Suggested actions.
        suggested_actions: Vec<String>,
        /// Optional phase label; `None` for pre-1.1 payloads.
        #[serde(default)]
        phase: Option<String>,
    },
    /// The aggregate was completed.
    Completed,
    /// A subject was forgotten (shredded).
    SubjectForgotten {
        /// The forgotten subject.
        subject_id: Uuid,
    },
    /// A data subject was registered.
    SubjectRegistered {
        /// Subject identity.
        subject_id: Uuid,
        /// Contact email.
        email: String,
    },
    /// A registered subject was bound to a role path.
    SubjectBound {
        /// The role path.
        role_path: PointerBuf,
        /// The subject bound to it.
        subject_id: Uuid,
    },
    /// Path-keyed attribute changes produced by a `SetAttributes` command.
    AttributesSet {
        /// Non-sensitive path → value changes.
        plaintext: BTreeMap<PointerBuf, Value>,
        /// One entry per subject whose secret attributes were updated.
        #[serde(default)]
        secret_partitions: Vec<SecretPartitionData>,
    },
}

impl DomainEvent for CaptureEvent {
    fn event_type(&self) -> String {
        match self {
            Self::Started { .. } => "Started",
            Self::WorkflowEvaluated { .. } => "WorkflowEvaluated",
            Self::Completed => "Completed",
            Self::SubjectForgotten { .. } => "SubjectForgotten",
            Self::SubjectRegistered { .. } => "SubjectRegistered",
            Self::SubjectBound { .. } => "SubjectBound",
            Self::AttributesSet { .. } => "AttributesSet",
        }
        .to_string()
    }

    fn event_version(&self) -> String {
        match self {
            // 1.1 added `phase`; old 1.0 payloads deserialise to `phase: None`.
            Self::WorkflowEvaluated { .. } => "1.1".to_string(),
            _ => "1.0".to_string(),
        }
    }
}

/// Errors produced by the capture aggregate.
#[derive(Error, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// The aggregate has not been started.
    #[error("aggregate not found")]
    NotFound,
    /// `Start` was issued for an already-started aggregate.
    #[error("aggregate already started")]
    AlreadyStarted,
    /// A mutating command was issued after completion.
    #[error("aggregate already completed")]
    AlreadyCompleted,
    /// The decision engine failed.
    #[error("decision engine error: {0}")]
    DecisionEngine(String),
    /// Data failed schema validation.
    #[error("invalid data: {0}")]
    InvalidData(String),
    /// A secret path's role is not bound to an active subject.
    #[error("no active subject bound for role path '{0}'")]
    SubjectNotResolved(PointerBuf),
    /// One or more paths are not described by the attribute schema.
    #[error("unknown attribute paths: {0:?}")]
    UnknownAttributePath(Vec<PointerBuf>),
    /// A change pointer could not be assigned.
    #[error("invalid JSON pointer: {0}")]
    InvalidJsonPointer(#[from] jsonptr::assign::Error),
    /// A bind was attempted for an unregistered subject.
    #[error("subject not registered — call RegisterSubject first")]
    SubjectNotRegistered,
    /// A role path is already bound to a different subject.
    #[error("role path '{0}' is already bound to a different subject")]
    RolePathConflict(PointerBuf),
    /// A rebind was refused: this aggregate wrote secrets under the role's
    /// current subject.
    ///
    /// Per-aggregate only — see [`CaptureCommand::RebindSubject`] for what the
    /// guard cannot see.
    #[error("role path '{0}' cannot be rebound: secrets were written under its current subject")]
    RebindRefused(PointerBuf),
}

impl From<SubjectError> for CaptureError {
    fn from(err: SubjectError) -> Self {
        match err {
            SubjectError::NotRegistered => Self::SubjectNotRegistered,
            SubjectError::RolePathConflict(role_path) => Self::RolePathConflict(role_path),
            SubjectError::RebindRefused(role_path) => Self::RebindRefused(role_path),
        }
    }
}

impl From<PipelineError> for CaptureError {
    fn from(err: PipelineError) -> Self {
        match err {
            PipelineError::UnknownAttributePath(paths) => Self::UnknownAttributePath(paths),
            PipelineError::SubjectNotResolved(role_path) => Self::SubjectNotResolved(role_path),
            PipelineError::InvalidJsonPointer(e) => Self::InvalidJsonPointer(e),
            PipelineError::InvalidData(msg) => Self::InvalidData(msg),
            PipelineError::DecisionEngine(msg) => Self::DecisionEngine(msg),
        }
    }
}

/// The collaborators a capture aggregate needs: attribute schema, validator,
/// and an optional decision engine.
pub struct CaptureServices {
    decision_engine: Option<Arc<dyn DecisionEngine>>,
    schema_validator: Arc<dyn SchemaValidator>,
    attribute_schema: Arc<AttributeSchema>,
}

impl CaptureServices {
    /// Construct with a decision engine (the common case).
    #[must_use]
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

    /// Construct without a decision engine — capture runs but emits no
    /// `WorkflowEvaluated` event.
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

    /// The configured decision engine, if any.
    #[must_use]
    pub fn decision_engine(&self) -> Option<&Arc<dyn DecisionEngine>> {
        self.decision_engine.as_ref()
    }

    /// The schema validator.
    #[must_use]
    pub fn schema_validator(&self) -> &Arc<dyn SchemaValidator> {
        &self.schema_validator
    }

    /// The attribute schema.
    #[must_use]
    pub const fn attribute_schema(&self) -> &Arc<AttributeSchema> {
        &self.attribute_schema
    }
}

/// The generic progressive-capture aggregate, parameterised by a domain
/// [`CaptureConfig`].
///
/// `Clone`/`Debug` are implemented by hand so they do not impose `C: Clone` /
/// `C: Debug` bounds — the config is a zero-sized marker.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct CaptureAggregate<C: CaptureConfig> {
    id: Uuid,
    state: CaptureState,
    /// Accumulated attributes (plaintext and decrypted secret values). Never
    /// encrypted at rest; intact after any shredding operation.
    shared_data: Value,
    /// Registered subjects and their role-path → subject bindings.
    #[serde(flatten)]
    registry: SubjectRegistry,
    latest_workflow_decision: Option<WorkflowDecisionState>,
    #[serde(skip)]
    _config: PhantomData<C>,
}

impl<C: CaptureConfig> Clone for CaptureAggregate<C> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            state: self.state,
            shared_data: self.shared_data.clone(),
            registry: self.registry.clone(),
            latest_workflow_decision: self.latest_workflow_decision.clone(),
            _config: PhantomData,
        }
    }
}

impl<C: CaptureConfig> std::fmt::Debug for CaptureAggregate<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureAggregate")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("shared_data", &self.shared_data)
            .field("registry", &self.registry)
            .field("latest_workflow_decision", &self.latest_workflow_decision)
            .finish()
    }
}

impl<C: CaptureConfig> Default for CaptureAggregate<C> {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: CaptureState::default(),
            shared_data: json!({}),
            registry: SubjectRegistry::default(),
            latest_workflow_decision: None,
            _config: PhantomData,
        }
    }
}

impl<C: CaptureConfig> CaptureAggregate<C> {
    /// The aggregate id.
    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    /// The lifecycle state.
    #[must_use]
    pub const fn state(&self) -> CaptureState {
        self.state
    }

    /// The accumulated plaintext + decrypted attribute bag.
    #[must_use]
    pub const fn shared_data(&self) -> &Value {
        &self.shared_data
    }

    /// The latest decision-engine result, if any.
    #[must_use]
    pub const fn latest_workflow_decision(&self) -> Option<&WorkflowDecisionState> {
        self.latest_workflow_decision.as_ref()
    }

    /// All registered subjects.
    #[must_use]
    pub const fn subjects(&self) -> &BTreeMap<Uuid, SubjectRegistration> {
        self.registry.subjects()
    }

    /// All role-path → subject bindings.
    #[must_use]
    pub const fn bindings(&self) -> &BTreeMap<PointerBuf, Uuid> {
        self.registry.bindings()
    }
}

impl<C: CaptureConfig> Aggregate for CaptureAggregate<C> {
    type Command = CaptureCommand;
    type Event = CaptureEvent;
    type Error = CaptureError;
    type Services = CaptureServices;

    const TYPE: &'static str = C::TYPE;

    #[allow(clippy::too_many_lines)]
    async fn handle(
        &mut self,
        command: Self::Command,
        services: &Self::Services,
        sink: &EventSink<Self>,
    ) -> Result<(), Self::Error> {
        match command {
            CaptureCommand::Start { id } => {
                if self.id == id {
                    Err(CaptureError::AlreadyStarted)
                } else {
                    sink.write(CaptureEvent::Started { id }, self).await;
                    Ok(())
                }
            }

            CaptureCommand::SetAttributes { changes } => {
                if self.id == Uuid::default() {
                    return Err(CaptureError::NotFound);
                }
                if CaptureState::Complete == self.state {
                    return Err(CaptureError::AlreadyCompleted);
                }
                if changes.is_empty() {
                    return Err(CaptureError::InvalidData("no changes".to_string()));
                }

                let outcome = capture(
                    services.attribute_schema(),
                    &self.registry,
                    self.shared_data(),
                    &changes,
                    services.schema_validator().as_ref(),
                    services.decision_engine().map(|engine| &**engine),
                )
                .await?;

                sink.write(
                    CaptureEvent::AttributesSet {
                        plaintext: outcome.plaintext,
                        secret_partitions: outcome
                            .secret
                            .into_iter()
                            .map(|slice| SecretPartitionData {
                                role_path: slice.role_path,
                                subject_id: slice.subject_id,
                                changes: slice.changes,
                            })
                            .collect(),
                    },
                    self,
                )
                .await;

                if let Some(decision) = outcome.decision {
                    sink.write(
                        CaptureEvent::WorkflowEvaluated {
                            suggested_actions: decision.suggested_actions,
                            phase: decision.phase,
                        },
                        self,
                    )
                    .await;
                }

                Ok(())
            }

            CaptureCommand::Complete => {
                if self.id == Uuid::default() {
                    Err(CaptureError::NotFound)
                } else if CaptureState::Complete == self.state {
                    Err(CaptureError::AlreadyCompleted)
                } else {
                    sink.write(CaptureEvent::Completed, self).await;
                    Ok(())
                }
            }

            CaptureCommand::RegisterSubject { subject_id, email } => {
                if self.id == Uuid::default() {
                    return Err(CaptureError::NotFound);
                }
                if CaptureState::Complete == self.state {
                    return Err(CaptureError::AlreadyCompleted);
                }
                if !self.registry.needs_registration(&subject_id, &email) {
                    return Ok(());
                }
                sink.write(CaptureEvent::SubjectRegistered { subject_id, email }, self)
                    .await;
                Ok(())
            }

            CaptureCommand::BindSubject {
                role_path,
                subject_id,
            } => {
                if self.id == Uuid::default() {
                    return Err(CaptureError::NotFound);
                }
                if CaptureState::Complete == self.state {
                    return Err(CaptureError::AlreadyCompleted);
                }
                if !self.registry.is_registered(&subject_id) {
                    return Err(CaptureError::SubjectNotRegistered);
                }
                match self.registry.check_binding(&role_path, &subject_id) {
                    Err(e) => return Err(e.into()),
                    Ok(false) => return Ok(()), // same subject — idempotent
                    Ok(true) => {}
                }
                sink.write(
                    CaptureEvent::SubjectBound {
                        role_path,
                        subject_id,
                    },
                    self,
                )
                .await;
                Ok(())
            }

            CaptureCommand::RegisterAndBindSubject {
                role_path,
                subject_id,
                email,
            } => {
                if self.id == Uuid::default() {
                    return Err(CaptureError::NotFound);
                }
                if CaptureState::Complete == self.state {
                    return Err(CaptureError::AlreadyCompleted);
                }
                // Validate the binding upfront before emitting any events.
                self.registry.check_binding(&role_path, &subject_id)?;
                // Emit SubjectRegistered if new or email changed.
                if self.registry.needs_registration(&subject_id, &email) {
                    sink.write(CaptureEvent::SubjectRegistered { subject_id, email }, self)
                        .await;
                }
                // Emit SubjectBound if not already bound.
                if self.registry.binding(&role_path).is_none() {
                    sink.write(
                        CaptureEvent::SubjectBound {
                            role_path,
                            subject_id,
                        },
                        self,
                    )
                    .await;
                }
                Ok(())
            }

            CaptureCommand::RebindSubject {
                role_path,
                subject_id,
                email,
            } => {
                if self.id == Uuid::default() {
                    return Err(CaptureError::NotFound);
                }
                if CaptureState::Complete == self.state {
                    return Err(CaptureError::AlreadyCompleted);
                }
                if !self.registry.check_rebind(&role_path, &subject_id)? {
                    return Ok(()); // same subject — idempotent
                }
                if self.registry.needs_registration(&subject_id, &email) {
                    sink.write(CaptureEvent::SubjectRegistered { subject_id, email }, self)
                        .await;
                }
                // The existing event, not a new one: `apply` replaces a binding
                // outright, so a second `SubjectBound` for the role *is* a move.
                sink.write(
                    CaptureEvent::SubjectBound {
                        role_path,
                        subject_id,
                    },
                    self,
                )
                .await;
                Ok(())
            }

            CaptureCommand::ForgetSubject { subject_id } => {
                if self.id == Uuid::default() {
                    return Err(CaptureError::NotFound);
                }
                // Only emit if the subject is still active — keeps shredding idempotent.
                if self.registry.needs_forgetting(&subject_id) {
                    sink.write(CaptureEvent::SubjectForgotten { subject_id }, self)
                        .await;
                }
                Ok(())
            }
        }
    }

    fn apply(&mut self, event: Self::Event) {
        match event {
            CaptureEvent::Started { id } => {
                self.id = id;
                self.state = CaptureState::InProgress;
            }
            CaptureEvent::AttributesSet {
                plaintext,
                secret_partitions,
            } => {
                assign_all(&mut self.shared_data, &plaintext).unwrap();
                for partition in &secret_partitions {
                    assign_all(&mut self.shared_data, &partition.changes)
                        .expect("events should have valid JSON pointers");
                    self.registry.record_secret(partition.subject_id);
                }
            }
            CaptureEvent::WorkflowEvaluated {
                suggested_actions,
                phase,
            } => {
                self.latest_workflow_decision = Some(WorkflowDecisionState {
                    suggested_actions,
                    phase,
                });
            }
            CaptureEvent::Completed => {
                self.state = CaptureState::Complete;
            }
            CaptureEvent::SubjectForgotten { subject_id } => {
                self.registry.forget(&subject_id);
            }
            CaptureEvent::SubjectRegistered { subject_id, email } => {
                self.registry.register(subject_id, email);
            }
            CaptureEvent::SubjectBound {
                role_path,
                subject_id,
            } => {
                self.registry.bind(role_path, subject_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use cqrs_es::test::TestFramework;
    use serde_json::json;

    use super::*;
    use crate::{attribute_schema::NamespacePattern, schema_validator::NoOpValidator};

    struct TestConfig;

    impl CaptureConfig for TestConfig {
        const TYPE: &'static str = "Test";
    }

    type Tester = TestFramework<CaptureAggregate<TestConfig>>;

    fn ptr(s: &str) -> PointerBuf {
        PointerBuf::parse(s).unwrap()
    }

    /// Permissive schema with a `/persons/<ref>/<field>` secret namespace, so a
    /// write under `/persons/0` is encrypted under the subject bound there.
    fn services() -> CaptureServices {
        let schema =
            AttributeSchema::permissive().with_namespace_patterns(vec![NamespacePattern {
                prefix: ptr("/persons"),
                plaintext_suffixes: BTreeSet::new(),
            }]);
        CaptureServices::without_decision_engine(Arc::new(NoOpValidator), Arc::new(schema))
    }

    fn started_and_bound(id: Uuid, subject_id: Uuid) -> Vec<CaptureEvent> {
        vec![
            CaptureEvent::Started { id },
            CaptureEvent::SubjectRegistered {
                subject_id,
                email: String::new(),
            },
            CaptureEvent::SubjectBound {
                role_path: ptr("/persons/0"),
                subject_id,
            },
        ]
    }

    fn rebind(subject_id: Uuid) -> CaptureCommand {
        CaptureCommand::RebindSubject {
            role_path: ptr("/persons/0"),
            subject_id,
            email: String::new(),
        }
    }

    #[test]
    fn rebind_registers_and_binds_the_new_subject() {
        let (id, old, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        Tester::with(services())
            .given(started_and_bound(id, old))
            .when(rebind(new))
            .then_expect_events(vec![
                CaptureEvent::SubjectRegistered {
                    subject_id: new,
                    email: String::new(),
                },
                CaptureEvent::SubjectBound {
                    role_path: ptr("/persons/0"),
                    subject_id: new,
                },
            ]);
    }

    #[test]
    fn replaying_a_rebind_yields_the_new_binding() {
        let (id, old, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut aggregate = CaptureAggregate::<TestConfig>::default();
        for event in started_and_bound(id, old) {
            aggregate.apply(event);
        }
        aggregate.apply(CaptureEvent::SubjectRegistered {
            subject_id: new,
            email: String::new(),
        });
        aggregate.apply(CaptureEvent::SubjectBound {
            role_path: ptr("/persons/0"),
            subject_id: new,
        });
        assert_eq!(aggregate.bindings().get(&ptr("/persons/0")), Some(&new));
    }

    #[test]
    fn rebind_to_an_already_registered_subject_emits_only_the_binding() {
        let (id, old, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut given = started_and_bound(id, old);
        given.push(CaptureEvent::SubjectRegistered {
            subject_id: new,
            email: String::new(),
        });
        Tester::with(services())
            .given(given)
            .when(rebind(new))
            .then_expect_events(vec![CaptureEvent::SubjectBound {
                role_path: ptr("/persons/0"),
                subject_id: new,
            }]);
    }

    #[test]
    fn rebind_to_the_same_subject_is_a_no_op() {
        let (id, subject) = (Uuid::new_v4(), Uuid::new_v4());
        Tester::with(services())
            .given(started_and_bound(id, subject))
            .when(rebind(subject))
            .then_expect_events(vec![]);
    }

    #[test]
    fn rebind_of_an_unbound_role_binds_it() {
        let (id, subject) = (Uuid::new_v4(), Uuid::new_v4());
        Tester::with(services())
            .given(vec![CaptureEvent::Started { id }])
            .when(rebind(subject))
            .then_expect_events(vec![
                CaptureEvent::SubjectRegistered {
                    subject_id: subject,
                    email: String::new(),
                },
                CaptureEvent::SubjectBound {
                    role_path: ptr("/persons/0"),
                    subject_id: subject,
                },
            ]);
    }

    #[test]
    fn rebind_is_refused_after_a_secret_write_under_the_current_subject() {
        let (id, old, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut given = started_and_bound(id, old);
        given.push(CaptureEvent::AttributesSet {
            plaintext: BTreeMap::new(),
            secret_partitions: vec![SecretPartitionData {
                role_path: ptr("/persons/0"),
                subject_id: old,
                changes: BTreeMap::from([(ptr("/persons/0/passport"), json!("AB123456"))]),
            }],
        });
        Tester::with(services())
            .given(given)
            .when(rebind(new))
            .then_expect_error(CaptureError::RebindRefused(ptr("/persons/0")));
    }

    #[test]
    fn rebind_is_allowed_when_only_plaintext_was_written() {
        let (id, old, new) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut given = started_and_bound(id, old);
        given.push(CaptureEvent::AttributesSet {
            plaintext: BTreeMap::from([(ptr("/search/origin"), json!("LHR"))]),
            secret_partitions: vec![],
        });
        Tester::with(services())
            .given(given)
            .when(rebind(new))
            .then_expect_events(vec![
                CaptureEvent::SubjectRegistered {
                    subject_id: new,
                    email: String::new(),
                },
                CaptureEvent::SubjectBound {
                    role_path: ptr("/persons/0"),
                    subject_id: new,
                },
            ]);
    }

    #[test]
    fn rebind_before_start_is_not_found() {
        Tester::with(services())
            .given_no_previous_events()
            .when(rebind(Uuid::new_v4()))
            .then_expect_error(CaptureError::NotFound);
    }

    #[test]
    fn rebind_after_completion_is_refused() {
        let (id, subject) = (Uuid::new_v4(), Uuid::new_v4());
        let mut given = started_and_bound(id, subject);
        given.push(CaptureEvent::Completed);
        Tester::with(services())
            .given(given)
            .when(rebind(Uuid::new_v4()))
            .then_expect_error(CaptureError::AlreadyCompleted);
    }
}
