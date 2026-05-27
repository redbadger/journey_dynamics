use std::collections::BTreeMap;

use cqrs_es::DomainEvent;
use cqrs_es_crypto::PiiCodec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::AttributePath;

/// Per-subject secret data carried by an [`JourneyEvent::AttributesSet`] event.
///
/// Each entry corresponds to one person slot whose secret attributes were
/// touched by the originating `SetAttributes` command. From step A7 onwards
/// the `changes` map is encrypted under the subject's DEK; until then it is
/// stored as plaintext (same behaviour as all other non-annotated variants).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretPartitionData {
    /// Journey-local slot name; used as the codec `label` from A7 onwards.
    pub person_ref: String,
    /// The subject's identity key, copied from `PersonSlot.subject_id`.
    pub subject_id: Uuid,
    /// Path → value changes. Encrypted under `subject_id`'s DEK from A7.
    pub changes: BTreeMap<AttributePath, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PiiCodec)]
pub enum JourneyEvent {
    Started {
        id: Uuid,
    },
    Modified {
        step: String,
        data: Value,
    },
    #[pii(event_type = "PersonCaptured")]
    PersonCaptured {
        #[pii(plaintext)]
        person_ref: String,
        #[pii(subject)]
        subject_id: Uuid,
        #[pii(secret)]
        name: String,
        #[pii(secret)]
        email: String,
        #[pii(secret)]
        phone: Option<String>,
    },
    #[pii(event_type = "PersonDetailsUpdated", sentinel = "encrypted_data")]
    PersonDetailsUpdated {
        #[pii(plaintext)]
        person_ref: String,
        #[pii(subject)]
        subject_id: Uuid,
        #[pii(secret)]
        data: Value,
    },
    WorkflowEvaluated {
        suggested_actions: Vec<String>,
    },
    StepProgressed {
        from_step: Option<String>,
        to_step: String,
    },
    Completed,
    SubjectForgotten {
        subject_id: Uuid,
    },
    /// Path-keyed attribute changes produced by a `SetAttributes` command.
    ///
    /// `plaintext` contains all changes that the attribute schema classified
    /// as non-sensitive. `secret_partitions` holds one entry per data subject
    /// whose secret attributes were touched; from A7 onwards each entry's
    /// `changes` map is encrypted under that subject's DEK.
    AttributesSet {
        /// Non-sensitive path → value changes.
        plaintext: BTreeMap<AttributePath, Value>,
        /// One entry per subject whose secret attributes were updated.
        /// Empty when the command set only plaintext attributes.
        #[serde(default)]
        secret_partitions: Vec<SecretPartitionData>,
    },
}

impl DomainEvent for JourneyEvent {
    fn event_type(&self) -> String {
        let event_type: &str = match self {
            Self::Started { .. } => "JourneyOpened",
            Self::Modified { .. } => "JourneyModified",
            Self::PersonCaptured { .. } => "PersonCaptured",
            Self::PersonDetailsUpdated { .. } => "PersonDetailsUpdated",
            Self::WorkflowEvaluated { .. } => "WorkflowEvaluated",
            Self::StepProgressed { .. } => "StepProgressed",
            Self::Completed => "JourneyClosed",
            Self::SubjectForgotten { .. } => "SubjectForgotten",
            Self::AttributesSet { .. } => "AttributesSet",
        };
        event_type.to_string()
    }

    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}
