//! Journey events.
//!
//! The journey domain uses the generic capture event set unchanged, re-exported
//! here under historical names.

pub use es_capture::aggregate::{CaptureEvent as JourneyEvent, SecretPartitionData};

#[cfg(test)]
mod tests {
    use cqrs_es::DomainEvent;

    use super::JourneyEvent;

    /// A v1.0 `WorkflowEvaluated` payload (no `phase`) must deserialise to
    /// `phase: None`.
    #[test]
    fn workflow_evaluated_v1_0_fixture_deserialises_to_phase_none() {
        let json = r#"{"WorkflowEvaluated": {"suggested_actions": ["next"]}}"#;
        let event: JourneyEvent = serde_json::from_str(json).unwrap();
        match event {
            JourneyEvent::WorkflowEvaluated {
                suggested_actions,
                phase,
            } => {
                assert_eq!(suggested_actions, vec!["next".to_string()]);
                assert!(phase.is_none(), "phase must be None for v1.0 payload");
            }
            other => panic!("expected WorkflowEvaluated, got {other:?}"),
        }
    }

    /// A v1.1 `WorkflowEvaluated` payload (with `phase`) round-trips.
    #[test]
    fn workflow_evaluated_v1_1_round_trips_phase() {
        let event = JourneyEvent::WorkflowEvaluated {
            suggested_actions: vec!["confirm".to_string()],
            phase: Some("collecting_passengers".to_string()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: JourneyEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, decoded);
        assert_eq!(event.event_version(), "1.1");
    }
}
