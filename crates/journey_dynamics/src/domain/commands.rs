use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    Start {
        id: Uuid,
    },
    Capture {
        data: (String, Value),
    },
    CapturePerson {
        name: String,
        email: String,
        phone: Option<String>,
    },
    Complete,
}
