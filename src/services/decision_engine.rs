use crate::domain::journey::{Journey, JourneyState};
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct WorkflowDecision {
    pub available_actions: Vec<String>,
    pub recommended_action: Option<String>,
    pub constraints: Vec<String>,
}

#[async_trait]
pub trait DecisionEngine: Send + Sync {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>>;
}

/// Simple rule-based decision engine implementation
pub struct SimpleDecisionEngine;

#[async_trait]
impl DecisionEngine for SimpleDecisionEngine {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let all_forms = journey.data_capture();
        let state = journey.state();

        let (available_actions, recommended_action) = match state {
            JourneyState::InProgress => {
                // Check what data has been captured
                let form_count = all_forms.len();

                if form_count == 0 {
                    (
                        vec!["submit_form".to_string(), "complete".to_string()],
                        Some("submit_form".to_string()),
                    )
                } else {
                    // Check if any form indicates readiness to complete
                    let ready_to_complete = all_forms.iter().any(|(_, data)| {
                        data.get("ready_to_complete")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    });

                    if ready_to_complete {
                        (
                            vec!["complete".to_string(), "submit_more_forms".to_string()],
                            Some("complete".to_string()),
                        )
                    } else {
                        (
                            vec![
                                "submit_form".to_string(),
                                "modify".to_string(),
                                "complete".to_string(),
                            ],
                            Some("submit_form".to_string()),
                        )
                    }
                }
            }
            JourneyState::Complete => (vec![], None),
        };

        Ok(WorkflowDecision {
            available_actions,
            recommended_action,
            constraints: vec![],
        })
    }
}
