//! [`CachingKeyStore`] — an in-memory DEK cache in front of any [`KeyStore`].
//!
//! Network-backed KEK providers (e.g. cloud KMS) turn every `get_key` into a
//! remote `decrypt`. This decorator caches the *plaintext* DEK per subject so the
//! hot read path avoids the round-trip.
//!
//! # Crypto-shredding correctness
//!
//! Erasure deletes a subject's wrapped DEK; a cached plaintext DEK must not
//! outlive that. This decorator evicts on [`KeyStore::delete_key`] and
//! [`KeyStore::delete_key_in_tx`]. **However**, `delete_key_in_tx` runs inside a
//! caller-owned transaction: between the in-tx delete and the caller's `commit`,
//! a concurrent `get_key` on another connection can still see the (uncommitted)
//! row and re-populate the cache. So the in-tx eviction here is best-effort — the
//! caller that owns the transaction must also evict **after commit** via the
//! shared [`DekCache`] (see [`CachingKeyStore::cache`]). The plain `delete_key`
//! path owns its own transaction, so its eviction is authoritative.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    cipher::KeyMaterial,
    key_store::{KeyStore, KeyStoreError},
};

/// Shareable, bounded, TTL'd cache of plaintext DEKs keyed by `subject_id`.
///
/// Clone the `Arc` to hand a handle to the erasure path so it can evict after
/// committing a shred (see the module docs).
pub struct DekCache {
    inner: Mutex<Inner>,
    capacity: usize,
    ttl: Duration,
}

struct Inner {
    map: HashMap<Uuid, Entry>,
    /// Insertion order for capacity eviction (FIFO). May contain ids no longer in
    /// `map`; stale ids are skipped when evicting.
    order: VecDeque<Uuid>,
}

struct Entry {
    key_id: Uuid,
    key: Zeroizing<Vec<u8>>,
    inserted: Instant,
}

impl DekCache {
    /// `capacity` bounds the number of cached subjects; entries older than `ttl`
    /// are treated as misses and dropped on access.
    #[must_use]
    pub fn new(capacity: usize, ttl: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                order: VecDeque::new(),
            }),
            capacity: capacity.max(1),
            ttl,
        })
    }

    fn get(&self, subject_id: &Uuid) -> Option<KeyMaterial> {
        let mut inner = self.inner.lock().expect("dek cache mutex");
        let fresh = match inner.map.get(subject_id) {
            Some(e) if e.inserted.elapsed() < self.ttl => Some(KeyMaterial {
                key_id: e.key_id,
                key: e.key.clone(),
            }),
            Some(_) => None, // expired
            None => return None,
        };
        if fresh.is_none() {
            inner.map.remove(subject_id);
        }
        fresh
    }

    // The guard is legitimately held for the whole critical section (insert +
    // capacity eviction); there is nothing to tighten.
    #[allow(clippy::significant_drop_tightening)]
    fn insert(&self, subject_id: Uuid, material: &KeyMaterial) {
        // Build the entry (incl. the key clone) before locking, so the guard's
        // scope stays tight.
        let entry = Entry {
            key_id: material.key_id,
            key: material.key.clone(),
            inserted: Instant::now(),
        };
        let mut inner = self.inner.lock().expect("dek cache mutex");
        inner.map.insert(subject_id, entry);
        inner.order.push_back(subject_id);
        // Evict oldest live entries until within capacity.
        while inner.map.len() > self.capacity {
            match inner.order.pop_front() {
                Some(oldest) => {
                    inner.map.remove(&oldest);
                }
                None => break,
            }
        }
    }

    /// Remove a subject's cached DEK. Idempotent. This is the eviction the
    /// erasure path must call **after** committing a shred.
    ///
    /// # Panics
    /// Panics if the cache mutex is poisoned.
    pub fn remove(&self, subject_id: &Uuid) {
        self.inner
            .lock()
            .expect("dek cache mutex")
            .map
            .remove(subject_id);
    }

    /// Test helper: is a subject currently cached (ignoring TTL)?
    #[cfg(test)]
    fn contains(&self, subject_id: &Uuid) -> bool {
        self.inner
            .lock()
            .expect("dek cache mutex")
            .map
            .contains_key(subject_id)
    }
}

/// Wraps an inner [`KeyStore`], caching plaintext DEKs by subject in a [`DekCache`].
pub struct CachingKeyStore {
    inner: Arc<dyn KeyStore>,
    cache: Arc<DekCache>,
}

impl CachingKeyStore {
    #[must_use]
    pub fn new(inner: Arc<dyn KeyStore>, cache: Arc<DekCache>) -> Self {
        Self { inner, cache }
    }

    /// The shared cache — hand this to the erasure path for post-commit eviction.
    #[must_use]
    pub fn cache(&self) -> Arc<DekCache> {
        Arc::clone(&self.cache)
    }
}

#[async_trait]
impl KeyStore for CachingKeyStore {
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError> {
        if let Some(hit) = self.cache.get(subject_id) {
            return Ok(hit);
        }
        let material = self.inner.get_or_create_key(subject_id).await?;
        self.cache.insert(*subject_id, &material);
        Ok(material)
    }

    async fn get_key(&self, subject_id: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError> {
        if let Some(hit) = self.cache.get(subject_id) {
            return Ok(Some(hit));
        }
        let found = self.inner.get_key(subject_id).await?;
        if let Some(material) = &found {
            self.cache.insert(*subject_id, material);
        }
        Ok(found)
    }

    async fn delete_key(&self, subject_id: &Uuid) -> Result<(), KeyStoreError> {
        self.inner.delete_key(subject_id).await?;
        self.cache.remove(subject_id); // authoritative: this path owns its own tx
        Ok(())
    }

    #[cfg(feature = "postgres")]
    async fn delete_key_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        subject_id: &Uuid,
    ) -> Result<(), KeyStoreError> {
        self.inner.delete_key_in_tx(tx, subject_id).await?;
        // Best-effort: the caller must also evict after commit (see module docs).
        self.cache.remove(subject_id);
        Ok(())
    }

    async fn list_stale_subjects(
        &self,
        current_kek_id: &str,
        batch_size: usize,
        after: Option<Uuid>,
    ) -> Result<Vec<Uuid>, KeyStoreError> {
        self.inner
            .list_stale_subjects(current_kek_id, batch_size, after)
            .await
    }

    async fn rewrap_key(&self, subject_id: &Uuid) -> Result<bool, KeyStoreError> {
        // Re-wrap changes only the wrapped bytes, not the plaintext DEK, so any
        // cached plaintext stays valid — no eviction needed.
        self.inner.rewrap_key(subject_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_store::InMemoryKeyStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts calls to the inner store so we can assert cache hits.
    struct CountingKeyStore {
        inner: InMemoryKeyStore,
        get_calls: AtomicUsize,
    }
    impl CountingKeyStore {
        fn new() -> Self {
            Self {
                inner: InMemoryKeyStore::new(),
                get_calls: AtomicUsize::new(0),
            }
        }
    }
    #[async_trait]
    impl KeyStore for CountingKeyStore {
        async fn get_or_create_key(&self, s: &Uuid) -> Result<KeyMaterial, KeyStoreError> {
            self.inner.get_or_create_key(s).await
        }
        async fn get_key(&self, s: &Uuid) -> Result<Option<KeyMaterial>, KeyStoreError> {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.get_key(s).await
        }
        async fn delete_key(&self, s: &Uuid) -> Result<(), KeyStoreError> {
            self.inner.delete_key(s).await
        }
        async fn list_stale_subjects(
            &self,
            k: &str,
            n: usize,
            a: Option<Uuid>,
        ) -> Result<Vec<Uuid>, KeyStoreError> {
            self.inner.list_stale_subjects(k, n, a).await
        }
        async fn rewrap_key(&self, s: &Uuid) -> Result<bool, KeyStoreError> {
            self.inner.rewrap_key(s).await
        }
    }

    #[tokio::test]
    async fn second_read_is_a_cache_hit() {
        let counting = Arc::new(CountingKeyStore::new());
        let store = CachingKeyStore::new(
            Arc::clone(&counting) as _,
            DekCache::new(10, Duration::from_secs(60)),
        );
        let subject = Uuid::new_v4();
        store.get_or_create_key(&subject).await.unwrap();

        let _ = store.get_key(&subject).await.unwrap();
        let _ = store.get_key(&subject).await.unwrap();
        // get_or_create populated the cache, so neither get_key hit the inner store.
        assert_eq!(counting.get_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn delete_key_evicts_the_cache() {
        let inner = Arc::new(InMemoryKeyStore::new());
        let cache = DekCache::new(10, Duration::from_secs(60));
        let store = CachingKeyStore::new(inner, Arc::clone(&cache));
        let subject = Uuid::new_v4();
        store.get_or_create_key(&subject).await.unwrap();
        assert!(cache.contains(&subject));

        store.delete_key(&subject).await.unwrap();
        assert!(!cache.contains(&subject));
        assert!(store.get_key(&subject).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_entries_miss() {
        let inner = Arc::new(InMemoryKeyStore::new());
        let cache = DekCache::new(10, Duration::from_millis(1));
        let store = CachingKeyStore::new(Arc::clone(&inner) as _, Arc::clone(&cache));
        let subject = Uuid::new_v4();
        store.get_or_create_key(&subject).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(cache.get(&subject).is_none());
    }

    #[tokio::test]
    async fn capacity_evicts_oldest() {
        let cache = DekCache::new(2, Duration::from_secs(60));
        let mk = |b| KeyMaterial {
            key_id: Uuid::new_v4(),
            key: Zeroizing::new(vec![b; 32]),
        };
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        cache.insert(a, &mk(1));
        cache.insert(b, &mk(2));
        cache.insert(c, &mk(3)); // evicts a
        assert!(!cache.contains(&a));
        assert!(cache.contains(&b));
        assert!(cache.contains(&c));
    }
}
