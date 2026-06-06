use std::collections::BTreeMap;

use cqrs_es::DomainEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::AttributePath;

/// Per-role secret data carried by an [`JourneyEvent::AttributesSet`] event.
///
/// Each entry corresponds to one role path (e.g. `"persons/passenger_0"`) whose
/// secret attributes were touched by the originating `SetAttributes` command.
/// The `changes` map is encrypted under the subject's DEK; `role_path` is used
/// as the crypto label (AAD) so the partition identity is meaningful on the
/// read path.
///
/// # Backward compatibility
/// Events written before this rename stored `person_ref: "passenger_0"` (the
/// short slot name without the `"persons/"` prefix). The custom [`Deserialize`]
/// impl accepts both formats: if `role_path` is absent it reads `person_ref`
/// and synthesises `"persons/{person_ref}"` as the role path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecretPartitionData {
    /// Full schema path at which the subject is bound, e.g. `"persons/passenger_0"`.
    /// Used as the crypto label (AAD).
    pub role_path: AttributePath,
    /// The subject's identity key — used to look up the DEK.
    pub subject_id: Uuid,
    /// Path → value changes. Encrypted under `subject_id`'s DEK.
    pub changes: BTreeMap<AttributePath, Value>,
}

impl<'de> Deserialize<'de> for SecretPartitionData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Intermediate struct that accepts both the current and legacy field names.
        #[derive(Deserialize)]
        struct Raw {
            /// Current field name.
            role_path: Option<AttributePath>,
            /// Legacy field name — present in events written before the rename.
            person_ref: Option<String>,
            subject_id: Uuid,
            #[serde(default)]
            changes: BTreeMap<AttributePath, Value>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let role_path = if let Some(rp) = raw.role_path {
            rp
        } else if let Some(pr) = raw.person_ref {
            // Old events stored only the short slot name; synthesise the full path.
            format!("persons/{pr}")
                .parse::<AttributePath>()
                .map_err(serde::de::Error::custom)?
        } else {
            return Err(serde::de::Error::missing_field("role_path"));
        };

        Ok(Self {
            role_path,
            subject_id: raw.subject_id,
            changes: raw.changes,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum JourneyEvent {
    Started {
        id: Uuid,
    },
    #[deprecated(since = "0.3.0", note = "use SetAttributes (path-keyed attributes)")]
    Modified {
        step: String,
        data: Value,
    },
    PersonCaptured {
        person_ref: String,
        subject_id: Uuid,
        name: String,
        email: String,
        phone: Option<String>,
    },
    #[deprecated(since = "0.3.0", note = "use SetAttributes (path-keyed attributes)")]
    PersonDetailsUpdated {
        person_ref: String,
        subject_id: Uuid,
        data: Value,
    },
    WorkflowEvaluated {
        suggested_actions: Vec<String>,
        /// Phase label from the decision engine; `None` for events written
        /// before schema version 1.1 (legacy `Capture` arm always writes `None`).
        #[serde(default)]
        phase: Option<String>,
    },
    #[deprecated(
        since = "0.3.0",
        note = "use WorkflowEvaluated.phase instead of tracking step progression"
    )]
    StepProgressed {
        from_step: Option<String>,
        to_step: String,
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
        role_path: AttributePath,
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
    #[allow(deprecated)]
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
