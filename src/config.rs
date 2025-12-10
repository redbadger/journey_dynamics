use std::sync::Arc;

use cqrs_es::Query;
use postgres_es::{PostgresCqrs, PostgresViewRepository};
use sqlx::{Pool, Postgres};

use crate::SimpleLoggingQuery;
use crate::domain::journey::{Journey, JourneyServices};
use crate::queries::{JourneyQuery, JourneyView};
use crate::services::decision_engine::GoRulesDecisionEngine;

pub fn cqrs_framework(
    pool: Pool<Postgres>,
) -> (
    Arc<PostgresCqrs<Journey>>,
    Arc<PostgresViewRepository<JourneyView, Journey>>,
) {
    // A very simple query that writes each event to stdout.
    let simple_query = SimpleLoggingQuery {};

    // A query that stores the current state of an individual account.
    let account_view_repo = Arc::new(PostgresViewRepository::new("journey_query", pool.clone()));
    let mut account_query = JourneyQuery::new(account_view_repo.clone());

    // Without a query error handler there will be no indication if an
    // error occurs (e.g., database connection failure, missing columns or table).
    // Consider logging an error or panicking in your own application.
    account_query.use_error_handler(Box::new(|e| println!("{e}")));

    // Create and return an event-sourced `CqrsFramework`.
    let queries: Vec<Box<dyn Query<Journey>>> =
        vec![Box::new(simple_query), Box::new(account_query)];
    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let services = JourneyServices::new(decision_engine);
    (
        Arc::new(postgres_es::postgres_cqrs(pool, queries, services)),
        account_view_repo,
    )
}
