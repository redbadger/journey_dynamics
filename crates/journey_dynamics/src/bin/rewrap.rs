//! One-shot KEK re-wrap CLI.
//!
//! Connects to the database, runs a single sweep that re-wraps every DEK still
//! encrypted under a retired KEK version, prints statistics, and exits.
//!
//! # Usage
//!
//! ```text
//! cargo run --bin rewrap
//! ```
//!
//! Set the same environment variables used by the main server:
//!
//! ```text
//! DATABASE_URL=postgres://...
//!
//! # Multi-version schema (for rotation):
//! JOURNEY_KEK_PRIMARY=v2
//! JOURNEY_KEK_v1=<base64>   # must still be present to unwrap legacy rows
//! JOURNEY_KEK_v2=<base64>
//!
//! # Legacy single-variable schema (backwards-compatible):
//! JOURNEY_KEK=<base64>
//! ```
//!
//! Exits with code 0 on a clean sweep, or 1 if any re-wrap failed or the
//! sweep itself encountered a database error.

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es_crypto::{
    KekProvider, PostgresKeyStore, PostgresKeyStoreOptions, RewrapWorker, RewrapWorkerOptions,
    StaticKekProvider,
};
use postgres_es::default_postgress_pool;

#[tokio::main]
async fn main() {
    // Load .env file if present — same behaviour as the main server.
    dotenv::dotenv().ok();

    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL environment variable must be set");

    println!("Connecting to database…");
    let pool = default_postgress_pool(&database_url).await;

    println!("Running migrations…");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Database migrations failed");

    // Build the KEK provider — mirrors the logic in state.rs.
    let provider: Arc<dyn KekProvider> = if std::env::var("JOURNEY_KEK_PRIMARY").is_ok() {
        Arc::new(
            StaticKekProvider::from_env("JOURNEY_KEK")
                .expect("JOURNEY_KEK_PRIMARY / JOURNEY_KEK_<id> env config is invalid"),
        )
    } else {
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

    let current_kek_id = provider.current().id;
    println!("Current primary KEK id: {current_kek_id}");

    // Build a key store with lazy re-wrap disabled — the sweep is the one doing
    // the re-wrapping; we don't want background spawns competing with it.
    let key_store = Arc::new(PostgresKeyStore::new_with_options(
        pool,
        Arc::clone(&provider),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    ));

    let worker = RewrapWorker::new(
        key_store,
        Arc::clone(&provider),
        RewrapWorkerOptions::default(),
    );

    println!("Starting re-wrap sweep…");
    match worker.run_once().await {
        Ok(stats) => {
            println!(
                "Sweep complete — scanned: {}, re-wrapped: {}, failures: {}, duration: {:?}",
                stats.scanned, stats.rewrapped, stats.failures, stats.duration
            );
            if stats.failures > 0 {
                eprintln!(
                    "ERROR: {} subject(s) failed to re-wrap. Check logs for details.",
                    stats.failures
                );
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("ERROR: Sweep failed with a database error: {e}");
            std::process::exit(1);
        }
    }
}
