//! Integration tests for [`PostgresKeyStore`].
//!
//! These tests require a live `PostgreSQL` database.  They are only compiled and
//! run when the `postgres` Cargo feature is enabled (enforced via
//! `required-features` in `Cargo.toml`).

use std::sync::Arc;

use cqrs_es_crypto::{
    KekProvider, KeyStore, PostgresKeyStore, PostgresKeyStoreOptions, StaticKekProvider,
};
use uuid::Uuid;

// ── Helpers ───────────────────────────────────────────────────────────────────

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
        .expect("Failed to run database migrations");

    pool
}

fn make_provider() -> Arc<dyn KekProvider> {
    Arc::new(
        StaticKekProvider::single("test:v1", vec![0x42u8; 32])
            .expect("0x42-filled 32-byte KEK must be valid"),
    )
}

fn make_v1_provider() -> Arc<dyn KekProvider> {
    Arc::new(StaticKekProvider::single("test:v1", vec![0x42u8; 32]).unwrap())
}

fn make_v1_v2_provider() -> Arc<dyn KekProvider> {
    let mut keks = std::collections::HashMap::new();
    keks.insert(
        "test:v1".to_string(),
        zeroize::Zeroizing::new(vec![0x42u8; 32]),
    );
    keks.insert(
        "test:v2".to_string(),
        zeroize::Zeroizing::new(vec![0xDEu8; 32]),
    );
    Arc::new(StaticKekProvider::new("test:v2", keks).unwrap())
}

async fn cleanup_key(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) {
    sqlx::query("DELETE FROM subject_encryption_keys WHERE subject_id = $1")
        .bind(subject_id)
        .execute(pool)
        .await
        .unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_postgres_get_or_create_creates_key() {
    let pool = setup_test_db().await;
    let store = PostgresKeyStore::new(pool.clone(), make_provider());
    let subject_id = Uuid::new_v4();

    let material = store.get_or_create_key(&subject_id).await.unwrap();
    assert_eq!(material.key.len(), 32);
    assert_ne!(material.key_id, Uuid::nil());

    // The key should now be readable via get_key too.
    let fetched = store.get_key(&subject_id).await.unwrap().unwrap();
    assert_eq!(material.key_id, fetched.key_id);
    assert_eq!(*material.key, *fetched.key);

    cleanup_key(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_postgres_get_or_create_is_idempotent() {
    let pool = setup_test_db().await;
    let store = PostgresKeyStore::new(pool.clone(), make_provider());
    let subject_id = Uuid::new_v4();

    let first = store.get_or_create_key(&subject_id).await.unwrap();
    let second = store.get_or_create_key(&subject_id).await.unwrap();

    assert_eq!(first.key_id, second.key_id);
    assert_eq!(*first.key, *second.key);

    cleanup_key(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_postgres_get_key_returns_none_for_unknown_subject() {
    let pool = setup_test_db().await;
    let store = PostgresKeyStore::new(pool, make_provider());
    let subject_id = Uuid::new_v4();

    let result = store.get_key(&subject_id).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_postgres_delete_key_removes_it() {
    let pool = setup_test_db().await;
    let store = PostgresKeyStore::new(pool.clone(), make_provider());
    let subject_id = Uuid::new_v4();

    store.get_or_create_key(&subject_id).await.unwrap();
    store.delete_key(&subject_id).await.unwrap();

    let result = store.get_key(&subject_id).await.unwrap();
    assert!(
        result.is_none(),
        "key must be gone from Postgres after delete_key"
    );
    // No cleanup needed — delete_key already removed the row.
}

#[tokio::test]
async fn test_postgres_delete_key_is_idempotent() {
    let pool = setup_test_db().await;
    let store = PostgresKeyStore::new(pool.clone(), make_provider());
    let subject_id = Uuid::new_v4();

    // Deleting a never-created key must not error.
    store.delete_key(&subject_id).await.unwrap();

    // Creating then double-deleting must also not error.
    store.get_or_create_key(&subject_id).await.unwrap();
    store.delete_key(&subject_id).await.unwrap();
    store.delete_key(&subject_id).await.unwrap();
}

#[tokio::test]
async fn test_postgres_wrap_unwrap_survives_db_roundtrip() {
    // Verifies that the provider's wrap/unwrap is consistent with what Postgres
    // stores: the bytes written are the same bytes read back, so DEK material
    // is preserved.
    let pool = setup_test_db().await;
    let provider = make_provider();
    let store = PostgresKeyStore::new(pool.clone(), Arc::clone(&provider));
    let subject_id = Uuid::new_v4();

    let original = store.get_or_create_key(&subject_id).await.unwrap();
    let original_key_bytes: Vec<u8> = original.key.to_vec();
    let original_key_id = original.key_id;

    // Re-create a store with the same provider and pool — simulates a server restart.
    let store2 = PostgresKeyStore::new(pool.clone(), provider);
    let reloaded = store2.get_key(&subject_id).await.unwrap().unwrap();

    assert_eq!(reloaded.key_id, original_key_id);
    assert_eq!(*reloaded.key, original_key_bytes);

    cleanup_key(&pool, &subject_id).await;
}

// ── Rotation ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_postgres_list_stale_subjects() {
    use sqlx::Row as _;

    let pool = setup_test_db().await;
    let subject_id = Uuid::new_v4();

    PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    )
    .get_or_create_key(&subject_id)
    .await
    .unwrap();

    // Direct SQL check: verify kek_id was persisted correctly.
    // This is not affected by how many other rows exist.
    let stored_kek_id: String =
        sqlx::query("SELECT kek_id FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("kek_id");
    assert_eq!(stored_kek_id, "test:v1", "row must be stored under test:v1");

    // list_stale_subjects must surface this subject when 'test:v2' is primary.
    // Paginate through all pages — other concurrent tests may have created many
    // rows that sort before ours, so a single small-batch check is not reliable.
    let store_v2 = PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_v2_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    );
    let mut cursor = None;
    let mut found = false;
    loop {
        let page = store_v2
            .list_stale_subjects("test:v2", 50, cursor)
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        if page.contains(&subject_id) {
            found = true;
            break;
        }
        cursor = page.last().copied();
    }
    assert!(found, "v1 row must appear in list_stale_subjects");

    cleanup_key(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_postgres_rewrap_key_updates_kek_id() {
    let pool = setup_test_db().await;
    let subject_id = Uuid::new_v4();

    // Create under v1.
    PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    )
    .get_or_create_key(&subject_id)
    .await
    .unwrap();

    // Re-wrap to v2.
    let store_v2 = PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_v2_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    );
    let rewrapped = store_v2.rewrap_key(&subject_id).await.unwrap();
    assert!(rewrapped, "rewrap_key must return true");

    // Row must no longer appear as stale.
    let stale = store_v2
        .list_stale_subjects("test:v2", 10, None)
        .await
        .unwrap();
    assert!(
        !stale.contains(&subject_id),
        "row must be current after rewrap"
    );

    cleanup_key(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_postgres_rewrap_key_is_idempotent() {
    let pool = setup_test_db().await;
    let subject_id = Uuid::new_v4();

    let store = PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_v2_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    );
    store.get_or_create_key(&subject_id).await.unwrap();

    // Already under v2 — second call must still succeed.
    assert!(store.rewrap_key(&subject_id).await.unwrap());
    assert!(store.rewrap_key(&subject_id).await.unwrap());

    cleanup_key(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_postgres_rewrap_key_missing_subject_returns_false() {
    let pool = setup_test_db().await;
    let store = PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_v2_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    );
    let result = store.rewrap_key(&Uuid::new_v4()).await.unwrap();
    assert!(!result, "rewrap_key on a missing subject must return false");
}

#[tokio::test]
async fn test_postgres_rewrap_preserves_dek_material() {
    // End-to-end: DEK material must be identical before and after a rewrap.
    let pool = setup_test_db().await;
    let subject_id = Uuid::new_v4();

    // Create under v1, capture the raw key bytes.
    let store_v1 = PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    );
    let original = store_v1.get_or_create_key(&subject_id).await.unwrap();
    let original_bytes = original.key.to_vec();

    // Rewrap to v2.
    let store_v2 = PostgresKeyStore::new_with_options(
        pool.clone(),
        make_v1_v2_provider(),
        &PostgresKeyStoreOptions { lazy_rewrap: false },
    );
    store_v2.rewrap_key(&subject_id).await.unwrap();

    // Reading back via the v2 store must return the same key bytes.
    let reloaded = store_v2.get_key(&subject_id).await.unwrap().unwrap();
    assert_eq!(
        *reloaded.key, original_bytes,
        "DEK bytes must survive a KEK re-wrap"
    );

    cleanup_key(&pool, &subject_id).await;
}
