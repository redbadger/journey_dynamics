use async_trait::async_trait;
use cqrs_es::persist::GenericQuery;
use cqrs_es::{EventEnvelope, Query, View};
use postgres_es::PostgresViewRepository;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::events::JourneyEvent;
use crate::domain::journey::Journey;

// Our second query, this one will be handled with Postgres `GenericQuery`
// which will serialize and persist our view after it is updated. It also
// provides a `load` method to deserialize the view on request.
pub type JourneyQuery =
    GenericQuery<PostgresViewRepository<JourneyView, Journey>, JourneyView, Journey>;

// The view for a BankAccount query, for a standard http application this should
// be designed to reflect the response dto that will be returned to a user.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct JourneyView {
    id: Uuid,
}

// This updates the view with events as they are committed.
// The logic should be minimal here, e.g., don't calculate the account balance,
// design the events to carry the balance information instead.
impl View<Journey> for JourneyView {
    fn update(&mut self, event: &EventEnvelope<Journey>) {
        match &event.payload {
            JourneyEvent::Started { id } => self.id = id.clone(),
            JourneyEvent::Modified { form_data } => todo!(),
            JourneyEvent::WorkflowEvaluated {
                available_actions,
                primary_next_step,
            } => todo!(),
            JourneyEvent::StepProgressed { from_step, to_step } => todo!(),
            JourneyEvent::Completed => todo!(),
        }
    }
}
