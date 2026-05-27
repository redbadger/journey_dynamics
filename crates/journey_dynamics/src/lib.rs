pub mod command_extractor;
pub mod config;
pub mod domain;
pub mod pii_codec;
pub mod queries;
pub mod route_handler;
pub mod services;
pub mod state;
pub mod subject_lookup_hook;
pub mod view_repository;

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
            let json = serde_json::to_string_pretty(&event.payload)
                .unwrap_or_else(|_| "failed to serialize event payload".to_string());
            println!("{}-{}\n{}", aggregate_id, event.sequence, &json);
        }
    }
}
