use cqrs_es::persist::GenericQuery;
use cqrs_es::{EventEnvelope, View};
use postgres_es::PostgresViewRepository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::events::JourneyEvent;
use crate::domain::journey::Journey;

// Our Journey query using PostgresViewRepository which will serialize and persist
// our view after it is updated. It provides a `load` method to deserialize the view on request.
pub type JourneyQuery =
    GenericQuery<PostgresViewRepository<JourneyView, Journey>, JourneyView, Journey>;

/// The view for a Journey query, designed to reflect the complete state
/// of a journey as stored in the database. This view is updated as events
/// are committed to the event store.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct JourneyView {
    /// Unique identifier for the journey
    pub id: Uuid,

    /// Current state of the journey (`InProgress` or `Complete`)
    pub state: JourneyState,

    /// All data accumulated during the journey
    pub accumulated_data: Value,

    /// The current step in the journey workflow
    pub current_step: Option<String>,

    /// The latest workflow decision state including available actions
    pub latest_workflow_decision: Option<WorkflowDecisionView>,
}

/// Represents the state of a journey in the view
#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum JourneyState {
    #[default]
    InProgress,
    Complete,
}

/// The workflow decision state in the view
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkflowDecisionView {
    pub suggested_actions: Vec<String>,
}

// This updates the view with events as they are committed.
// The logic should be minimal here - the events should carry all necessary information.
impl View<Journey> for JourneyView {
    fn update(&mut self, event: &EventEnvelope<Journey>) {
        match &event.payload {
            JourneyEvent::Started { id } => {
                // Initialize the journey view with the ID
                self.id = *id;
                self.state = JourneyState::InProgress;
                self.accumulated_data = json!({});
                self.current_step = None;
                self.latest_workflow_decision = None;
            }

            JourneyEvent::Modified { step: _, data } => {
                // Merge new data into accumulated data
                json_patch::merge(&mut self.accumulated_data, data);
            }

            JourneyEvent::PersonCaptured { .. } => {
                // Person data is projected to structured database tables
                // No need to update the view here
            }

            JourneyEvent::WorkflowEvaluated { suggested_actions } => {
                // Update the latest workflow decision
                self.latest_workflow_decision = Some(WorkflowDecisionView {
                    suggested_actions: suggested_actions.clone(),
                });
            }

            JourneyEvent::StepProgressed {
                from_step: _,
                to_step,
            } => {
                // Update the current step
                self.current_step = Some(to_step.clone());
            }

            JourneyEvent::Completed => {
                // Mark the journey as complete
                self.state = JourneyState::Complete;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use serde_json::json;

    #[test]
    fn test_journey_view_started_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView::default();

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 1,
            payload: JourneyEvent::Started { id },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(view.id, id);
        assert_eq!(view.state, JourneyState::InProgress);
        assert_eq!(view.accumulated_data, json!({}));
        assert!(view.current_step.is_none());
        assert!(view.latest_workflow_decision.is_none());
    }

    #[test]
    fn test_journey_view_modified_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            accumulated_data: json!({}),
            current_step: None,
            latest_workflow_decision: None,
        };

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 2,
            payload: JourneyEvent::Modified {
                step: "user_name".to_string(),
                data: json!({"user_name": "John Doe"}),
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(
            view.accumulated_data.get("user_name"),
            Some(&json!("John Doe"))
        );
    }

    #[test]
    fn test_journey_view_workflow_evaluated_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            accumulated_data: json!({}),
            current_step: None,
            latest_workflow_decision: None,
        };

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec![
                    "step2".to_string(),
                    "next".to_string(),
                    "back".to_string(),
                ],
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert!(view.latest_workflow_decision.is_some());
        let decision = view.latest_workflow_decision.as_ref().unwrap();
        assert_eq!(decision.suggested_actions.len(), 3);
        assert_eq!(decision.suggested_actions[0], "step2");
        assert_eq!(decision.suggested_actions[1], "next");
        assert_eq!(decision.suggested_actions[2], "back");
    }

    #[test]
    fn test_journey_view_step_progressed_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            accumulated_data: json!({}),
            current_step: Some("step1".to_string()),
            latest_workflow_decision: None,
        };

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 4,
            payload: JourneyEvent::StepProgressed {
                from_step: Some("step1".to_string()),
                to_step: "step2".to_string(),
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(view.current_step, Some("step2".to_string()));
    }

    #[test]
    fn test_journey_view_completed_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            accumulated_data: json!({}),
            current_step: Some("final_step".to_string()),
            latest_workflow_decision: None,
        };

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 5,
            payload: JourneyEvent::Completed,
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(view.state, JourneyState::Complete);
    }

    #[test]
    fn test_journey_view_full_lifecycle() {
        let id = Uuid::new_v4();
        let mut view = JourneyView::default();

        // Started
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 1,
            payload: JourneyEvent::Started { id },
            metadata: HashMap::default(),
        });

        // Modified with data
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 2,
            payload: JourneyEvent::Modified {
                step: "email".to_string(),
                data: json!({"email": "test@example.com"}),
            },
            metadata: HashMap::default(),
        });

        // Workflow evaluated
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["confirmation".to_string(), "continue".to_string()],
            },
            metadata: HashMap::default(),
        });

        // Step progressed
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 4,
            payload: JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "confirmation".to_string(),
            },
            metadata: HashMap::default(),
        });

        // Completed
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 5,
            payload: JourneyEvent::Completed,
            metadata: HashMap::default(),
        });

        assert_eq!(view.id, id);
        assert_eq!(view.state, JourneyState::Complete);
        assert_eq!(
            view.accumulated_data.get("email"),
            Some(&json!("test@example.com"))
        );
        assert_eq!(view.current_step, Some("confirmation".to_string()));
        assert!(view.latest_workflow_decision.is_some());
    }
}
