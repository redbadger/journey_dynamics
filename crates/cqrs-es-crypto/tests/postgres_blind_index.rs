//! End-to-end blind-index tests against a real Postgres.
//!
//! Proves the erasure claim on the path consumers actually use: the lookup row
//! is written transactionally with the event, resolves before shred, and is
//! removed by the FK `ON DELETE CASCADE` when the DEK is deleted via
//! `delete_key_in_tx` inside a caller-owned transaction. Compiled/run only with
//! `blind-index`, `postgres`, and `testing` (see `Cargo.toml`).

#![cfg(all(feature = "blind-index", feature = "postgres", feature = "testing"))]

use std::sync::Arc;

use cqrs_es::DomainEvent;
use cqrs_es::persist::{PersistedEventRepository, SerializedEvent};
use cqrs_es_crypto::{
    BlindIndexHook, BlindIndexKeyProvider, CryptoShreddingEventRepository, DecryptedPartition,
    FieldCipher, FieldSource, InMemoryEventRepository, KekProvider, KeyStore, PiiCodecError,
    PiiEventCodec, PostgresKeyStore, SecretPartition, StaticBlindIndexKeyProvider,
    StaticKekProvider,
};
use uuid::Uuid;
use zeroize::Zeroizing;

const LOOKUP_TYPE: &str = "secret";

// ── Minimal test aggregate + single-secret codec (mirrors postgres_repository.rs) ──

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
enum TestEvent {
    TestPii {
        subject_id: String,
        secret: serde_json::Value,
    },
}

impl DomainEvent for TestEvent {
    fn event_type(&self) -> String {
        "TestPii".to_string()
    }
    fn event_version(&self) -> String {
        "1.0".to_string()
    }
}

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
        _c: (),
        _s: &(),
        _sink: &cqrs_es::event_sink::EventSink<Self>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
    fn apply(&mut self, _event: TestEvent) {}
}

struct TestPiiCodec;

impl PiiEventCodec for TestPiiCodec {
    fn extract_partitions(
        &self,
        event: &mut SerializedEvent,
    ) -> Result<Vec<SecretPartition>, PiiCodecError> {
        if event.event_type != "TestPii" {
            return Ok(vec![]);
        }
        let Some(subject_id) = event.payload["TestPii"]["subject_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(vec![]);
        };
        let secret = event.payload["TestPii"]["secret"].clone();
        if secret.is_null() {
            return Ok(vec![]);
        }
        if let Some(obj) = event.payload["TestPii"].as_object_mut() {
            obj.remove("secret");
        }
        Ok(vec![SecretPartition {
            subject_id,
            label: "default".to_string(),
            payload: serde_json::to_vec(&serde_json::json!({ "secret": secret }))?,
        }])
    }

    fn reconstruct(
        &self,
        event: &mut SerializedEvent,
        partitions: Vec<DecryptedPartition>,
    ) -> Result<(), PiiCodecError> {
        if event.event_type != "TestPii" {
            return Ok(());
        }
        for part in partitions {
            if part.label == "default" {
                let pii: serde_json::Value = serde_json::from_slice(&part.payload)?;
                if let Some(obj) = event.payload["TestPii"].as_object_mut() {
                    obj.insert("secret".to_string(), pii["secret"].clone());
                }
            }
        }
        Ok(())
    }

    fn redact_partitions(
        &self,
        event: &mut SerializedEvent,
        labels: &[String],
    ) -> Result<(), PiiCodecError> {
        if event.event_type == "TestPii" && labels.iter().any(|l| l == "default") {
            if let Some(obj) = event.payload["TestPii"].as_object_mut() {
                obj.insert("secret".to_string(), serde_json::json!("[redacted]"));
            }
        }
        Ok(())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Connect, migrate, and create a per-test uniquely-named lookup table with the
/// load-bearing cascade FK. A unique name avoids the `CREATE TABLE` race on
/// Postgres's `pg_type` catalog when tests run concurrently.
async fn setup() -> (sqlx::Pool<sqlx::Postgres>, String) {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgres@localhost:5432/journey_dynamics".to_string()
    });
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let table = format!("bi_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(
        "CREATE TABLE {table} (
            subject_id   UUID  NOT NULL,
            lookup_type  TEXT  NOT NULL,
            lookup_value BYTEA NOT NULL,
            PRIMARY KEY (subject_id, lookup_type),
            FOREIGN KEY (subject_id) REFERENCES subject_encryption_keys(subject_id) ON DELETE CASCADE
        )"
    ))
    .execute(&pool)
    .await
    .expect("create lookup table");
    (pool, table)
}

fn provider() -> Arc<dyn KekProvider> {
    Arc::new(StaticKekProvider::single("test:v1", vec![0x42u8; 32]).unwrap())
}

fn bi_keys() -> Arc<StaticBlindIndexKeyProvider> {
    Arc::new(StaticBlindIndexKeyProvider::new(Zeroizing::new(vec![9u8; 32])).unwrap())
}

fn repo_with_hook(
    pool: sqlx::Pool<sqlx::Postgres>,
    keys: Arc<StaticBlindIndexKeyProvider>,
    table: &str,
) -> CryptoShreddingEventRepository<InMemoryEventRepository> {
    let kek = provider();
    let key_store: Arc<dyn KeyStore> =
        Arc::new(PostgresKeyStore::new(pool.clone(), Arc::clone(&kek)));
    let hook = BlindIndexHook::new(
        keys,
        vec![Box::new(FieldSource {
            event_type: "TestPii".into(),
            subject_field: "subject_id".into(),
            source_field: "secret".into(),
            lookup_type: LOOKUP_TYPE.into(),
        })],
    )
    .with_table(table);
    CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        FieldCipher::new(),
        Arc::new(TestPiiCodec),
    )
    .with_transactional_writes(pool, kek)
    .with_persist_hook(Arc::new(hook))
}

#[allow(clippy::needless_pass_by_value)]
fn pii_event(
    aggregate_id: &str,
    seq: usize,
    subject: Uuid,
    secret: serde_json::Value,
) -> SerializedEvent {
    SerializedEvent::new(
        aggregate_id.into(),
        seq,
        "Test".into(),
        "TestPii".into(),
        "1.0".into(),
        serde_json::json!({ "TestPii": { "subject_id": subject.to_string(), "secret": secret } }),
        serde_json::json!({}),
    )
}

async fn resolve(
    pool: &sqlx::Pool<sqlx::Postgres>,
    keys: &StaticBlindIndexKeyProvider,
    table: &str,
    value: &str,
) -> Option<Uuid> {
    let digest = keys.compute(LOOKUP_TYPE, value).await.unwrap();
    sqlx::query_scalar(&format!(
        "SELECT subject_id FROM {table} WHERE lookup_type = $1 AND lookup_value = $2"
    ))
    .bind(LOOKUP_TYPE)
    .bind(digest.to_vec())
    .fetch_optional(pool)
    .await
    .unwrap()
}

// ── Tests ────────────────────────────────────────────────────────────────--

#[tokio::test]
async fn resolves_before_shred_then_fk_cascade_removes_on_delete_key_in_tx() {
    let (pool, table) = setup().await;
    let keys = bi_keys();
    let repo = repo_with_hook(pool.clone(), Arc::clone(&keys), &table);
    let agg = format!("bi-{}", Uuid::new_v4());
    let subject = Uuid::new_v4();

    repo.persist::<TestAggregate>(
        &[pii_event(
            &agg,
            1,
            subject,
            serde_json::json!("jane@corp.com"),
        )],
        None,
    )
    .await
    .unwrap();

    // Before shred: the hashed lookup resolves to the subject, written
    // transactionally with the event; the DEK exists.
    assert_eq!(
        resolve(&pool, &keys, &table, "jane@corp.com").await,
        Some(subject)
    );
    assert!(dek_exists(&pool, &subject).await);

    // Shred via delete_key_in_tx inside a caller-owned transaction — the path
    // hr/erasure.rs and route_handler.rs actually use.
    let mut tx = pool.begin().await.unwrap();
    PostgresKeyStore::new(pool.clone(), provider())
        .delete_key_in_tx(&mut tx, &subject)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // After shred: the DEK is gone (secret now unrecoverable — redaction on the
    // read path is covered by postgres_repository.rs) and the FK cascade has
    // removed the lookup row, so the pseudonym no longer resolves.
    assert!(!dek_exists(&pool, &subject).await);
    assert_eq!(resolve(&pool, &keys, &table, "jane@corp.com").await, None);
}

async fn dek_exists(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) -> bool {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .fetch_one(pool)
            .await
            .unwrap();
    n > 0
}

#[tokio::test]
async fn recapture_updates_row_no_orphan() {
    let (pool, table) = setup().await;
    let keys = bi_keys();
    let repo = repo_with_hook(pool.clone(), Arc::clone(&keys), &table);
    let agg = format!("bi-{}", Uuid::new_v4());
    let subject = Uuid::new_v4();

    repo.persist::<TestAggregate>(
        &[pii_event(
            &agg,
            1,
            subject,
            serde_json::json!("old@corp.com"),
        )],
        None,
    )
    .await
    .unwrap();
    repo.persist::<TestAggregate>(
        &[pii_event(
            &agg,
            2,
            subject,
            serde_json::json!("new@corp.com"),
        )],
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        resolve(&pool, &keys, &table, "new@corp.com").await,
        Some(subject)
    );
    assert_eq!(
        resolve(&pool, &keys, &table, "old@corp.com").await,
        None,
        "old value must not orphan"
    );

    let mut tx = pool.begin().await.unwrap();
    PostgresKeyStore::new(pool.clone(), provider())
        .delete_key_in_tx(&mut tx, &subject)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn null_value_clears_row() {
    let (pool, table) = setup().await;
    let keys = bi_keys();
    let repo = repo_with_hook(pool.clone(), Arc::clone(&keys), &table);
    let agg = format!("bi-{}", Uuid::new_v4());
    let subject = Uuid::new_v4();

    repo.persist::<TestAggregate>(
        &[pii_event(
            &agg,
            1,
            subject,
            serde_json::json!("gone@corp.com"),
        )],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        resolve(&pool, &keys, &table, "gone@corp.com").await,
        Some(subject)
    );

    // A later event clears the value (explicit null) → tombstone deletes the row.
    repo.persist::<TestAggregate>(
        &[pii_event(&agg, 2, subject, serde_json::Value::Null)],
        None,
    )
    .await
    .unwrap();
    assert_eq!(resolve(&pool, &keys, &table, "gone@corp.com").await, None);

    let mut tx = pool.begin().await.unwrap();
    PostgresKeyStore::new(pool.clone(), provider())
        .delete_key_in_tx(&mut tx, &subject)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn hook_without_transactional_writes_is_a_hard_error() {
    // No DB needed: the legacy persist path must refuse rather than silently
    // skip the hook.
    let key_store: Arc<dyn KeyStore> = Arc::new(cqrs_es_crypto::InMemoryKeyStore::default());
    let hook = BlindIndexHook::new(
        bi_keys(),
        vec![Box::new(FieldSource {
            event_type: "TestPii".into(),
            subject_field: "subject_id".into(),
            source_field: "secret".into(),
            lookup_type: LOOKUP_TYPE.into(),
        })],
    );
    let repo = CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        FieldCipher::new(),
        Arc::new(TestPiiCodec),
    )
    .with_persist_hook(Arc::new(hook)); // note: NO with_transactional_writes

    let subject = Uuid::new_v4();
    let err = repo
        .persist::<TestAggregate>(
            &[pii_event("agg", 1, subject, serde_json::json!("x@y.com"))],
            None,
        )
        .await;
    assert!(
        err.is_err(),
        "persist must fail when a hook is registered without transactional writes"
    );
}
