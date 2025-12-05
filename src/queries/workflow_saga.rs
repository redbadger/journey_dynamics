use async_trait::async_trait;
use cqrs_es::{CqrsFramework, EventEnvelope, EventStore, Query};
use std::sync::Arc;

use crate::domain::commands::JourneyCommand;
use crate::domain::events::JourneyEvent;
use crate::domain::journey::Journey;

/// Saga that automatically triggers workflow evaluation after every domain event
/// except WorkflowEvaluated events (to prevent infinite loops)
pub struct WorkflowEvaluationSaga<ES>
where
    ES: EventStore<Journey>,
{
    cqrs: Arc<CqrsFramework<Journey, ES>>,
}

impl<ES> WorkflowEvaluationSaga<ES>
where
    ES: EventStore<Journey>,
{
    pub fn new(cqrs: Arc<CqrsFramework<Journey, ES>>) -> Self {
        Self { cqrs }
    }
}

#[async_trait]
impl<ES> Query<Journey> for WorkflowEvaluationSaga<ES>
where
    ES: EventStore<Journey>,
    ES::AC: Send,
{
    async fn dispatch(&self, aggregate_id: &str, events: &[EventEnvelope<Journey>]) {
        // Check if any of the new events warrant a workflow evaluation
        let should_evaluate = events
            .iter()
            .any(|envelope| !matches!(envelope.payload, JourneyEvent::WorkflowEvaluated { .. }));

        if should_evaluate {
            println!("Triggering workflow evaluation for journey {aggregate_id}");

            // Issue UpdateWorkflowRequirements command
            if let Err(e) = self
                .cqrs
                .execute(aggregate_id, JourneyCommand::UpdateWorkflowRequirements)
                .await
            {
                eprintln!("Failed to evaluate workflow for {aggregate_id}: {e}");
            }
        }
    }
}
