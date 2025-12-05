use cqrs_es::DomainEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JourneyEvent {
    Started { id: Uuid },
    Modified { form_data: Option<Value> },
    Completed,
}

impl DomainEvent for JourneyEvent {
    fn event_type(&self) -> String {
        let event_type: &str = match self {
            JourneyEvent::Started { .. } => "JourneyOpened",
            JourneyEvent::Modified { .. } => "JourneyModified",
            JourneyEvent::Completed => "JourneyClosed",
        };
        event_type.to_string()
    }

    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}
