use cqrs_es::persist::GenericQuery;
use cqrs_es::{EventEnvelope, View};
use postgres_es::PostgresViewRepository;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

    /// All data captured during the journey as key-value pairs
    pub data_capture: Vec<DataCaptureEntry>,

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

/// A data capture entry with a key and JSON value
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DataCaptureEntry {
    pub key: String,
    pub value: Value,
}

/// The workflow decision state in the view
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkflowDecisionView {
    pub available_actions: Vec<String>,
    pub primary_next_step: Option<String>,
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
                self.data_capture = Vec::new();
                self.current_step = None;
                self.latest_workflow_decision = None;
            }

            JourneyEvent::Modified { form_data } => {
                // Add captured data to the journey
                if let Some((key, value)) = form_data {
                    self.data_capture.push(DataCaptureEntry {
                        key: key.clone(),
                        value: value.clone(),
                    });
                }
            }

            JourneyEvent::PersonCaptured { .. } => {
                // Person data is projected to structured database tables
                // No need to update the view here
            }

            JourneyEvent::WorkflowEvaluated {
                available_actions,
                primary_next_step,
            } => {
                // Update the latest workflow decision
                self.latest_workflow_decision = Some(WorkflowDecisionView {
                    available_actions: available_actions.clone(),
                    primary_next_step: primary_next_step.clone(),
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
        assert!(view.data_capture.is_empty());
        assert!(view.current_step.is_none());
        assert!(view.latest_workflow_decision.is_none());
    }

    #[test]
    fn test_journey_view_modified_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            data_capture: Vec::new(),
            current_step: None,
            latest_workflow_decision: None,
        };

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 2,
            payload: JourneyEvent::Modified {
                form_data: Some(("user_name".to_string(), json!("John Doe"))),
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(view.data_capture.len(), 1);
        assert_eq!(view.data_capture[0].key, "user_name");
        assert_eq!(view.data_capture[0].value, json!("John Doe"));
    }

    #[test]
    fn test_journey_view_workflow_evaluated_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            data_capture: Vec::new(),
            current_step: None,
            latest_workflow_decision: None,
        };

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["next".to_string(), "back".to_string()],
                primary_next_step: Some("step2".to_string()),
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert!(view.latest_workflow_decision.is_some());
        let decision = view.latest_workflow_decision.as_ref().unwrap();
        assert_eq!(decision.available_actions.len(), 2);
        assert_eq!(decision.primary_next_step, Some("step2".to_string()));
    }

    #[test]
    fn test_journey_view_step_progressed_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            data_capture: Vec::new(),
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
            data_capture: Vec::new(),
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
                form_data: Some(("email".to_string(), json!("test@example.com"))),
            },
            metadata: HashMap::default(),
        });

        // Workflow evaluated
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::WorkflowEvaluated {
                available_actions: vec!["continue".to_string()],
                primary_next_step: Some("confirmation".to_string()),
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
        assert_eq!(view.data_capture.len(), 1);
        assert_eq!(view.current_step, Some("confirmation".to_string()));
        assert!(view.latest_workflow_decision.is_some());
    }
}
