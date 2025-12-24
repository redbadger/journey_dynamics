use std::sync::Arc;

use cqrs_es::Query;
use postgres_es::PostgresCqrs;
use sqlx::{Pool, Postgres};

use crate::SimpleLoggingQuery;
use crate::domain::journey::{Journey, JourneyServices};
use crate::services::decision_engine::GoRulesDecisionEngine;
use crate::view_repository::StructuredJourneyViewRepository;

#[must_use]
pub fn cqrs_framework(
    pool: Pool<Postgres>,
) -> (
    Arc<PostgresCqrs<Journey>>,
    Arc<StructuredJourneyViewRepository>,
) {
    // A very simple query that writes each event to stdout.
    let simple_query = SimpleLoggingQuery {};

    // A structured query that stores journey data in proper SQL tables
    let journey_view_repo = Arc::new(StructuredJourneyViewRepository::new(pool.clone()));

    // Create and return an event-sourced `CqrsFramework`.
    let queries: Vec<Box<dyn Query<Journey>>> = vec![
        Box::new(simple_query),
        Box::new((*journey_view_repo).clone()),
    ];

    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(
        decision_engine,
        Arc::new(crate::services::schema_validator::NoOpValidator),
    );

    (
        Arc::new(postgres_es::postgres_cqrs(pool, queries, services)),
        journey_view_repo,
    )
}
