use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use postgres_es::default_postgress_pool;

use cqrs_es_crypto::{
    FieldCipher, KeyStore, PostgresKeyStore, RewrapWorker, RewrapWorkerOptions, StaticKekProvider,
};

use crate::{
    config::{CryptoCqrs, cqrs_framework},
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
/// - `JOURNEY_KEK_PRIMARY` or the corresponding key variable are not set, are
///   not valid base64, or do not decode to exactly 32 bytes each
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

    // Build the KEK provider from environment variables.
    //
    // Multi-version (rotation) schema:
    //   JOURNEY_KEK_PRIMARY=v2
    //   JOURNEY_KEK_v1=<base64>   ← still readable for legacy rows
    //   JOURNEY_KEK_v2=<base64>   ← used for new wraps
    //
    // Single-version (existing deployments) schema — kept for backwards compat:
    //   JOURNEY_KEK=<base64>
    //
    // We prefer the multi-version schema; fall back to the single-version variable.
    let provider: Arc<dyn cqrs_es_crypto::KekProvider> =
        if std::env::var("JOURNEY_KEK_PRIMARY").is_ok() {
            Arc::new(
                StaticKekProvider::from_env("JOURNEY_KEK")
                    .expect("JOURNEY_KEK_PRIMARY / JOURNEY_KEK_<id> env config is invalid"),
            )
        } else {
            // Legacy single-variable path: JOURNEY_KEK=<base64-encoded 32-byte key>
            let kek_b64 = std::env::var("JOURNEY_KEK")
                .expect("Either JOURNEY_KEK_PRIMARY or JOURNEY_KEK must be set");
            let kek = BASE64
                .decode(kek_b64.trim())
                .expect("JOURNEY_KEK must be valid base64");
            Arc::new(
                StaticKekProvider::single("legacy:v1", kek)
                    .expect("JOURNEY_KEK must decode to exactly 32 bytes"),
            )
        };

    // Boot-time fail-fast: verify every kek_id in the database is resolvable
    // by the current provider.  If not, a KEK version was retired before all DEKs
    // were re-wrapped — serving traffic would cause decryption failures at request
    // time, which is far harder to diagnose than a clean startup panic.
    {
        let unknown: Vec<String> =
            sqlx::query_scalar::<_, String>("SELECT DISTINCT kek_id FROM subject_encryption_keys")
                .fetch_all(&pool)
                .await
                .expect("Failed to query kek_ids from subject_encryption_keys")
                .into_iter()
                .filter(|id| provider.by_id(id).is_none())
                .collect();

        assert!(
            unknown.is_empty(),
            "FATAL: {} kek_id value(s) in subject_encryption_keys are not known to \
             the configured KekProvider: {unknown:?}. A KEK version was likely retired \
             before all DEKs were re-wrapped under the new version. Restore the \
             missing KEK variable(s) or run `cargo run --bin rewrap` to diagnose. \
             See docs/KEK_ROTATION_RUNBOOK.md.",
            unknown.len(),
        );
    }

    // The KeyStore owns the provider and uses it to wrap/unwrap DEKs.
    let key_store: Arc<dyn KeyStore> =
        Arc::new(PostgresKeyStore::new(pool.clone(), Arc::clone(&provider)));

    // The CryptoShreddingEventRepository uses a stateless FieldCipher for
    // AES-256-GCM field encryption — it does not need the KEK at all.
    let cipher = FieldCipher::new();

    let (cqrs, journey_query) = cqrs_framework(pool, Arc::clone(&key_store), cipher);

    // Spawn the background re-wrap sweeper.  It polls every 5 minutes and re-wraps
    // any DEKs still encrypted under a retired KEK version.  Safe to run alongside
    // live traffic — the CAS UPDATE in rewrap_key makes concurrent re-wraps idempotent.
    let worker = RewrapWorker::new(
        Arc::clone(&key_store),
        Arc::clone(&provider),
        RewrapWorkerOptions::default(),
    );
    tokio::spawn(async move {
        worker.run_forever(std::time::Duration::from_mins(5)).await;
    });

    ApplicationState {
        cqrs,
        journey_query,
        key_store,
    }
}
