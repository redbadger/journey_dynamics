use crate::domain::commands::JourneyCommand;
use crate::domain::events::JourneyEvent;
use async_trait::async_trait;
use cqrs_es::Aggregate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Journey {
    id: Uuid,
    state: JourneyState,
    data_capture: Vec<(String, Value)>,
    latest_workflow_decision: Option<WorkflowDecisionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDecisionState {
    pub available_actions: Vec<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub enum JourneyState {
    #[default]
    InProgress,
    Complete,
}

#[async_trait]
impl Aggregate for Journey {
    type Command = JourneyCommand;
    type Event = JourneyEvent;
    type Error = JourneyError;
    type Services = JourneyServices;

    // This identifier should be unique to the system.
    fn aggregate_type() -> String {
        "Journey".to_string()
    }

    // The aggregate logic goes here. Note that this will be the _bulk_ of a CQRS system
    // so expect to use helper functions elsewhere to keep the code clean.
    async fn handle(
        &self,
        command: Self::Command,
        _services: &Self::Services,
    ) -> Result<Vec<Self::Event>, Self::Error> {
        match command {
            JourneyCommand::Start { id } => {
                if self.id == id {
                    Err(JourneyError::AlreadyStarted)
                } else {
                    Ok(vec![JourneyEvent::Started { id }])
                }
            }
            JourneyCommand::Capture { data } => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    let decision = _services
                        .decision_engine()
                        .evaluate_next_steps(self, &data)
                        .await
                        .map_err(|e| JourneyError::DecisionEngineError(e.to_string()))?;

                    Ok(vec![
                        JourneyEvent::Modified {
                            form_data: Some(data),
                        },
                        JourneyEvent::WorkflowEvaluated {
                            available_actions: decision.available_actions,
                        },
                    ])
                }
            }
            JourneyCommand::Complete => {
                if self.id == Uuid::default() {
                    Err(JourneyError::NotFound)
                } else if JourneyState::Complete == self.state {
                    Err(JourneyError::AlreadyCompleted)
                } else {
                    Ok(vec![JourneyEvent::Completed])
                }
            }
        }
    }

    fn apply(&mut self, event: Self::Event) {
        match event {
            JourneyEvent::Started { id } => {
                self.id = id;
                self.state = JourneyState::InProgress;
            }
            JourneyEvent::Modified { form_data } => {
                if let Some(data) = form_data {
                    self.data_capture.push(data);
                }
            }
            JourneyEvent::WorkflowEvaluated { available_actions } => {
                self.latest_workflow_decision = Some(WorkflowDecisionState { available_actions });
            }
            JourneyEvent::Completed => {
                self.state = JourneyState::Complete;
            }
        }
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum JourneyError {
    #[error("Journey not found")]
    NotFound,
    #[error("Journey already opened")]
    AlreadyStarted,
    #[error("Journey already closed")]
    AlreadyCompleted,
    #[error("Decision engine error: {0}")]
    DecisionEngineError(String),
}

pub struct JourneyServices {
    decision_engine: std::sync::Arc<dyn crate::services::decision_engine::DecisionEngine>,
}

impl JourneyServices {
    pub fn new(
        decision_engine: std::sync::Arc<dyn crate::services::decision_engine::DecisionEngine>,
    ) -> Self {
        Self { decision_engine }
    }

    #[must_use]
    pub fn decision_engine(
        &self,
    ) -> &std::sync::Arc<dyn crate::services::decision_engine::DecisionEngine> {
        &self.decision_engine
    }
}

impl Journey {
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn state(&self) -> &JourneyState {
        &self.state
    }

    #[must_use]
    pub fn data_capture(&self) -> &[(String, Value)] {
        &self.data_capture
    }

    #[must_use]
    pub fn latest_workflow_decision(&self) -> Option<&WorkflowDecisionState> {
        self.latest_workflow_decision.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use cqrs_es::{AggregateError, CqrsFramework, EventStore, mem_store::MemStore};
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    use super::*;
    use crate::SimpleLoggingQuery;
    use crate::services::decision_engine::{GoRulesDecisionEngine, SimpleDecisionEngine};

    #[tokio::test]
    async fn happy_path() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store.clone(), vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // modify the Journey
        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("form_data".to_string(), json!({})),
            },
        )
        .await
        .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // this here to show how to list events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("{events:#?}");
    }

    #[tokio::test]
    async fn happy_path_form() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store.clone(), vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // modify the Journey
        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("form_data".to_string(), json!({})),
            },
        )
        .await
        .unwrap();

        let form_value: serde_json::Value = json!({
                "alpha": 42,
                    "beta": "hello"
        });

        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("alpha".to_string(), form_value),
            },
        )
        .await
        .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // this here to show how to list events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("{events:#?}");
    }

    #[tokio::test]
    async fn open_already_opened() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // try to start the Journey again
        let result = cqrs
            .execute(&id.to_string(), JourneyCommand::Start { id })
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::AlreadyStarted))
        ));
    }

    #[tokio::test]
    async fn complete_not_started() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // try to complete the Journey
        let result = cqrs
            .execute(&id.to_string(), JourneyCommand::Complete)
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::NotFound))
        ));
    }

    #[tokio::test]
    async fn complete_already_completed() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // try to complete the Journey again
        let result = cqrs
            .execute(&id.to_string(), JourneyCommand::Complete)
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::AlreadyCompleted))
        ));
    }

    #[tokio::test]
    async fn modify_not_started() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // try to modify the Journey before starting
        let result = cqrs
            .execute(
                &id.to_string(),
                JourneyCommand::Capture {
                    data: ("form_data".to_string(), json!({})),
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::NotFound))
        ));
    }

    #[tokio::test]
    async fn modify_already_completed() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);
        let cqrs = CqrsFramework::new(event_store, vec![Box::new(query)], services);

        let id = Uuid::new_v4();

        // start a Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // complete the Journey
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // try to modify the Journey after completion
        let result = cqrs
            .execute(
                &id.to_string(),
                JourneyCommand::Capture {
                    data: ("form_data".to_string(), json!({})),
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(AggregateError::UserError(JourneyError::AlreadyCompleted))
        ));
    }

    #[tokio::test]
    async fn automatic_workflow_evaluation_after_every_event() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);

        // Create CQRS framework first
        let cqrs = Arc::new(CqrsFramework::new(
            event_store.clone(),
            vec![Box::new(query)],
            services,
        ));
        let id = Uuid::new_v4();

        // Start a Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // Submit a form - should trigger workflow evaluation
        let form_value = json!({
            "step": "personal_info",
            "email": "user@example.com",
            "name": "Alice"
        });

        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("step".to_string(), form_value),
            },
        )
        .await
        .unwrap();

        // Complete the Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // Verify events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("\n=== All Events ===");
        for event in &events {
            println!("{}-{}: {:?}", id, event.sequence, event.payload);
        }

        // Expected event pattern:
        // 1. Started
        // 2. Modified (form submission)
        // 3. WorkflowEvaluated (triggered by Modified)
        // 4. Completed

        assert!(events.len() == 4, "Should have 4 events");

        // Verify WorkflowEvaluated events are interleaved
        assert!(matches!(events[0].payload, JourneyEvent::Started { .. }));
        assert!(matches!(events[1].payload, JourneyEvent::Modified { .. }));
        assert!(matches!(
            events[2].payload,
            JourneyEvent::WorkflowEvaluated { .. }
        ));
        assert_eq!(
            events[2].payload,
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec![]
            }
        );
        assert!(matches!(events[3].payload, JourneyEvent::Completed));
    }

    #[tokio::test]
    async fn automatic_workflow_evaluation_for_specific_data() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(SimpleDecisionEngine);
        let services = JourneyServices::new(decision_engine);

        // Create CQRS framework first
        let cqrs = Arc::new(CqrsFramework::new(
            event_store.clone(),
            vec![Box::new(query)],
            services,
        ));
        let id = Uuid::new_v4();

        // Start a Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // Submit a form - should trigger workflow evaluation
        let form_value = json!({
            "step": "personal_info",
            "email": "user@example.com",
            "first_name": "Alice"
        });

        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("step_1".to_string(), form_value),
            },
        )
        .await
        .unwrap();

        // Complete the Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // Verify events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("\n=== All Events ===");

        assert!(events.len() == 4, "Should have 4 events");
        assert_eq!(
            events[2].payload,
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["form_3".to_string()]
            }
        );
    }

    #[tokio::test]
    async fn use_go_rules_workflow_evaluation_gia() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(GoRulesDecisionEngine::new());
        let services = JourneyServices::new(decision_engine);

        // Create CQRS framework first
        let cqrs = Arc::new(CqrsFramework::new(
            event_store.clone(),
            vec![Box::new(query)],
            services,
        ));
        let id = Uuid::new_v4();

        // Start a Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // Submit a form - should trigger workflow evaluation
        let form_value = json!({
            "step": "personal_info",
            "country": "UK",
            "name": "Alice",
        });

        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("personal_info".to_string(), form_value),
            },
        )
        .await
        .unwrap();

        // Complete the Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // Verify events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("\n=== All Events ===");

        assert!(events.len() == 4, "Should have 4 events");
        assert_eq!(
            events[2].payload,
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["form_3".to_string()]
            }
        );
    }
    #[tokio::test]
    async fn use_go_rules_workflow_evaluation_isa() {
        let event_store = MemStore::<Journey>::default();
        let query = SimpleLoggingQuery {};
        let decision_engine = Arc::new(GoRulesDecisionEngine::new());
        let services = JourneyServices::new(decision_engine);

        // Create CQRS framework first
        let cqrs = Arc::new(CqrsFramework::new(
            event_store.clone(),
            vec![Box::new(query)],
            services,
        ));
        let id = Uuid::new_v4();

        // Start a Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Start { id })
            .await
            .unwrap();

        // Submit a form - should trigger workflow evaluation
        let form_value = json!({
            "step": "personal_info",
            "country": "UK",
            "name": "Alice",
        });

        cqrs.execute(
            &id.to_string(),
            JourneyCommand::Capture {
                data: ("personal_info".to_string(), form_value),
            },
        )
        .await
        .unwrap();

        // Complete the Journey - should trigger workflow evaluation
        cqrs.execute(&id.to_string(), JourneyCommand::Complete)
            .await
            .unwrap();

        // Verify events in the store
        let events = event_store.load_events(&id.to_string()).await.unwrap();
        println!("\n=== All Events ===");

        assert!(events.len() == 4, "Should have 4 events");
        assert_eq!(
            events[2].payload,
            JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["form_3".to_string()]
            }
        );
    }
}
