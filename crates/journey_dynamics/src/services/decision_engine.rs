use crate::domain::journey::{Journey, JourneyState};
use async_trait::async_trait;

use serde_json::{Map, Value, json};
use tokio::runtime::Handle;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine as ZenEngine, DecisionGraphResponse, EvaluationOptions};

#[derive(Debug, Clone)]
pub struct WorkflowDecision {
    pub available_actions: Vec<String>,
    pub primary_next_step: Option<String>,
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

        Ok(WorkflowDecision {
            available_actions,
            primary_next_step: None,
        })
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
        new_data: &(String, Value),
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut combined_data = journey.data_capture().to_vec();
        let _state = journey.state();

        combined_data.push(new_data.to_owned());

        let map: Map<String, Value> = combined_data.into_iter().collect();

        // Build the context for decision engine evaluation
        let mut context = Map::new();

        // Include currentStep so the decision engine can route correctly
        if let Some(current_step) = journey.current_step() {
            context.insert(
                "currentStep".to_string(),
                Value::String(current_step.clone()),
            );
        }

        // Merge all step data into capturedData object for decision engine rules
        // Rules expect capturedData.tripType, capturedData.selectedOutboundFlight, etc.
        let mut captured_data = json!({});

        for (key, value) in &map {
            // Skip meta keys, merge step data into capturedData
            if key != "currentStep" && key != "capturedData" {
                if let Value::Object(_) = value {
                    // Use custom merge that prefers arrays over objects for generic conflict resolution
                    Self::merge_with_array_preference(&mut captured_data, value);
                }
            } else if key == "capturedData" {
                // If there's already a capturedData key, merge it too
                if let Value::Object(_) = value {
                    Self::merge_with_array_preference(&mut captured_data, value);
                }
            }
        }

        context.insert("capturedData".to_string(), captured_data);

        let something = serde_json::to_value(&context).unwrap();

        // println!("JDM Context: {:#?}", something);

        // Create a new Decision for each evaluation
        // Use spawn_blocking to move CPU-intensive decision evaluation off the async runtime
        let decision_content = self.decision_content.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let engine = ZenEngine::default();
            let decision = engine.create_decision(decision_content.into());
            let response = Handle::current()
                .block_on(decision.evaluate_with_opts(
                    something.into(),
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

        // Try to get available actions from either "output" or "availableNextSteps" field
        let test = take
            .get("output")
            .or_else(|| take.get("availableNextSteps"))
            .ok_or("No available actions")?;

        let available_actions: Vec<String> = test
            .as_array()
            .ok_or("No available actions")?
            .take()
            .into_iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect();

        // Try to get primary next step
        let primary_next_step = take
            .get("primaryNextStep")
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string);

        Ok(WorkflowDecision {
            available_actions,
            primary_next_step,
        })
    }
}

impl GoRulesDecisionEngine {
    /// Generic merge function that prefers arrays over objects when conflicts occur
    /// This handles cases where the same field name has different data structures
    fn merge_with_array_preference(target: &mut Value, source: &Value) {
        if let (Value::Object(target_obj), Value::Object(source_obj)) = (target, source) {
            for (key, source_value) in source_obj {
                match target_obj.get(key) {
                    Some(target_value) => {
                        // Handle conflicts based on data type preference: Arrays > Objects > Others
                        let merged_value = match (target_value, source_value) {
                            // If source is array and target is not, prefer source (array)
                            (_, Value::Array(_)) => source_value.clone(),
                            // If target is array and source is not, keep target (array)
                            (Value::Array(_), _) => target_value.clone(),
                            // If both are objects, recursively merge them
                            (Value::Object(_), Value::Object(_)) => {
                                let mut merged = target_value.clone();
                                Self::merge_with_array_preference(&mut merged, source_value);
                                merged
                            }
                            // For all other cases, source overwrites target (standard merge behavior)
                            _ => source_value.clone(),
                        };
                        target_obj.insert(key.clone(), merged_value);
                    }
                    None => {
                        // No conflict, just insert the new value
                        target_obj.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
    }
}
