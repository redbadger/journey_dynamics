use cqrs_es::DomainEvent;
use cqrs_es_crypto::PiiCodec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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
        };
        event_type.to_string()
    }

    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}
