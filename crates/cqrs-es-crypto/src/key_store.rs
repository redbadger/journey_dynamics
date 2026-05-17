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
    sync::{Arc, Mutex, PoisonError},
};

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::cipher::{FieldCipher, KeyMaterial};
#[cfg(feature = "postgres")]
use crate::kek::WrappedDek;
use crate::kek::{KekError, KekProvider};

// ─────────────────────────────────────────────────────────────────────────────
// Error
// ─────────────────────────────────────────────────────────────────────────────

/// Errors returned by [`KeyStore`] operations.
#[derive(Debug, Error)]
pub enum KeyStoreError {
    /// A KEK wrap or unwrap operation failed (wrong version, corrupt data, vault error).
    #[error("KEK error: {0}")]
    Kek(#[from] KekError),
    #[cfg(feature = "postgres")]
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

/// Manages per-subject Data Encryption Keys (DEKs) for GDPR crypto-shredding.
///
/// Each data subject gets a unique DEK. Deleting a subject's DEK is the shredding
/// operation: any PII encrypted under that DEK becomes permanently irrecoverable
/// without touching individual events.
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

    /// Returns up to `batch_size` subject IDs whose DEK is not wrapped under
    /// `current_kek_id`.
    ///
    /// Results are ordered by subject ID and start after `after` (exclusive cursor
    /// for pagination). An empty result indicates the re-wrap sweep is complete.
    ///
    /// # Errors
    ///
    /// Propagates storage errors as [`KeyStoreError`].
    async fn list_stale_subjects(
        &self,
        current_kek_id: &str,
        batch_size: usize,
        after: Option<Uuid>,
    ) -> Result<Vec<Uuid>, KeyStoreError>;

    /// Re-wraps the DEK for `subject_id` under the current primary KEK version.
    ///
    /// Returns `true` if the key was actually re-wrapped, `false` if the subject
    /// was not found (already shredded) or the key was already current.
    ///
    /// # Errors
    ///
    /// Propagates KEK or storage errors as [`KeyStoreError`].
    async fn rewrap_key(&self, subject_id: &Uuid) -> Result<bool, KeyStoreError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// InMemoryKeyStore
// ─────────────────────────────────────────────────────────────────────────────

/// Internal per-subject record held by [`InMemoryKeyStore`].
struct KeyEntry {
    key_id: Uuid,
    key: Zeroizing<Vec<u8>>,
    /// The KEK version id that would have been used to wrap this key at rest.
    /// For stores created with [`InMemoryKeyStore::new`] this is the sentinel
    /// `"memory:v0"` since no real wrapping takes place.
    kek_id: String,
}

/// Map from `subject_id` to [`KeyEntry`].
type KeyMap = HashMap<Uuid, KeyEntry>;

/// In-memory [`KeyStore`] backed by a `HashMap`.
///
/// Intended for use in unit tests.  All operations are infallible; the `Result` wrapper
/// exists solely to satisfy the trait contract.
///
/// Create with [`Self::new`] for basic usage, or [`Self::new_with_provider`] when
/// KEK rotation operations ([`KeyStore::list_stale_subjects`] /
/// [`KeyStore::rewrap_key`]) are required.
pub struct InMemoryKeyStore {
    /// `subject_id` → key entry.
    store: Mutex<KeyMap>,
    /// KEK provider used to determine the current version for re-wrap support.
    /// `None` when the store is created with [`Self::new`].
    provider: Option<Arc<dyn KekProvider>>,
}

impl InMemoryKeyStore {
    /// Creates a new, empty [`InMemoryKeyStore`] without a KEK provider.
    ///
    /// [`KeyStore::rewrap_key`] will always return `Ok(false)` on a store created
    /// this way.  Use [`Self::new_with_provider`] when rotation support is needed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            provider: None,
        }
    }

    /// Creates a new, empty [`InMemoryKeyStore`] with a [`KekProvider`].
    ///
    /// Keys vended by [`KeyStore::get_or_create_key`] are tagged with the
    /// provider's current KEK id, enabling [`KeyStore::list_stale_subjects`] and
    /// [`KeyStore::rewrap_key`] to work correctly.
    #[must_use]
    pub fn new_with_provider(provider: Arc<dyn KekProvider>) -> Self {
        Self {
            store: Mutex::new(HashMap::new()),
            provider: Some(provider),
        }
    }

    /// Inserts a subject entry directly with the specified `kek_id`, bypassing the
    /// normal key-creation flow.
    ///
    /// This is intended for unit tests that need to pre-populate "stale" entries
    /// (i.e. entries wrapped under an old KEK version) before exercising the
    /// re-wrap worker.
    ///
    /// Only available in test builds and when the `testing` feature is enabled.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned, which can only happen if another
    /// thread panicked while holding the lock — effectively impossible in test code.
    #[cfg(any(test, feature = "testing"))]
    pub fn insert_for_testing(&self, subject_id: Uuid, kek_id: &str) {
        let dek = FieldCipher::generate_dek();
        self.store
            .lock()
            .expect("lock is not poisoned in insert_for_testing")
            .insert(
                subject_id,
                KeyEntry {
                    key_id: dek.key_id,
                    key: dek.key,
                    kek_id: kek_id.to_string(),
                },
            );
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

            if let Some(entry) = store.get(subject_id) {
                // Key already exists — return a copy.
                return Ok(KeyMaterial {
                    key_id: entry.key_id,
                    key: Zeroizing::new(entry.key.to_vec()),
                });
            }

            // Generate a fresh DEK, stash a copy tagged with the current KEK id,
            // and return the original.
            let dek = FieldCipher::generate_dek();
            let kek_id = self
                .provider
                .as_ref()
                .map_or_else(|| "memory:v0".to_string(), |p| p.current().id);
            store.insert(
                *subject_id,
                KeyEntry {
                    key_id: dek.key_id,
                    key: Zeroizing::new(dek.key.to_vec()),
                    kek_id,
                },
            );
            dek
        };

        Ok(dek)
    }

    async fn get_key(&self, subject_id: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError> {
        let store = self.store.lock()?;
        Ok(store.get(subject_id).map(|e| KeyMaterial {
            key_id: e.key_id,
            key: Zeroizing::new(e.key.to_vec()),
        }))
    }

    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError> {
        self.store.lock()?.remove(subject_id);
        Ok(())
    }

    async fn list_stale_subjects(
        &self,
        current_kek_id: &str,
        batch_size: usize,
        after: Option<Uuid>,
    ) -> Result<Vec<Uuid>, KeyStoreError> {
        // Collect while the lock is held, then release before sorting/truncating.
        let mut subjects: Vec<Uuid> = {
            let store = self.store.lock()?;
            store
                .iter()
                .filter(|(_, e)| e.kek_id != current_kek_id)
                .map(|(id, _)| *id)
                .filter(|id| after.is_none_or(|cursor| *id > cursor))
                .collect()
        };
        subjects.sort_unstable();
        subjects.truncate(batch_size);
        Ok(subjects)
    }

    async fn rewrap_key(&self, subject_id: &Uuid) -> Result<bool, KeyStoreError> {
        let Some(ref provider) = self.provider else {
            return Ok(false);
        };
        let current_id = provider.current().id;
        // Scope the lock to the mutation only; drop it before returning.
        let result = {
            let mut store = self.store.lock()?;
            match store.get_mut(subject_id) {
                None => false, // subject has been shredded
                Some(entry) => {
                    entry.kek_id = current_id; // no-op if already current
                    true
                }
            }
        };
        Ok(result)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PostgresKeyStore
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for [`PostgresKeyStore`].
#[cfg(feature = "postgres")]
#[derive(Debug, Clone, Copy)]
pub struct PostgresKeyStoreOptions {
    /// Spawn a background task to re-wrap stale DEKs on every read.
    ///
    /// Default: `true`.  Set to `false` in tests that need deterministic,
    /// single-threaded key-store state.
    pub lazy_rewrap: bool,
}

#[cfg(feature = "postgres")]
impl Default for PostgresKeyStoreOptions {
    fn default() -> Self {
        Self { lazy_rewrap: true }
    }
}

/// [`KeyStore`] backed by the `subject_encryption_keys` Postgres table.
///
/// DEKs are stored wrapped by the supplied [`KekProvider`].  The raw DEK bytes never
/// leave this struct in plaintext — they are held in memory only for the duration of a
/// single request.  The `kek_id` of the wrapping KEK version is persisted alongside the
/// wrapped bytes so that multiple KEK versions can coexist during a rotation.
#[cfg(feature = "postgres")]
#[derive(Clone)]
pub struct PostgresKeyStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    provider: Arc<dyn KekProvider>,
    lazy_rewrap: bool,
}

#[cfg(feature = "postgres")]
impl PostgresKeyStore {
    /// Create a new [`PostgresKeyStore`] with default options (lazy re-wrap enabled).
    #[must_use]
    pub fn new(pool: sqlx::Pool<sqlx::Postgres>, provider: Arc<dyn KekProvider>) -> Self {
        Self::new_with_options(pool, provider, &PostgresKeyStoreOptions::default())
    }

    /// Create a new [`PostgresKeyStore`] with explicit options.
    #[must_use]
    pub fn new_with_options(
        pool: sqlx::Pool<sqlx::Postgres>,
        provider: Arc<dyn KekProvider>,
        options: &PostgresKeyStoreOptions,
    ) -> Self {
        Self {
            pool,
            provider,
            lazy_rewrap: options.lazy_rewrap,
        }
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl KeyStore for PostgresKeyStore {
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError> {
        // Fast path: key already exists.
        if let Some(material) = self.get_key(subject_id).await? {
            return Ok(material);
        }

        // Generate a fresh DEK and wrap it under the current primary KEK version.
        let dek = FieldCipher::generate_dek();
        let kek = self.provider.current();
        let wrapped = self.provider.wrap(&kek, &dek).await?;
        let key_id = dek.key_id;

        // INSERT … ON CONFLICT DO NOTHING handles the concurrent-creation race: if two
        // callers reach this point simultaneously, only one INSERT succeeds.
        let result = sqlx::query(
            "INSERT INTO subject_encryption_keys (key_id, subject_id, wrapped_key, kek_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (subject_id) DO NOTHING",
        )
        .bind(key_id)
        .bind(subject_id)
        .bind(&wrapped.wrapped_key)
        .bind(&wrapped.kek_id)
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
            "SELECT key_id, wrapped_key, kek_id \
             FROM subject_encryption_keys \
             WHERE subject_id = $1",
        )
        .bind(subject_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        // Fire-and-forget lazy re-wrap when this row's KEK version is stale.
        if self.lazy_rewrap && row.get::<String, _>("kek_id") != self.provider.current().id {
            let me = self.clone();
            let sid = *subject_id;
            tokio::spawn(async move {
                if let Err(e) = me.rewrap_key(&sid).await {
                    tracing::warn!(?sid, error = ?e, "kek.rotation.lazy_rewrap failed");
                } else {
                    tracing::debug!(?sid, "kek.rotation.lazy_rewrap succeeded");
                }
            });
        }

        let material = self
            .provider
            .unwrap(&WrappedDek {
                key_id: row.get("key_id"),
                kek_id: row.get("kek_id"),
                wrapped_key: row.get("wrapped_key"),
            })
            .await?;

        Ok(Some(material))
    }

    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError> {
        sqlx::query("DELETE FROM subject_encryption_keys WHERE subject_id = $1")
            .bind(subject_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn list_stale_subjects(
        &self,
        current_kek_id: &str,
        batch_size: usize,
        after: Option<Uuid>,
    ) -> Result<Vec<Uuid>, KeyStoreError> {
        use sqlx::Row;

        let limit = i64::try_from(batch_size).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT subject_id FROM subject_encryption_keys \
             WHERE kek_id != $1 AND ($2::uuid IS NULL OR subject_id > $2) \
             ORDER BY subject_id LIMIT $3",
        )
        .bind(current_kek_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.iter().map(|r| r.get("subject_id")).collect())
    }

    async fn rewrap_key(&self, subject_id: &Uuid) -> Result<bool, KeyStoreError> {
        use sqlx::Row;

        let Some(row) = sqlx::query(
            "SELECT key_id, wrapped_key, kek_id \
             FROM subject_encryption_keys WHERE subject_id = $1",
        )
        .bind(subject_id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(false); // already shredded
        };

        let current_kek = self.provider.current();
        let existing_kek_id: String = row.get("kek_id");

        if existing_kek_id == current_kek.id {
            // Already wrapped under the current KEK — touch rewrapped_at for observability.
            sqlx::query(
                "UPDATE subject_encryption_keys \
                 SET rewrapped_at = NOW() WHERE subject_id = $1",
            )
            .bind(subject_id)
            .execute(&self.pool)
            .await?;
            return Ok(true);
        }

        let material = self
            .provider
            .unwrap(&WrappedDek {
                key_id: row.get("key_id"),
                kek_id: existing_kek_id.clone(),
                wrapped_key: row.get("wrapped_key"),
            })
            .await?;

        let rewrapped = self.provider.wrap(&current_kek, &material).await?;

        // CAS UPDATE: only applies if no concurrent re-wrap has already changed kek_id.
        let result = sqlx::query(
            "UPDATE subject_encryption_keys \
             SET wrapped_key = $1, kek_id = $2, rewrapped_at = NOW() \
             WHERE subject_id = $3 AND kek_id = $4",
        )
        .bind(&rewrapped.wrapped_key)
        .bind(&rewrapped.kek_id)
        .bind(subject_id)
        .bind(&existing_kek_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            // A concurrent re-wrap beat us, or the row was shredded mid-flight.
            // Check whether the row still exists to return the correct value.
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM subject_encryption_keys WHERE subject_id = $1)",
            )
            .bind(subject_id)
            .fetch_one(&self.pool)
            .await?;
            return Ok(exists);
        }

        Ok(true)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::kek::StaticKekProvider;

    use super::*;

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

    // ── Rotation unit tests: InMemoryKeyStore ────────────────────────────────

    fn v1_provider() -> Arc<dyn KekProvider> {
        Arc::new(StaticKekProvider::single("v1", vec![0x42u8; 32]).unwrap())
    }

    fn v1_v2_provider() -> Arc<dyn KekProvider> {
        let mut keks = std::collections::HashMap::new();
        keks.insert("v1".to_string(), Zeroizing::new(vec![0x42u8; 32]));
        keks.insert("v2".to_string(), Zeroizing::new(vec![0xDEu8; 32]));
        Arc::new(StaticKekProvider::new("v2", keks).unwrap())
    }

    #[tokio::test]
    async fn test_list_stale_subjects_empty_when_all_current() {
        let provider = v1_provider();
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        let subject_id = Uuid::new_v4();
        store.get_or_create_key(&subject_id).await.unwrap();

        let stale = store.list_stale_subjects("v1", 10, None).await.unwrap();
        assert!(
            stale.is_empty(),
            "no stale entries when all use the current KEK"
        );
    }

    #[tokio::test]
    async fn test_list_stale_subjects_finds_old_kek_entries() {
        let provider = v1_v2_provider(); // primary = v2
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        let subject_id = Uuid::new_v4();
        store.insert_for_testing(subject_id, "v1"); // stale entry

        let stale = store.list_stale_subjects("v2", 10, None).await.unwrap();
        assert!(stale.contains(&subject_id), "v1 entry must appear as stale");
    }

    #[tokio::test]
    async fn test_list_stale_subjects_respects_after_cursor() {
        let provider = v1_v2_provider();
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        // Insert several stale entries.
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        for &id in &ids {
            store.insert_for_testing(id, "v1");
        }
        // Sort to mimic what list_stale_subjects returns.
        let mut sorted = ids.clone();
        sorted.sort_unstable();

        // First page.
        let page1 = store.list_stale_subjects("v2", 3, None).await.unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page1, sorted[..3]);

        // Second page — cursor is the last UUID from page1.
        let cursor = *page1.last().unwrap();
        let page2 = store
            .list_stale_subjects("v2", 3, Some(cursor))
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2, sorted[3..]);
    }

    #[tokio::test]
    async fn test_list_stale_subjects_respects_batch_size() {
        let provider = v1_v2_provider();
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        for _ in 0..10 {
            store.insert_for_testing(Uuid::new_v4(), "v1");
        }
        let page = store.list_stale_subjects("v2", 4, None).await.unwrap();
        assert_eq!(page.len(), 4, "batch_size must limit results");
    }

    #[tokio::test]
    async fn test_rewrap_key_updates_kek_id() {
        let provider = v1_v2_provider(); // primary = v2
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        let subject_id = Uuid::new_v4();
        store.insert_for_testing(subject_id, "v1");

        let result = store.rewrap_key(&subject_id).await.unwrap();
        assert!(result, "rewrap_key must return true");

        // No longer stale.
        let stale = store.list_stale_subjects("v2", 10, None).await.unwrap();
        assert!(
            !stale.contains(&subject_id),
            "entry must be current after rewrap"
        );
    }

    #[tokio::test]
    async fn test_rewrap_key_is_idempotent() {
        let provider = v1_v2_provider(); // primary = v2
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        let subject_id = Uuid::new_v4();
        store.get_or_create_key(&subject_id).await.unwrap(); // already under v2

        // Both calls must succeed even though the first is already current.
        assert!(store.rewrap_key(&subject_id).await.unwrap());
        assert!(store.rewrap_key(&subject_id).await.unwrap());
    }

    #[tokio::test]
    async fn test_rewrap_key_returns_false_for_missing_subject() {
        let provider = v1_v2_provider();
        let store = InMemoryKeyStore::new_with_provider(Arc::clone(&provider));
        let result = store.rewrap_key(&Uuid::new_v4()).await.unwrap();
        assert!(!result, "rewrap_key on a missing subject must return false");
    }
}
