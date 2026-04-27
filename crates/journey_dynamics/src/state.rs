use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use postgres_es::default_postgress_pool;

use crate::{
    config::{CryptoCqrs, cqrs_framework},
    crypto::{
        cipher::PiiCipher,
        key_store::{KeyStore, PostgresKeyStore},
    },
    view_repository::StructuredJourneyViewRepository,
};

#[derive(Clone)]
pub struct ApplicationState {
    pub cqrs: Arc<CryptoCqrs>,
    pub journey_query: Arc<StructuredJourneyViewRepository>,
    pub key_store: Arc<dyn KeyStore>,
}

/// # Panics
///
/// Panics if:
/// - `DATABASE_URL` environment variable is not set
/// - `JOURNEY_KEK` environment variable is not set, is not valid base64, or does not decode
///   to exactly 32 bytes
/// - Database migrations fail
#[allow(clippy::missing_panics_doc)]
pub async fn new_application_state() -> ApplicationState {
    let pool = default_postgress_pool(
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL environment variable must be set")
            .as_str(),
    )
    .await;

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run database migrations");

    // Load the Key Encryption Key (KEK) from the environment.
    // Generate one with: openssl rand -base64 32
    let kek_b64 = std::env::var("JOURNEY_KEK")
        .expect("JOURNEY_KEK must be set (generate with: openssl rand -base64 32)");
    let kek = BASE64
        .decode(kek_b64.trim())
        .expect("JOURNEY_KEK must be valid base64");

    // The KeyStore needs its own PiiCipher instance for wrapping/unwrapping DEKs.
    let key_store: Arc<dyn KeyStore> = Arc::new(PostgresKeyStore::new(
        pool.clone(),
        PiiCipher::new(kek.clone()).expect("JOURNEY_KEK must decode to exactly 32 bytes"),
    ));

    // The CryptoShreddingEventRepository gets its own PiiCipher for field encryption.
    let cipher = PiiCipher::new(kek).expect("JOURNEY_KEK must decode to exactly 32 bytes");

    let (cqrs, journey_query) = cqrs_framework(pool, Arc::clone(&key_store), cipher);

    ApplicationState {
        cqrs,
        journey_query,
        key_store,
    }
}
