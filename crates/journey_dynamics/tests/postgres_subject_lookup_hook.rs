//! Integration tests for [`SubjectLookupHook`].
//!
//! These tests connect to a real PostgreSQL database and are intentionally
//! kept outside the library crate so that `cargo nextest run --lib` skips them
//! when no database is available.  Run them with:
//!
//! ```text
//! cargo nextest run --test postgres_subject_lookup_hook
//! ```
//!
//! The `DATABASE_URL` environment variable must point at a reachable Postgres
//! instance (defaults to `postgres://postgres:postgres@localhost:5432/journey_dynamics`).

use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::PersistHook;
use journey_dynamics::subject_lookup_hook::SubjectLookupHook;
use serde_json::{Value, json};
use uuid::Uuid;

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn setup_test_db() -> sqlx::Pool<sqlx::Postgres> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:postgres@localhost:5432/journey_dynamics".to_string()
    });
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("Failed to connect to database");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");
    pool
}

fn person_captured(subject_id: Uuid, email: &str) -> SerializedEvent {
    SerializedEvent::new(
        Uuid::new_v4().to_string(),
        1,
        "Journey".to_string(),
        "PersonCaptured".to_string(),
        "1.0".to_string(),
        json!({
            "PersonCaptured": {
                "person_ref": "passenger_0",
                "subject_id": subject_id.to_string(),
                "name": "Test User",
                "email": email,
                "phone": Value::Null
            }
        }),
        json!({}),
    )
}

fn non_pii_event() -> SerializedEvent {
    SerializedEvent::new(
        Uuid::new_v4().to_string(),
        1,
        "Journey".to_string(),
        "JourneyModified".to_string(),
        "1.0".to_string(),
        json!({ "Modified": { "step": "search", "data": {} } }),
        json!({}),
    )
}

async fn row_count(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM subject_lookup WHERE subject_id = $1")
        .bind(subject_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn email_for(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT email_lower FROM subject_lookup WHERE subject_id = $1")
        .bind(subject_id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn cleanup(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) {
    let _ = sqlx::query("DELETE FROM subject_lookup WHERE subject_id = $1")
        .bind(subject_id)
        .execute(pool)
        .await;
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_person_captured_inserts_email_lowercase() {
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;
    let subject_id = Uuid::new_v4();

    let mut tx = pool.begin().await.unwrap();
    hook.on_persist(&[person_captured(subject_id, "Alice@Example.COM")], &mut tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(row_count(&pool, &subject_id).await, 1);
    assert_eq!(
        email_for(&pool, &subject_id).await.as_deref(),
        Some("alice@example.com")
    );

    cleanup(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_non_person_captured_event_produces_no_row() {
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;
    let subject_id = Uuid::new_v4();

    let mut tx = pool.begin().await.unwrap();
    hook.on_persist(&[non_pii_event()], &mut tx).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(row_count(&pool, &subject_id).await, 0);
}

#[tokio::test]
async fn test_second_person_captured_updates_email() {
    // Upsert semantics: a second PersonCaptured for the same subject
    // (e.g. an email change) overwrites email_lower rather than inserting
    // a duplicate row.
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;
    let subject_id = Uuid::new_v4();

    let mut tx = pool.begin().await.unwrap();
    hook.on_persist(
        &[
            person_captured(subject_id, "first@example.com"),
            person_captured(subject_id, "second@example.com"),
        ],
        &mut tx,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(
        row_count(&pool, &subject_id).await,
        1,
        "must have exactly one row"
    );
    assert_eq!(
        email_for(&pool, &subject_id).await.as_deref(),
        Some("second@example.com"),
        "email must reflect the second PersonCaptured"
    );

    cleanup(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_missing_subject_id_is_silently_skipped() {
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;

    let malformed = SerializedEvent::new(
        Uuid::new_v4().to_string(),
        1,
        "Journey".to_string(),
        "PersonCaptured".to_string(),
        "1.0".to_string(),
        json!({ "PersonCaptured": { "person_ref": "p0", "email": "x@x.com" } }),
        json!({}),
    );

    let mut tx = pool.begin().await.unwrap();
    let result = hook.on_persist(&[malformed], &mut tx).await;
    tx.rollback().await.unwrap();

    assert!(result.is_ok(), "missing subject_id must not error");
}

#[tokio::test]
async fn test_unparseable_uuid_is_silently_skipped() {
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;

    let malformed = SerializedEvent::new(
        Uuid::new_v4().to_string(),
        1,
        "Journey".to_string(),
        "PersonCaptured".to_string(),
        "1.0".to_string(),
        json!({
            "PersonCaptured": {
                "person_ref": "p0",
                "subject_id": "not-a-uuid",
                "email": "x@x.com"
            }
        }),
        json!({}),
    );

    let mut tx = pool.begin().await.unwrap();
    let result = hook.on_persist(&[malformed], &mut tx).await;
    tx.rollback().await.unwrap();

    assert!(result.is_ok(), "bad uuid must not error");
}

#[tokio::test]
async fn test_missing_email_is_silently_skipped() {
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;
    let subject_id = Uuid::new_v4();

    let malformed = SerializedEvent::new(
        Uuid::new_v4().to_string(),
        1,
        "Journey".to_string(),
        "PersonCaptured".to_string(),
        "1.0".to_string(),
        json!({
            "PersonCaptured": {
                "person_ref": "p0",
                "subject_id": subject_id.to_string()
            }
        }),
        json!({}),
    );

    let mut tx = pool.begin().await.unwrap();
    let result = hook.on_persist(&[malformed], &mut tx).await;
    tx.commit().await.unwrap();

    assert!(result.is_ok(), "missing email must not error");
    assert_eq!(row_count(&pool, &subject_id).await, 0);
}

#[tokio::test]
async fn test_mixed_batch_only_writes_person_captured_rows() {
    // A batch containing PersonCaptured events mixed with other event types
    // must produce exactly one row per PersonCaptured subject.
    let pool = setup_test_db().await;
    let hook = SubjectLookupHook;
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();

    let events = vec![
        person_captured(subject_a, "a@example.com"),
        non_pii_event(),
        person_captured(subject_b, "b@example.com"),
        non_pii_event(),
    ];

    let mut tx = pool.begin().await.unwrap();
    hook.on_persist(&events, &mut tx).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(row_count(&pool, &subject_a).await, 1);
    assert_eq!(row_count(&pool, &subject_b).await, 1);

    cleanup(&pool, &subject_a).await;
    cleanup(&pool, &subject_b).await;
}
