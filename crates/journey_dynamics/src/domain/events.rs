use std::collections::BTreeMap;

use cqrs_es::DomainEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use jsonptr::PointerBuf;

/// Per-role secret data carried by an [`JourneyEvent::AttributesSet`] event.
///
/// Each entry corresponds to one role path (e.g. `"/persons/passenger_0"`) whose
/// secret attributes were touched by the originating `SetAttributes` command.
/// The `changes` map is encrypted under the subject's DEK; `role_path` is used
/// as the crypto label (AAD) so the partition identity is meaningful on the
/// read path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretPartitionData {
    /// Full schema path at which the subject is bound, e.g. `"/persons/passenger_0"`.
    /// Used as the crypto label (AAD).
    pub role_path: PointerBuf,
    /// The subject's identity key — used to look up the DEK.
    pub subject_id: Uuid,
    /// Path → value changes. Encrypted under `subject_id`'s DEK.
    #[serde(default)]
    pub changes: BTreeMap<PointerBuf, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JourneyEvent {
    Started {
        id: Uuid,
    },
    WorkflowEvaluated {
        suggested_actions: Vec<String>,
        /// Phase label from the decision engine; `None` for events written
        /// before schema version 1.1.
        #[serde(default)]
        phase: Option<String>,
    },
    Completed,
    SubjectForgotten {
        subject_id: Uuid,
    },
    /// A data subject was registered in this journey.
    SubjectRegistered {
        subject_id: Uuid,
        email: String,
    },
    /// A registered subject was bound to a role path within this journey.
    SubjectBound {
        role_path: PointerBuf,
        subject_id: Uuid,
    },
    /// Path-keyed attribute changes produced by a `SetAttributes` command.
    ///
    /// `plaintext` contains all changes that the attribute schema classified
    /// as non-sensitive. `secret_partitions` holds one entry per data subject
    /// whose secret attributes were touched; each entry's `changes` map is
    /// encrypted under that subject's DEK.
    AttributesSet {
        /// Non-sensitive path → value changes.
        plaintext: BTreeMap<PointerBuf, Value>,
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
            Self::WorkflowEvaluated { .. } => "WorkflowEvaluated",
            Self::Completed => "JourneyClosed",
            Self::SubjectForgotten { .. } => "SubjectForgotten",
            Self::SubjectRegistered { .. } => "SubjectRegistered",
            Self::SubjectBound { .. } => "SubjectBound",
            Self::AttributesSet { .. } => "AttributesSet",
        };
        event_type.to_string()
    }

    fn event_version(&self) -> String {
        match self {
            // Bumped to 1.1 when `phase` was added (step B1).
            // Old 1.0 payloads (no `phase`) deserialise to `phase: None`
            // via `#[serde(default)]` on the field.
            Self::WorkflowEvaluated { .. } => "1.1".to_string(),
            _ => "1.0".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that a v1.0 `WorkflowEvaluated` payload (no `phase` field)
    /// deserialises without error and produces `phase: None`.
    #[test]
    fn workflow_evaluated_v1_0_fixture_deserialises_to_phase_none() {
        let json = r#"{"WorkflowEvaluated": {"suggested_actions": ["next"]}}"#;
        let event: JourneyEvent = serde_json::from_str(json).unwrap();
        match event {
            JourneyEvent::WorkflowEvaluated {
                suggested_actions,
                phase,
            } => {
                assert_eq!(suggested_actions, vec!["next".to_string()]);
                assert!(phase.is_none(), "phase must be None for v1.0 payload");
            }
            other => panic!("expected WorkflowEvaluated, got {other:?}"),
        }
    }

    /// Verify that a v1.1 `WorkflowEvaluated` payload (with `phase`) round-trips.
    #[test]
    fn workflow_evaluated_v1_1_round_trips_phase() {
        let event = JourneyEvent::WorkflowEvaluated {
            suggested_actions: vec!["confirm".to_string()],
            phase: Some("collecting_passengers".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: JourneyEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, decoded);
        assert_eq!(event.event_version(), "1.1");
    }
}
