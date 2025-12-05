pub mod domain;
pub mod services;

use async_trait::async_trait;
use cqrs_es::{Aggregate, EventEnvelope, Query};

pub struct SimpleLoggingQuery {}

#[async_trait]
impl<A> Query<A> for SimpleLoggingQuery
where
    A: Aggregate,
{
    async fn dispatch(&self, aggregate_id: &str, events: &[EventEnvelope<A>]) {
        for event in events {
            println!("{}-{}\n{:#?}", aggregate_id, event.sequence, &event.payload);
        }
    }
}
