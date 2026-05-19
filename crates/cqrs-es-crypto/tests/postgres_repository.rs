//! Integration tests for the transactional persist path of
//! [`CryptoShreddingEventRepository`] backed by a real `PostgreSQL` database.
//!
//! These tests require a live `PostgreSQL` database and are only compiled and run
//! when both the `postgres` and `testing` Cargo features are enabled (enforced
//! via `required-features` in `Cargo.toml`).

use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es::{
    DomainEvent,
    event_sink::EventSink,
    persist::{PersistedEventRepository, PersistenceError, SerializedEvent},
};
use cqrs_es_crypto::{
    CryptoShreddingEventRepository, EncryptedPiiExtract, EncryptedPiiSentinel, FieldCipher,
    InMemoryEventRepository, KekProvider, KeyStore, PersistHook, PiiEventCodec, PiiFields,
    PostgresKeyStore, StaticKekProvider,
};
use uuid::Uuid;

// ── TestEvent ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
enum TestEvent {
    TestPii { subject_id: String, secret: String },
    TestPlain { data: String },
}

impl DomainEvent for TestEvent {
    fn event_type(&self) -> String {
        match self {
            Self::TestPii { .. } => "TestPii".to_string(),
            Self::TestPlain { .. } => "TestPlain".to_string(),
        }
    }
    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}

// ── TestAggregate ─────────────────────────────────────────────────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct TestAggregate;

impl cqrs_es::Aggregate for TestAggregate {
    type Command = ();
    type Event = TestEvent;
    type Error = std::convert::Infallible;
    type Services = ();

    const TYPE: &'static str = "Test";

    async fn handle(
        &mut self,
        _command: (),
        _services: &(),
        _sink: &EventSink<Self>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn apply(&mut self, _event: TestEvent) {}
}

// ── TestPiiCodec ──────────────────────────────────────────────────────────────

struct TestPiiCodec;

impl PiiEventCodec for TestPiiCodec {
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields> {
        if event.event_type != "TestPii" {
            return None;
        }

        let subject_id_str = event.payload["TestPii"]["subject_id"].as_str()?.to_string();
        let subject_id = Uuid::parse_str(&subject_id_str).ok()?;
        let plaintext_pii = serde_json::json!({
            "secret": event.payload["TestPii"]["secret"].clone(),
        });

        Some(PiiFields {
            subject_id,
            plaintext_pii,
            build_encrypted_payload: Box::new(
                move |EncryptedPiiSentinel {
                          ciphertext_b64,
                          nonce_b64,
                      }| {
                    serde_json::json!({
                        "TestPii": {
                            "subject_id":    subject_id_str,
                            "encrypted_pii": ciphertext_b64,
                            "nonce":         nonce_b64,
                        }
                    })
                },
            ),
        })
    }

    fn extract_encrypted(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
        if event.event_type != "TestPii" {
            return None;
        }
        // No sentinel → legacy plaintext event, pass through.
        event.payload["TestPii"].get("encrypted_pii")?;

        let subject_id = Uuid::parse_str(event.payload["TestPii"]["subject_id"].as_str()?).ok()?;
        let ciphertext = BASE64
            .decode(event.payload["TestPii"]["encrypted_pii"].as_str()?)
            .ok()?;
        let nonce = BASE64
            .decode(event.payload["TestPii"]["nonce"].as_str()?)
            .ok()?;

        Some(EncryptedPiiExtract {
            subject_id,
            ciphertext,
            nonce,
        })
    }

    fn reconstruct(
        &self,
        event: &SerializedEvent,
        plaintext_pii: &serde_json::Value,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let subject_id = event.payload["TestPii"]["subject_id"].clone();
        Ok(serde_json::json!({
            "TestPii": {
                "subject_id": subject_id,
                "secret":     plaintext_pii["secret"].clone(),
            }
        }))
    }

    fn redact(
        &self,
        event: &SerializedEvent,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
        let subject_id = event.payload["TestPii"]["subject_id"].clone();
        Ok(serde_json::json!({
            "TestPii": {
                "subject_id": subject_id,
                "secret":     "[redacted]",
            }
        }))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn setup_test_db() -> sqlx::Pool<sqlx::Postgres> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgres@localhost:5432/journey_dynamics".to_string()
    });
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("Failed to connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");
    pool
}

fn make_provider() -> Arc<dyn KekProvider> {
    Arc::new(StaticKekProvider::single("test:v1", vec![0x42u8; 32]).expect("valid KEK"))
}

// The transactional write path bypasses inner.persist(), so
// InMemoryEventRepository is sufficient as the inner repo.
fn make_transactional_repo(
    pool: sqlx::Pool<sqlx::Postgres>,
) -> CryptoShreddingEventRepository<InMemoryEventRepository> {
    let provider = make_provider();
    let key_store: Arc<dyn KeyStore> =
        Arc::new(PostgresKeyStore::new(pool.clone(), Arc::clone(&provider)));
    let codec = Arc::new(TestPiiCodec);
    CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        FieldCipher::new(),
        codec,
    )
    .with_transactional_writes(pool, provider)
}

async fn event_row_count(pool: &sqlx::Pool<sqlx::Postgres>, aggregate_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE aggregate_id = $1")
        .bind(aggregate_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn dek_exists(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) -> bool {
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .fetch_one(pool)
            .await
            .unwrap();
    count > 0
}

async fn cleanup(pool: &sqlx::Pool<sqlx::Postgres>, aggregate_id: &str) {
    let _ = sqlx::query("DELETE FROM events WHERE aggregate_id = $1")
        .bind(aggregate_id)
        .execute(pool)
        .await;
}

async fn cleanup_dek(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) {
    let _ = sqlx::query("DELETE FROM subject_encryption_keys WHERE subject_id = $1")
        .bind(subject_id)
        .execute(pool)
        .await;
}

fn pii_event(aggregate_id: &str, sequence: usize, subject_id: Uuid) -> SerializedEvent {
    SerializedEvent::new(
        aggregate_id.to_string(),
        sequence,
        "Test".to_string(),
        "TestPii".to_string(),
        "1.0".to_string(),
        serde_json::json!({
            "TestPii": {
                "subject_id": subject_id.to_string(),
                "secret":     "hunter2",
            }
        }),
        serde_json::json!({}),
    )
}

fn plain_event(aggregate_id: &str, sequence: usize) -> SerializedEvent {
    SerializedEvent::new(
        aggregate_id.to_string(),
        sequence,
        "Test".to_string(),
        "TestPlain".to_string(),
        "1.0".to_string(),
        serde_json::json!({ "TestPlain": { "data": "no secrets here" } }),
        serde_json::json!({}),
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_transactional_persist_writes_dek_and_events_atomically() {
    // A successful persist must commit both the DEK and the event row
    // in the same transaction.
    let pool = setup_test_db().await;
    let repo = make_transactional_repo(pool.clone());
    let aggregate_id = format!("tx-atomic-{}", Uuid::new_v4());
    let subject_id = Uuid::new_v4();

    repo.persist::<TestAggregate>(&[pii_event(&aggregate_id, 1, subject_id)], None)
        .await
        .unwrap();

    assert_eq!(event_row_count(&pool, &aggregate_id).await, 1);
    assert!(dek_exists(&pool, &subject_id).await);

    cleanup(&pool, &aggregate_id).await;
    cleanup_dek(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_hook_receives_unencrypted_events() {
    // The hook must see the plaintext payload — not the encrypted form.
    use std::sync::Mutex;

    struct CapturingHook {
        captured: Arc<Mutex<Vec<SerializedEvent>>>,
    }

    #[async_trait]
    impl PersistHook for CapturingHook {
        async fn on_persist(
            &self,
            events: &[SerializedEvent],
            _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ) -> Result<(), PersistenceError> {
            self.captured.lock().unwrap().extend_from_slice(events);
            Ok(())
        }
    }

    let pool = setup_test_db().await;
    let captured = Arc::new(Mutex::new(vec![]));
    let hook = CapturingHook {
        captured: Arc::clone(&captured),
    };

    let provider = make_provider();
    let key_store: Arc<dyn KeyStore> =
        Arc::new(PostgresKeyStore::new(pool.clone(), Arc::clone(&provider)));
    let codec = Arc::new(TestPiiCodec);
    let repo = CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        FieldCipher::new(),
        codec,
    )
    .with_transactional_writes(pool.clone(), Arc::clone(&provider))
    .with_persist_hook(Arc::new(hook));

    let aggregate_id = format!("tx-hook-plain-{}", Uuid::new_v4());
    let subject_id = Uuid::new_v4();

    repo.persist::<TestAggregate>(&[pii_event(&aggregate_id, 1, subject_id)], None)
        .await
        .unwrap();

    let seen = captured.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    // The hook must have received the PLAINTEXT payload.
    assert!(
        seen[0].payload["TestPii"].get("secret").is_some(),
        "hook must see plaintext secret field, not encrypted sentinel"
    );
    assert!(
        seen[0].payload["TestPii"].get("encrypted_pii").is_none(),
        "hook must not see encrypted_pii sentinel"
    );

    cleanup(&pool, &aggregate_id).await;
    cleanup_dek(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_hook_error_rolls_back_entire_transaction() {
    // If the hook returns an error, the transaction must roll back —
    // neither the DEK nor the event row must be committed.
    struct FailingHook;

    #[async_trait]
    impl PersistHook for FailingHook {
        async fn on_persist(
            &self,
            _events: &[SerializedEvent],
            _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ) -> Result<(), PersistenceError> {
            Err(PersistenceError::UnknownError("hook failure".into()))
        }
    }

    let pool = setup_test_db().await;
    let provider = make_provider();
    let key_store: Arc<dyn KeyStore> =
        Arc::new(PostgresKeyStore::new(pool.clone(), Arc::clone(&provider)));
    let codec = Arc::new(TestPiiCodec);
    let repo = CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        FieldCipher::new(),
        codec,
    )
    .with_transactional_writes(pool.clone(), Arc::clone(&provider))
    .with_persist_hook(Arc::new(FailingHook));

    let aggregate_id = format!("tx-rollback-{}", Uuid::new_v4());
    let subject_id = Uuid::new_v4();

    let result = repo
        .persist::<TestAggregate>(&[pii_event(&aggregate_id, 1, subject_id)], None)
        .await;

    assert!(result.is_err(), "persist must propagate the hook error");
    // The transaction must have been rolled back.
    assert_eq!(
        event_row_count(&pool, &aggregate_id).await,
        0,
        "event must not be committed when the hook fails"
    );
    // DEK may or may not exist (created before the hook runs);
    // clean up regardless to avoid leaking test data.
    cleanup_dek(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_get_or_create_key_in_tx_reuses_dek_within_batch() {
    // Two PII events for the same subject in one persist call must
    // result in exactly one DEK row, not two.
    let pool = setup_test_db().await;
    let repo = make_transactional_repo(pool.clone());
    let aggregate_id = format!("tx-dek-reuse-{}", Uuid::new_v4());
    let subject_id = Uuid::new_v4();

    repo.persist::<TestAggregate>(
        &[
            pii_event(&aggregate_id, 1, subject_id),
            pii_event(&aggregate_id, 2, subject_id),
        ],
        None,
    )
    .await
    .unwrap();

    let dek_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dek_count, 1, "exactly one DEK row must exist");

    cleanup(&pool, &aggregate_id).await;
    cleanup_dek(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_transactional_persist_plain_events_roundtrip() {
    // Plain (non-PII) events written via the transactional path must
    // appear in the events table — regression guard for the #[cfg] routing.
    // We query the DB directly because inner is InMemoryEventRepository
    // and doesn't see writes made via the transactional path.
    let pool = setup_test_db().await;
    let repo = make_transactional_repo(pool.clone());
    let aggregate_id = format!("tx-plain-rt-{}", Uuid::new_v4());

    repo.persist::<TestAggregate>(&[plain_event(&aggregate_id, 1)], None)
        .await
        .unwrap();

    // Verify the row exists in the DB.
    assert_eq!(event_row_count(&pool, &aggregate_id).await, 1);

    // Verify the payload was stored verbatim (no encryption on plain events).
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM events WHERE aggregate_id = $1")
            .bind(&aggregate_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        payload["TestPlain"]["data"].as_str().unwrap(),
        "no secrets here"
    );

    cleanup(&pool, &aggregate_id).await;
}
