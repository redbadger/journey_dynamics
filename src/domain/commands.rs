use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    Start { id: Uuid },
    Modify,
    FormSubmitted { data: Value },
    Complete,
}
