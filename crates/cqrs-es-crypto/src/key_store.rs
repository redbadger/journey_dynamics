//! Per-subject Data Encryption Key (DEK) management for GDPR crypto-shredding.
//!
//! # Design
//!
//! Each data subject gets a unique 256-bit AES DEK.  The DEK is stored wrapped (encrypted)
//! by a Key Encryption Key (KEK) in the `subject_encryption_keys` table.  Shredding a
//! subject is a single hard-delete of that row: without the DEK, all ciphertext in the
//! event store is permanently unreadable.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::cipher::{CryptoError, KeyMaterial, PiiCipher};

// ─────────────────────────────────────────────────────────────────────────────
// Error
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum KeyStoreError {
    #[error("Crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Key store lock poisoned — another thread panicked while holding the lock")]
    LockPoisoned,
}

impl<T> From<PoisonError<T>> for KeyStoreError {
    fn from(_: PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Trait
// ─────────────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait KeyStore: Send + Sync {
    /// Return the DEK for `subject_id`, creating and persisting a fresh one if none exists.
    ///
    /// This operation is idempotent: calling it multiple times for the same subject always
    /// returns key material with the same key bytes.
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError>;

    /// Return the DEK for `subject_id`, or `None` if it has been deleted (shredded).
    async fn get_key(&self, subject_id: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError>;

    /// Hard-delete the DEK for `subject_id`.  This is the shredding operation.
    ///
    /// After this call all ciphertext encrypted with the deleted key is permanently
    /// unreadable.  Calling `delete_key` on a subject that has no key is a no-op (idempotent).
    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// InMemoryKeyStore
// ─────────────────────────────────────────────────────────────────────────────

/// Map from `subject_id` to `(key_id, raw_key_bytes)`.
type KeyMap = HashMap<Uuid, (Uuid, Zeroizing<Vec<u8>>)>;

/// In-memory [`KeyStore`] backed by a `HashMap`.
///
/// Intended for use in unit tests.  All operations are infallible; the `Result` wrapper
/// exists solely to satisfy the trait contract.
pub struct InMemoryKeyStore {
    /// `subject_id` → (`key_id`, `raw_key_bytes`)
    store: Mutex<KeyMap>,
}

impl InMemoryKeyStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl KeyStore for InMemoryKeyStore {
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError> {
        let dek = {
            let mut store = self.store.lock()?;

            if let Some((key_id, key)) = store.get(subject_id) {
                // Key already exists — return a copy.
                return Ok(KeyMaterial {
                    key_id: *key_id,
                    key: Zeroizing::new(key.to_vec()),
                });
            }

            // Generate a fresh DEK, stash a copy, and return the original.
            let dek = PiiCipher::generate_dek();
            store.insert(*subject_id, (dek.key_id, Zeroizing::new(dek.key.to_vec())));
            dek
        };

        Ok(dek)
    }

    async fn get_key(&self, subject_id: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError> {
        let store = self.store.lock()?;
        Ok(store.get(subject_id).map(|(key_id, key)| KeyMaterial {
            key_id: *key_id,
            key: Zeroizing::new(key.to_vec()),
        }))
    }

    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError> {
        self.store.lock()?.remove(subject_id);

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PostgresKeyStore
// ─────────────────────────────────────────────────────────────────────────────

/// [`KeyStore`] backed by the `subject_encryption_keys` Postgres table.
///
/// DEKs are stored wrapped (AES-256-KWP) with the KEK held by `cipher`.  The raw key
/// bytes never leave this struct in plaintext — they are only held in memory for the
/// duration of a single request.
pub struct PostgresKeyStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    cipher: PiiCipher,
}

impl PostgresKeyStore {
    #[must_use]
    pub const fn new(pool: sqlx::Pool<sqlx::Postgres>, cipher: PiiCipher) -> Self {
        Self { pool, cipher }
    }
}

#[async_trait]
impl KeyStore for PostgresKeyStore {
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError> {
        // Fast path: key already exists.
        if let Some(material) = self.get_key(subject_id).await? {
            return Ok(material);
        }

        // Generate a fresh DEK and wrap it with the KEK before storing.
        let dek = PiiCipher::generate_dek();
        let wrapped_key = self.cipher.wrap_dek(&dek);
        let key_id = dek.key_id;

        // INSERT … ON CONFLICT DO NOTHING handles the concurrent-creation race: if two
        // callers reach this point simultaneously, only one INSERT succeeds.
        let result = sqlx::query(
            "INSERT INTO subject_encryption_keys (key_id, subject_id, wrapped_key) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (subject_id) DO NOTHING",
        )
        .bind(key_id)
        .bind(subject_id)
        .bind(&wrapped_key)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            // The concurrent caller won the race — load what they stored.
            self.get_key(subject_id)
                .await?
                .ok_or_else(|| KeyStoreError::Database(sqlx::Error::RowNotFound))
        } else {
            Ok(dek)
        }
    }

    async fn get_key(&self, subject_id: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError> {
        use sqlx::Row;

        let row = sqlx::query(
            "SELECT key_id, wrapped_key \
             FROM subject_encryption_keys \
             WHERE subject_id = $1",
        )
        .bind(subject_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let key_id: Uuid = row.get("key_id");
        let wrapped_key: Vec<u8> = row.get("wrapped_key");
        let material = self.cipher.unwrap_dek(key_id, &wrapped_key)?;
        Ok(Some(material))
    }

    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError> {
        sqlx::query("DELETE FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

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

    fn test_cipher() -> PiiCipher {
        PiiCipher::new(vec![0x42u8; 32]).expect("0x42-filled 32-byte KEK must be valid")
    }

    // ── Unit tests: InMemoryKeyStore ──────────────────────────────────────────

    #[tokio::test]
    async fn test_get_key_returns_none_for_unknown_subject() {
        let store = InMemoryKeyStore::new();
        let subject_id = Uuid::new_v4();

        let result = store.get_key(&subject_id).await.unwrap();
        assert!(
            result.is_none(),
            "a fresh store should have no key for an unseen subject"
        );
    }

    #[tokio::test]
    async fn test_get_or_create_creates_key_for_new_subject() {
        let store = InMemoryKeyStore::new();
        let subject_id = Uuid::new_v4();

        let material = store.get_or_create_key(&subject_id).await.unwrap();
        assert_eq!(material.key.len(), 32, "DEK must be 32 bytes");
        assert_ne!(
            material.key_id,
            Uuid::nil(),
            "key_id must be a non-nil UUID"
        );
    }

    #[tokio::test]
    async fn test_get_or_create_returns_same_key_on_second_call() {
        let store = InMemoryKeyStore::new();
        let subject_id = Uuid::new_v4();

        let first = store.get_or_create_key(&subject_id).await.unwrap();
        let second = store.get_or_create_key(&subject_id).await.unwrap();

        assert_eq!(
            first.key_id, second.key_id,
            "repeated calls must return the same key_id"
        );
        assert_eq!(
            *first.key, *second.key,
            "repeated calls must return identical key bytes"
        );
    }

    #[tokio::test]
    async fn test_get_key_returns_some_after_create() {
        let store = InMemoryKeyStore::new();
        let subject_id = Uuid::new_v4();

        let created = store.get_or_create_key(&subject_id).await.unwrap();
        let fetched = store
            .get_key(&subject_id)
            .await
            .unwrap()
            .expect("get_key must return Some after get_or_create_key");

        assert_eq!(created.key_id, fetched.key_id);
        assert_eq!(*created.key, *fetched.key);
    }

    #[tokio::test]
    async fn test_delete_key_makes_get_key_return_none() {
        let store = InMemoryKeyStore::new();
        let subject_id = Uuid::new_v4();

        store.get_or_create_key(&subject_id).await.unwrap();
        store.delete_key(&subject_id).await.unwrap();

        let result = store.get_key(&subject_id).await.unwrap();
        assert!(
            result.is_none(),
            "get_key must return None after delete_key"
        );
    }

    #[tokio::test]
    async fn test_delete_key_is_idempotent() {
        let store = InMemoryKeyStore::new();
        let subject_id = Uuid::new_v4();

        // Deleting a key that was never created should not error.
        store.delete_key(&subject_id).await.unwrap();

        // Creating, then deleting twice, should also not error.
        store.get_or_create_key(&subject_id).await.unwrap();
        store.delete_key(&subject_id).await.unwrap();
        store.delete_key(&subject_id).await.unwrap();
    }

    #[tokio::test]
    async fn test_different_subjects_get_different_keys() {
        let store = InMemoryKeyStore::new();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        let key_a = store.get_or_create_key(&subject_a).await.unwrap();
        let key_b = store.get_or_create_key(&subject_b).await.unwrap();

        assert_ne!(
            *key_a.key, *key_b.key,
            "distinct subjects must receive distinct DEKs"
        );
        assert_ne!(
            key_a.key_id, key_b.key_id,
            "distinct subjects must receive distinct key_ids"
        );
    }

    #[tokio::test]
    async fn test_delete_only_affects_target_subject() {
        let store = InMemoryKeyStore::new();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        store.get_or_create_key(&subject_a).await.unwrap();
        store.get_or_create_key(&subject_b).await.unwrap();

        // Shred only subject_a.
        store.delete_key(&subject_a).await.unwrap();

        assert!(
            store.get_key(&subject_a).await.unwrap().is_none(),
            "subject_a's key must be gone after deletion"
        );
        assert!(
            store.get_key(&subject_b).await.unwrap().is_some(),
            "subject_b's key must be unaffected by subject_a's deletion"
        );
    }

    // ── Integration tests: PostgresKeyStore ───────────────────────────────────

    async fn cleanup_key(pool: &sqlx::Pool<sqlx::Postgres>, subject_id: &Uuid) {
        sqlx::query("DELETE FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_postgres_get_or_create_creates_key() {
        let pool = setup_test_db().await;
        let store = PostgresKeyStore::new(pool.clone(), test_cipher());
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
        let store = PostgresKeyStore::new(pool.clone(), test_cipher());
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
        let store = PostgresKeyStore::new(pool, test_cipher());
        let subject_id = Uuid::new_v4();

        let result = store.get_key(&subject_id).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_postgres_delete_key_removes_it() {
        let pool = setup_test_db().await;
        let store = PostgresKeyStore::new(pool.clone(), test_cipher());
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
        let store = PostgresKeyStore::new(pool.clone(), test_cipher());
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
        // Verifies that the cipher's wrap/unwrap is consistent with what Postgres stores:
        // the bytes written are the same bytes read back, so DEK material is preserved.
        let pool = setup_test_db().await;
        let cipher = test_cipher();
        let store = PostgresKeyStore::new(pool.clone(), test_cipher());
        let subject_id = Uuid::new_v4();

        let original = store.get_or_create_key(&subject_id).await.unwrap();
        let original_key_bytes: Vec<u8> = original.key.to_vec();
        let original_key_id = original.key_id;

        // Re-create a store with the same cipher and pool — simulates a server restart.
        let store2 = PostgresKeyStore::new(pool.clone(), cipher);
        let reloaded = store2.get_key(&subject_id).await.unwrap().unwrap();

        assert_eq!(reloaded.key_id, original_key_id);
        assert_eq!(*reloaded.key, original_key_bytes);

        cleanup_key(&pool, &subject_id).await;
    }
}
