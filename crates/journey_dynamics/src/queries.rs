use cqrs_es::{EventEnvelope, View, persist::GenericQuery};
use postgres_es::PostgresViewRepository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::{events::JourneyEvent, journey::Journey};

/// Person data for a single slot within a journey.
/// One row per `(journey_id, person_ref)` in the `journey_person` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PersonView {
    pub journey_id: Uuid,
    pub person_ref: String,
    pub subject_id: Uuid,
    /// Identity fields — nulled when the subject is forgotten.
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Free-form PII details — cleared to `{}` when the subject is forgotten.
    pub details: serde_json::Value,
    /// `true` once a `SubjectForgotten` event has been applied for this subject.
    pub forgotten: bool,
}

// Our Journey query using PostgresViewRepository which will serialize and persist
// our view after it is updated. It provides a `load` method to deserialize the view on request.
pub type JourneyQuery =
    GenericQuery<PostgresViewRepository<JourneyView, Journey>, JourneyView, Journey>;

/// The view for a Journey query, designed to reflect the complete state
/// of a journey as stored in the database. This view is updated as events
/// are committed to the event store.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JourneyView {
    /// Unique identifier for the journey
    pub id: Uuid,

    /// Current state of the journey (`InProgress` or `Complete`)
    pub state: JourneyState,

    /// Shared, non-PII data accumulated during the journey.
    /// Never encrypted. Fully intact after any shredding operation.
    pub shared_data: Value,

    /// The current step in the journey workflow
    pub current_step: Option<String>,

    /// The latest workflow decision state including available actions
    pub latest_workflow_decision: Option<WorkflowDecisionView>,

    /// All person slots associated with this journey.
    /// Each slot holds identity fields and free-form PII details encrypted at rest.
    /// Populated by `StructuredJourneyViewRepository::load`; empty in the in-memory view.
    #[serde(default)]
    pub persons: Vec<PersonView>,
}

impl Default for JourneyView {
    fn default() -> Self {
        Self {
            id: Uuid::default(),
            state: JourneyState::default(),
            shared_data: json!({}),
            current_step: None,
            latest_workflow_decision: None,
            persons: Vec::new(),
        }
    }
}

/// Represents the state of a journey in the view
#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
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
                self.id = *id;
                self.state = JourneyState::InProgress;
                self.shared_data = json!({});
                self.current_step = None;
                self.latest_workflow_decision = None;
            }

            JourneyEvent::Modified { step: _, data } => {
                // Merge new data into shared data
                json_patch::merge(&mut self.shared_data, data);
            }

            // Person events are projected to structured database tables by
            // StructuredJourneyViewRepository; no state change needed here.
            // Person events are projected to journey_person by StructuredJourneyViewRepository.
            // The persons field on JourneyView is populated by load(), not from events.
            JourneyEvent::PersonCaptured { .. }
            | JourneyEvent::PersonDetailsUpdated { .. }
            | JourneyEvent::SubjectForgotten { .. } => {}

            JourneyEvent::WorkflowEvaluated { suggested_actions } => {
                self.latest_workflow_decision = Some(WorkflowDecisionView {
                    suggested_actions: suggested_actions.clone(),
                });
            }

            JourneyEvent::StepProgressed {
                from_step: _,
                to_step,
            } => {
                self.current_step = Some(to_step.clone());
            }

            JourneyEvent::Completed => {
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
        assert_eq!(view.shared_data, json!({}));
        assert!(view.current_step.is_none());
        assert!(view.latest_workflow_decision.is_none());
        assert!(view.persons.is_empty());
    }

    #[test]
    fn test_journey_view_modified_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            shared_data: json!({}),
            current_step: None,
            latest_workflow_decision: None,
            persons: vec![],
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

        assert_eq!(view.shared_data.get("user_name"), Some(&json!("John Doe")));
    }

    #[test]
    fn test_journey_view_person_captured_is_noop() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            shared_data: json!({"origin": "LHR"}),
            current_step: None,
            latest_workflow_decision: None,
            persons: vec![],
        };
        let before = view.shared_data.clone();

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 2,
            payload: JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id: Uuid::new_v4(),
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        // shared_data must be untouched
        assert_eq!(view.shared_data, before);
    }

    #[test]
    fn test_journey_view_person_details_updated_is_noop() {
        let id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            shared_data: json!({"origin": "LHR"}),
            current_step: None,
            latest_workflow_decision: None,
            persons: vec![],
        };
        let before = view.shared_data.clone();

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::PersonDetailsUpdated {
                person_ref: "passenger_0".to_string(),
                subject_id,
                data: json!({"passportNumber": "GB123456789"}),
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(view.shared_data, before);
    }

    #[test]
    fn test_journey_view_workflow_evaluated_event() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            shared_data: json!({}),
            current_step: None,
            latest_workflow_decision: None,
            persons: vec![],
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
            shared_data: json!({}),
            current_step: Some("step1".to_string()),
            latest_workflow_decision: None,
            persons: vec![],
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
            shared_data: json!({}),
            current_step: Some("final_step".to_string()),
            latest_workflow_decision: None,
            persons: vec![],
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
    fn test_journey_view_subject_forgotten_is_noop() {
        let id = Uuid::new_v4();
        let mut view = JourneyView {
            id,
            state: JourneyState::InProgress,
            shared_data: json!({"origin": "LHR", "destination": "JFK"}),
            current_step: Some("confirmation".to_string()),
            latest_workflow_decision: None,
            persons: vec![],
        };
        let before_data = view.shared_data.clone();
        let before_step = view.current_step.clone();

        let envelope = EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 6,
            payload: JourneyEvent::SubjectForgotten {
                subject_id: Uuid::new_v4(),
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        // shared_data and current_step must be completely untouched
        assert_eq!(view.shared_data, before_data);
        assert_eq!(view.current_step, before_step);
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

        // Modified with shared data
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 2,
            payload: JourneyEvent::Modified {
                step: "search".to_string(),
                data: json!({"origin": "LHR", "destination": "JFK"}),
            },
            metadata: HashMap::default(),
        });

        // PersonCaptured — must not affect shared_data
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::PersonCaptured {
                person_ref: "passenger_0".to_string(),
                subject_id: Uuid::new_v4(),
                name: "Alice Smith".to_string(),
                email: "alice@example.com".to_string(),
                phone: None,
            },
            metadata: HashMap::default(),
        });

        // WorkflowEvaluated
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 4,
            payload: JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["confirmation".to_string(), "continue".to_string()],
            },
            metadata: HashMap::default(),
        });

        // StepProgressed
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 5,
            payload: JourneyEvent::StepProgressed {
                from_step: None,
                to_step: "confirmation".to_string(),
            },
            metadata: HashMap::default(),
        });

        // Completed
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 6,
            payload: JourneyEvent::Completed,
            metadata: HashMap::default(),
        });

        assert_eq!(view.id, id);
        assert_eq!(view.state, JourneyState::Complete);
        assert_eq!(view.shared_data.get("origin"), Some(&json!("LHR")));
        assert_eq!(view.shared_data.get("destination"), Some(&json!("JFK")));
        assert_eq!(view.current_step, Some("confirmation".to_string()));
        assert!(view.latest_workflow_decision.is_some());
    }
}
