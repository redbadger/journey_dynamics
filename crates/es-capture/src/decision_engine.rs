use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::json_path::assign_all;
use jsonptr::PointerBuf;

#[cfg(feature = "gorules")]
mod gorules;
#[cfg(feature = "gorules")]
pub use gorules::GoRulesDecisionEngine;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WorkflowDecision {
    pub suggested_actions: Vec<String>,
    /// Optional phase label returned by the decision engine.
    /// `None` until the JDM model emits a `phase` output key.
    pub phase: Option<String>,
}

#[async_trait]
pub trait DecisionEngine: Send + Sync {
    /// Evaluate the workflow against the aggregate's accumulated plaintext
    /// `current_state` plus an explicit `new_data` slice keyed under
    /// `current_step`.
    ///
    /// # Errors
    /// Returns an error if the underlying engine fails to evaluate.
    async fn evaluate_next_steps(
        &self,
        current_state: &Value,
        current_step: &str,
        new_data: &Value,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>>;

    /// Evaluate the workflow after a `SetAttributes` command.
    ///
    /// The default implementation rehydrates `pending_changes` into a nested
    /// JSON tree, merges it with the aggregate's current `current_state`, and
    /// delegates to [`Self::evaluate_next_steps`] with an empty step string.
    ///
    /// `current_state` is the aggregate's accumulated plaintext bag (the data
    /// that was historically read from `Journey::shared_data`).
    ///
    /// # Errors
    /// Returns an error if the changes cannot be applied or the engine fails.
    async fn evaluate_attributes(
        &self,
        current_state: &Value,
        pending_changes: &BTreeMap<PointerBuf, Value>,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut merged = current_state.clone();
        assign_all(&mut merged, pending_changes)?;
        self.evaluate_next_steps(current_state, "", &merged).await
    }
}

// ---------------------------------------------------------------------------
// SimpleDecisionEngine — in-process rule-based fallback used in tests
// ---------------------------------------------------------------------------

pub struct SimpleDecisionEngine;

#[async_trait]
impl DecisionEngine for SimpleDecisionEngine {
    async fn evaluate_next_steps(
        &self,
        current_state: &Value,
        current_step: &str,
        new_data: &Value,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut accumulated_data = current_state.clone();
        let keyed_data = serde_json::json!({ current_step: new_data });
        json_patch::merge(&mut accumulated_data, &keyed_data);

        // The engine is only consulted while the aggregate is in progress
        // (a completed aggregate rejects further attribute changes upstream),
        // so there is no terminal-state branch here.
        let has_first_name = accumulated_data.as_object().is_some_and(|obj| {
            obj.values().any(|value| {
                value
                    .as_object()
                    .and_then(|obj| obj.get("first_name"))
                    .is_some()
            })
        });

        let suggested_actions = if has_first_name {
            vec!["form_3".to_string()]
        } else if current_step.contains("section_2") {
            vec!["form_4".to_string()]
        } else {
            vec![]
        };

        Ok(WorkflowDecision {
            suggested_actions,
            phase: None,
        })
    }
}
