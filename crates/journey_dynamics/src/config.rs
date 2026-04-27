use std::sync::Arc;

use cqrs_es::{CqrsFramework, Query, persist::PersistedEventStore};
use postgres_es::PostgresEventRepository;
use sqlx::{Pool, Postgres};

use crate::SimpleLoggingQuery;
use crate::{
    crypto::{cipher::PiiCipher, key_store::KeyStore, repository::CryptoShreddingEventRepository},
    domain::journey::{Journey, JourneyServices},
    services::decision_engine::GoRulesDecisionEngine,
    view_repository::StructuredJourneyViewRepository,
};

/// The CQRS framework type used throughout the application.
///
/// Wraps [`PostgresEventRepository`] with [`CryptoShreddingEventRepository`] so that
/// PII fields are encrypted at rest and crypto-shredded on right-to-erasure requests.
pub type CryptoCqrs = CqrsFramework<
    Journey,
    PersistedEventStore<CryptoShreddingEventRepository<PostgresEventRepository>, Journey>,
>;

/// Build the CQRS framework and the journey view repository.
///
/// The caller is responsible for creating the [`PiiCipher`] and [`KeyStore`] so that
/// the same instances can also be held in
/// [`ApplicationState`](crate::state::ApplicationState) for use by the shredding endpoint.
///
/// # Panics
///
/// Panics if the JSON schema file cannot be parsed or compiled.
#[must_use]
pub fn cqrs_framework(
    pool: Pool<Postgres>,
    key_store: Arc<dyn KeyStore>,
    cipher: PiiCipher,
) -> (Arc<CryptoCqrs>, Arc<StructuredJourneyViewRepository>) {
    let simple_query = SimpleLoggingQuery {};

    let journey_view_repo = Arc::new(StructuredJourneyViewRepository::new(pool.clone()));

    let queries: Vec<Box<dyn Query<Journey>>> = vec![
        Box::new(simple_query),
        Box::new((*journey_view_repo).clone()),
    ];

    let decision_engine = Arc::new(GoRulesDecisionEngine::new(include_str!(
        "../../../examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json"
    )));
    let schema_validator = Arc::new(
        crate::services::schema_validator::JsonSchemaValidator::from_json_str(include_str!(
            "../../../examples/flight-booking/schemas/flight-booking-schema.json"
        ))
        .expect("flight-booking JSON schema is invalid — this is a compile-time programming error"),
    );

    let services = JourneyServices::new(decision_engine, schema_validator);

    let inner = PostgresEventRepository::new(pool);
    let crypto_repo = CryptoShreddingEventRepository::new(inner, key_store, cipher);
    let store = PersistedEventStore::new_event_store(crypto_repo);

    (
        Arc::new(CqrsFramework::new(store, queries, services)),
        journey_view_repo,
    )
}
