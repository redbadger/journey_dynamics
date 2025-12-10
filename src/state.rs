use crate::config::cqrs_framework;
use crate::domain::journey::Journey;
use crate::queries::JourneyView;
use postgres_es::{PostgresCqrs, PostgresViewRepository, default_postgress_pool};
use std::sync::Arc;

#[derive(Clone)]
pub struct ApplicationState {
    pub cqrs: Arc<PostgresCqrs<Journey>>,
    pub journey_query: Arc<PostgresViewRepository<JourneyView, Journey>>,
}

pub async fn new_application_state() -> ApplicationState {
    // Configure the CQRS framework, backed by a Postgres database, along with two queries:
    // - a simply-query prints events to stdout as they are published
    // - `account_query` stores the current state of the account in a ViewRepository that we can access
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
