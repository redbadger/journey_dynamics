use crate::domain::journey::{Journey, JourneyState};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct WorkflowDecision {
    pub available_actions: Vec<String>,
}

#[async_trait]
pub trait DecisionEngine: Send + Sync {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
        data: &(String, Value),
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>>;
}

/// Simple rule-based decision engine implementation
pub struct SimpleDecisionEngine;

#[async_trait]
impl DecisionEngine for SimpleDecisionEngine {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
        new_data: &(String, Value),
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut combined_data = journey.data_capture().to_vec();
        let state = journey.state();

        combined_data.push(new_data.to_owned());

        let available_actions = match state {
            JourneyState::InProgress => {
                // Check if any form has "first_name" key
                let has_first_name = combined_data.iter().any(|(_, data)| {
                    data.as_object()
                        .and_then(|obj| obj.get("first_name"))
                        .is_some()
                });

                if has_first_name {
                    vec!["form_3".to_string()]
                } else if combined_data
                    .iter()
                    .any(|(section, _)| section.contains("section_2"))
                {
                    vec!["form_4".to_string()]
                } else {
                    vec![]
                }
            }
            JourneyState::Complete => vec![],
        };

        Ok(WorkflowDecision { available_actions })
    }
}
