use crate::domain::journey::{Journey, JourneyState};
use async_trait::async_trait;

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine as ZenEngine, DecisionGraphResponse, EvaluationOptions};

#[derive(Debug, Clone)]
pub struct WorkflowDecision {
    pub suggested_actions: Vec<String>,
}

#[async_trait]
pub trait DecisionEngine: Send + Sync {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
        current_step: &str,
        new_data: &Value,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>>;
}

/// Simple rule-based decision engine implementation
pub struct SimpleDecisionEngine;

#[async_trait]
impl DecisionEngine for SimpleDecisionEngine {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
        current_step: &str,
        new_data: &Value,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut accumulated_data = journey.accumulated_data().clone();
        let keyed_data = serde_json::json!({ current_step: new_data });
        json_patch::merge(&mut accumulated_data, &keyed_data);
        let state = journey.state();

        let suggested_actions = match state {
            JourneyState::InProgress => {
                // Check if any step has "first_name" key
                let has_first_name = accumulated_data.as_object().is_some_and(|obj| {
                    obj.values().any(|value| {
                        value
                            .as_object()
                            .and_then(|obj| obj.get("first_name"))
                            .is_some()
                    })
                });

                if has_first_name {
                    vec!["form_3".to_string()]
                } else if current_step.contains("section_2") {
                    vec!["form_4".to_string()]
                } else {
                    vec![]
                }
            }
            JourneyState::Complete => vec![],
        };

        Ok(WorkflowDecision { suggested_actions })
    }
}

/// Simple rule-based decision engine implementation
pub struct GoRulesDecisionEngine {
    pub decision_content: DecisionContent,
}

impl GoRulesDecisionEngine {
    /// # Panics
    #[must_use]
    pub fn new(json: &str) -> Self {
        let decision_content: DecisionContent = serde_json::from_str(json).unwrap();
        GoRulesDecisionEngine { decision_content }
    }
}

#[async_trait]
impl DecisionEngine for GoRulesDecisionEngine {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
        current_step: &str,
        new_data: &Value,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut captured_data = journey.accumulated_data().clone();
        json_patch::merge(&mut captured_data, new_data);

        // Build the context for decision engine evaluation
        let mut context = Map::new();

        // Include currentStep so the decision engine can route correctly
        context.insert(
            "currentStep".to_string(),
            Value::String(current_step.to_string()),
        );

        context.insert("capturedData".to_string(), captured_data);

        let context = serde_json::to_value(&context).unwrap();

        eprintln!("JDM Context: {context:#?}");

        // Create a new Decision for each evaluation
        // Use spawn_blocking because ZenEngine contains non-Send types (Rc<str>)
        let decision_content = self.decision_content.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let engine = ZenEngine::default();
            let decision = engine.create_decision(decision_content.into());
            let response = Handle::current()
                .block_on(decision.evaluate_with_opts(
                    context.into(),
                    EvaluationOptions {
                        trace: true,
                        ..Default::default()
                    },
                ))
                .map_err(|e| e.to_string())?;

            // println!("JDM Response: {:#?}", response);
            serde_json::to_value(response).map_err(|e| e.to_string())
        })
        .await
        .unwrap()
        .map_err(|e| {
            Box::new(std::io::Error::other(e)) as Box<dyn std::error::Error + Send + Sync>
        })?;

        let DecisionGraphResponse { result, .. } = serde_json::from_value(result)?;
        let unwrapped_map = result.as_object().unwrap();
        let take = unwrapped_map.take();

        // Get suggested actions directly from the decision result
        let suggested_actions: Vec<String> = take
            .get("suggestedActions")
            .ok_or("No suggested actions found")?
            .as_array()
            .ok_or("Suggested actions is not an array")?
            .take()
            .into_iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect();

        Ok(WorkflowDecision { suggested_actions })
    }
}
