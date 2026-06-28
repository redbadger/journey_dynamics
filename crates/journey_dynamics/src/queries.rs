#![allow(deprecated)]
use cqrs_es::{EventEnvelope, View, persist::GenericQuery};
use postgres_es::PostgresViewRepository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::domain::{assign_all, events::JourneyEvent, journey::Journey};

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
    ///
    /// Deprecated: the canonical location for per-person attributes is
    /// `shared_data` under `persons/<ref>/…`. This field is a back-compat
    /// mirror; prefer reading from `JourneyView.shared_data`.
    #[deprecated(
        since = "0.3.0",
        note = "read from shared_data under persons/<ref>/… instead"
    )]
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

    /// The current step in the journey workflow.
    ///
    /// Deprecated: read `WorkflowEvaluated.phase` from `shared_data` instead.
    #[deprecated(
        since = "0.3.0",
        note = "read WorkflowEvaluated.phase from shared_data instead"
    )]
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
    /// Phase label persisted from the decision engine output.
    /// `None` until the `WorkflowEvaluated` event carries `phase` (step B1).
    #[serde(default)]
    pub phase: Option<String>,
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

            // Subject events are projected to journey_person by
            // StructuredJourneyViewRepository; the persons field on JourneyView
            // is populated by load(), not from events — no in-memory update here.
            JourneyEvent::SubjectForgotten { .. }
            | JourneyEvent::SubjectRegistered { .. }
            | JourneyEvent::SubjectBound { .. } => {}

            JourneyEvent::WorkflowEvaluated {
                suggested_actions,
                phase,
            } => {
                self.latest_workflow_decision = Some(WorkflowDecisionView {
                    suggested_actions: suggested_actions.clone(),
                    phase: phase.clone(),
                });
            }

            JourneyEvent::Completed => {
                self.state = JourneyState::Complete;
            }

            JourneyEvent::AttributesSet { plaintext, .. } => {
                // Merge plaintext changes into shared_data.
                // Secret partitions are projected to journey_person by
                // StructuredJourneyViewRepository; no state change needed here.
                assign_all(&mut self.shared_data, plaintext).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(deprecated)]
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
    fn test_journey_view_attributes_set_merges_plaintext() {
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
            payload: JourneyEvent::AttributesSet {
                plaintext: std::collections::BTreeMap::from([(
                    "/user_name".parse().unwrap(),
                    json!("John Doe"),
                )]),
                secret_partitions: vec![],
            },
            metadata: HashMap::default(),
        };

        view.update(&envelope);

        assert_eq!(view.shared_data.get("user_name"), Some(&json!("John Doe")));
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
                phase: None,
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

        // AttributesSet with shared data
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 2,
            payload: JourneyEvent::AttributesSet {
                plaintext: std::collections::BTreeMap::from([
                    ("/origin".parse().unwrap(), json!("LHR")),
                    ("/destination".parse().unwrap(), json!("JFK")),
                ]),
                secret_partitions: vec![],
            },
            metadata: HashMap::default(),
        });

        // WorkflowEvaluated
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 3,
            payload: JourneyEvent::WorkflowEvaluated {
                suggested_actions: vec!["confirmation".to_string(), "continue".to_string()],
                phase: Some("confirmation".to_string()),
            },
            metadata: HashMap::default(),
        });

        // Completed
        view.update(&EventEnvelope {
            aggregate_id: id.to_string(),
            sequence: 4,
            payload: JourneyEvent::Completed,
            metadata: HashMap::default(),
        });

        assert_eq!(view.id, id);
        assert_eq!(view.state, JourneyState::Complete);
        assert_eq!(view.shared_data.get("origin"), Some(&json!("LHR")));
        assert_eq!(view.shared_data.get("destination"), Some(&json!("JFK")));
        assert!(view.latest_workflow_decision.is_some());
    }
}
