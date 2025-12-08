use cqrs_es::DomainEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JourneyEvent {
    Started {
        id: Uuid,
    },
    Modified {
        form_data: Option<(String, Value)>,
    },
    WorkflowEvaluated {
        available_actions: Vec<String>,
        primary_next_step: Option<String>,
    },
    StepProgressed {
        from_step: Option<String>,
        to_step: String,
    },
    Completed,
}

impl DomainEvent for JourneyEvent {
    fn event_type(&self) -> String {
        let event_type: &str = match self {
            JourneyEvent::Started { .. } => "JourneyOpened",
            JourneyEvent::Modified { .. } => "JourneyModified",
            JourneyEvent::WorkflowEvaluated { .. } => "WorkflowEvaluated",
            JourneyEvent::StepProgressed { .. } => "StepProgressed",
            JourneyEvent::Completed => "JourneyClosed",
        };
        event_type.to_string()
    }

    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}
