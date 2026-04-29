use std::{
    future::Future,
    sync::{Arc, OnceLock},
    thread::available_parallelism,
};

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::task::JoinHandle;
use tokio_util::task::LocalPoolHandle;
use zen_engine::{
    DecisionEngine as ZenEngine, DecisionGraphResponse, EvaluationOptions, model::DecisionContent,
};

use crate::domain::journey::{Journey, JourneyState};

// ---------------------------------------------------------------------------
// Thread-pinned worker pool
//
// The Future returned by `decision.evaluate*` is !Send by design — the
// expression engine is intentionally single-threaded for maximum performance.
// The GoRules docs recommend using LocalPoolHandle::spawn_pinned() from
// tokio-util for multi-threaded workloads.  This replaces the previous
// spawn_blocking + Handle::block_on antipattern.
// ---------------------------------------------------------------------------

fn worker_pool() -> LocalPoolHandle {
    static LOCAL_POOL: OnceLock<LocalPoolHandle> = OnceLock::new();
    LOCAL_POOL
        .get_or_init(|| LocalPoolHandle::new(available_parallelism().map_or(1, Into::into)))
        .clone()
}

/// Spawn a `!Send` future on a thread-pinned worker.
///
/// The closure `F` must be `Send` (so it can be delivered to the worker
/// thread), but the `Future` it produces does not need to be.
fn spawn_pinned<F, Fut>(create_task: F) -> JoinHandle<Fut::Output>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future + 'static,
    Fut::Output: Send + 'static,
{
    worker_pool().spawn_pinned(create_task)
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// SimpleDecisionEngine — in-process rule-based fallback used in tests
// ---------------------------------------------------------------------------

pub struct SimpleDecisionEngine;

#[async_trait]
impl DecisionEngine for SimpleDecisionEngine {
    async fn evaluate_next_steps(
        &self,
        journey: &Journey,
        current_step: &str,
        new_data: &Value,
    ) -> Result<WorkflowDecision, Box<dyn std::error::Error + Send + Sync>> {
        let mut accumulated_data = journey.shared_data().clone();
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

// ---------------------------------------------------------------------------
// GoRulesDecisionEngine — production JDM engine
//
// Best-practice changes versus the original implementation:
//
//  1. `ZenEngine` is initialised **once** at construction and shared via `Arc`.
//     All fields of `DecisionEngine` are `Arc<dyn Trait + Send + Sync>`, so the
//     engine itself is `Send + Sync` and can be cheaply cloned into each task.
//
//  2. `DecisionContent` is wrapped in `Arc` so the deserialized decision graph
//     is never copied — only a pointer is cloned per evaluation.
//
//  3. `spawn_pinned` (via `LocalPoolHandle`) is used instead of the previous
//     `spawn_blocking` + `Handle::block_on` antipattern.
//
//  4. `decision.compile()` is called before each evaluation so that the graph
//     is parsed and optimised ahead of time, reducing per-evaluation overhead.
// ---------------------------------------------------------------------------

pub struct GoRulesDecisionEngine {
    engine: Arc<ZenEngine>,
    decision_content: Arc<DecisionContent>,
}

impl GoRulesDecisionEngine {
    /// # Panics
    ///
    /// Panics if `json` cannot be deserialized as a [`DecisionContent`].
    #[must_use]
    pub fn new(json: &str) -> Self {
        let mut decision_content: DecisionContent = serde_json::from_str(json).unwrap();
        // Compile once at startup: pre-computes all expression bytecodes into
        // an OpcodeCache stored inside DecisionContent.  Every Decision created
        // from this Arc will carry the compiled cache, so no per-request
        // compilation is needed.
        decision_content.compile();
        Self {
            engine: Arc::new(ZenEngine::default()),
            decision_content: Arc::new(decision_content),
        }
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
        let mut captured_data = journey.shared_data().clone();
        json_patch::merge(&mut captured_data, new_data);

        // Build the evaluation context.
        let mut context = Map::new();
        // Include currentStep so the decision engine can route correctly.
        context.insert(
            "currentStep".to_string(),
            Value::String(current_step.to_string()),
        );
        context.insert("capturedData".to_string(), captured_data);
        let context = serde_json::to_value(&context).unwrap();

        eprintln!("JDM Context: {context:#?}");

        // Both clones are cheap Arc pointer copies.  Arc<ZenEngine> and
        // Arc<DecisionContent> are Send + Sync so they can be moved into the
        // spawn_pinned closure, whose own signature requires Send + 'static.
        let engine = Arc::clone(&self.engine);
        let jdm_content = Arc::clone(&self.decision_content);

        // We serialise the response to serde_json::Value inside the closure so
        // that the JoinHandle output type is Send, even though
        // DecisionGraphResponse (which holds zen_engine::Variable) is not.
        let result: Value = spawn_pinned(move || async move {
            // The DecisionContent was compiled once in new(), so the OpcodeCache
            // is already populated inside the Arc.  No compile() call needed here.
            let decision = engine.create_decision(jdm_content);

            let response = decision
                .evaluate_with_opts(
                    context.into(),
                    EvaluationOptions {
                        trace: true,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| e.to_string())?;

            serde_json::to_value(response).map_err(|e| e.to_string())
        })
        .await
        // JoinError (task panicked or was cancelled)
        .map_err(|e| {
            Box::new(std::io::Error::other(e.to_string()))
                as Box<dyn std::error::Error + Send + Sync>
        })?
        // String error from inside the closure
        .map_err(|e| {
            Box::new(std::io::Error::other(e)) as Box<dyn std::error::Error + Send + Sync>
        })?;

        let DecisionGraphResponse { result, .. } = serde_json::from_value(result)?;
        let unwrapped_map = result.as_object().unwrap();
        let take = unwrapped_map.take();

        // Get suggested actions directly from the decision result.
        // If the key is absent (e.g. no JDM route matched the current step),
        // fall back to an empty list rather than propagating an error.
        let suggested_actions: Vec<String> = take
            .get("suggestedActions")
            .and_then(zen_engine::Variable::as_array)
            .map(|arr| {
                arr.take()
                    .into_iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        Ok(WorkflowDecision { suggested_actions })
    }
}
