use crate::config::cqrs_framework;
use crate::domain::journey::Journey;
use crate::view_repository::StructuredJourneyViewRepository;
use postgres_es::{PostgresCqrs, default_postgress_pool};
use std::sync::Arc;

#[derive(Clone)]
pub struct ApplicationState {
    pub cqrs: Arc<PostgresCqrs<Journey>>,
    pub journey_query: Arc<StructuredJourneyViewRepository>,
}

#[allow(clippy::missing_panics_doc)]
pub async fn new_application_state() -> ApplicationState {
    // Configure the CQRS framework, backed by a Postgres database, along with two queries:
    // - a simple query that prints events to stdout as they are published
    // - `journey_query` stores the current state of journeys in structured SQL tables
    //
    // The needed database tables are automatically configured with `docker-compose up -d`,
    // see init file at `/db/init.sql` for more.
    let pool = default_postgress_pool(std::env::var("DATABASE_URL").unwrap().as_str()).await;
    let (cqrs, journey_query) = cqrs_framework(pool);
    ApplicationState {
        cqrs,
        journey_query,
    }
}
