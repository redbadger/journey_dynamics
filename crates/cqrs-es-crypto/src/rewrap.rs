//! Background worker for re-wrapping DEKs still encrypted under a retired KEK version.
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::{sync::Arc, time::Duration};
//! use cqrs_es_crypto::rewrap::{RewrapWorker, RewrapWorkerOptions};
//!
//! // One-shot sweep — useful in scheduled jobs or health checks:
//! let worker = RewrapWorker::new(store, provider, RewrapWorkerOptions::default());
//! let stats = worker.run_once().await?;
//!
//! // Or run forever as a background task:
//! tokio::spawn(async move {
//!     worker.run_forever(Duration::from_secs(60)).await
//! });
//! ```

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use crate::{
    kek::KekProvider,
    key_store::{KeyStore, KeyStoreError},
};

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for a [`RewrapWorker`].
#[derive(Debug, Clone)]
pub struct RewrapWorkerOptions {
    /// Number of subjects to fetch per database query. Default: `100`.
    pub batch_size: usize,
    /// Maximum number of concurrent re-wrap tasks within a single batch. Default: `8`.
    pub max_concurrency: usize,
    /// Pause inserted between consecutive batches to avoid overwhelming the database.
    ///
    /// Default: `100ms`. Set to [`Duration::ZERO`] to disable.
    pub batch_pause: Duration,
}

impl Default for RewrapWorkerOptions {
    fn default() -> Self {
        Self {
            batch_size: 100,
            max_concurrency: 8,
            batch_pause: Duration::from_millis(100),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Stats
// ─────────────────────────────────────────────────────────────────────────────

/// Statistics returned by a single [`RewrapWorker::run_once`] sweep.
#[derive(Debug, Default)]
pub struct RewrapStats {
    /// Total number of subject IDs examined during the sweep.
    pub scanned: usize,
    /// Number of DEKs successfully re-wrapped under the current KEK version.
    pub rewrapped: usize,
    /// Number of subjects whose re-wrap attempt failed (errors logged as warnings).
    pub failures: usize,
    /// Wall-clock duration of the entire sweep, including inter-batch pauses.
    pub duration: Duration,
}

// ─────────────────────────────────────────────────────────────────────────────
// Worker
// ─────────────────────────────────────────────────────────────────────────────

/// Background worker that re-wraps all DEKs still encrypted under a retired KEK version.
///
/// Create with [`RewrapWorker::new`] and drive with [`run_once`](Self::run_once)
/// (one sweep) or [`run_forever`](Self::run_forever) (polls on a timer).
///
/// # Zero-downtime rotation
///
/// Once the operator has promoted a new KEK version to primary, this worker sweeps
/// the database in cursor-paginated batches and re-wraps every stale DEK.  When
/// [`KeyStore::list_stale_subjects`] returns an empty batch the sweep is complete and
/// the old KEK version can be safely retired at the vault.
pub struct RewrapWorker {
    store: Arc<dyn KeyStore>,
    provider: Arc<dyn KekProvider>,
    options: RewrapWorkerOptions,
}

impl RewrapWorker {
    /// Creates a new [`RewrapWorker`].
    ///
    /// The `store` must implement [`KeyStore::list_stale_subjects`] and
    /// [`KeyStore::rewrap_key`].  The `provider` is queried for the current
    /// primary KEK version at the start of each sweep.
    #[must_use]
    pub fn new(
        store: Arc<dyn KeyStore>,
        provider: Arc<dyn KekProvider>,
        options: RewrapWorkerOptions,
    ) -> Self {
        Self {
            store,
            provider,
            options,
        }
    }

    /// Performs a single cursor-paginated sweep over all DEKs with a stale KEK version.
    ///
    /// Each batch is processed with bounded concurrency controlled by a [`Semaphore`]
    /// and a [`JoinSet`].  Per-subject failures are counted in [`RewrapStats::failures`]
    /// rather than propagated, so a partial failure does not abort the sweep.
    ///
    /// # Errors
    ///
    /// Returns a [`KeyStoreError`] if [`KeyStore::list_stale_subjects`] fails.
    /// Per-subject [`KeyStore::rewrap_key`] failures are logged as warnings and
    /// accumulated in [`RewrapStats::failures`] instead.
    ///
    /// # Panics
    ///
    /// Does not panic under normal operation.  The internal semaphore permit
    /// acquisition uses `.expect`, but the semaphore is created locally and never
    /// closed, so the acquire cannot fail.
    #[must_use = "inspect RewrapStats::failures to detect partial failures"]
    pub async fn run_once(&self) -> Result<RewrapStats, KeyStoreError> {
        let start = Instant::now();
        let mut stats = RewrapStats::default();
        let current_id = self.provider.current().id;
        let mut cursor: Option<Uuid> = None;

        loop {
            let batch = self
                .store
                .list_stale_subjects(&current_id, self.options.batch_size, cursor)
                .await?;

            if batch.is_empty() {
                break;
            }

            cursor = batch.last().copied();
            let batch_len = batch.len();
            stats.scanned += batch_len;

            let sem = Arc::new(Semaphore::new(self.options.max_concurrency));
            let mut join_set = JoinSet::new();

            for subject_id in batch {
                let permit = Arc::clone(&sem)
                    .acquire_owned()
                    .await
                    .expect("semaphore is never closed");
                let store = Arc::clone(&self.store);
                join_set.spawn(async move {
                    let result = store.rewrap_key(&subject_id).await;
                    drop(permit);
                    result
                });
            }

            let mut batch_rewrapped = 0_usize;
            let mut batch_failures = 0_usize;

            while let Some(join_result) = join_set.join_next().await {
                match join_result {
                    Ok(Ok(true)) => batch_rewrapped += 1,
                    Ok(Ok(false)) => {} // already current or shredded between list and rewrap
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "rewrap_key failed for a subject");
                        batch_failures += 1;
                    }
                    Err(join_err) => {
                        tracing::warn!(error = %join_err, "rewrap task panicked");
                        batch_failures += 1;
                    }
                }
            }

            stats.rewrapped += batch_rewrapped;
            stats.failures += batch_failures;

            tracing::info!(
                batch_scanned = batch_len,
                batch_rewrapped,
                batch_failures,
                total_scanned = stats.scanned,
                total_rewrapped = stats.rewrapped,
                total_failures = stats.failures,
                "rewrap batch complete"
            );

            if !self.options.batch_pause.is_zero() {
                tokio::time::sleep(self.options.batch_pause).await;
            }
        }

        stats.duration = start.elapsed();

        tracing::info!(
            scanned = stats.scanned,
            rewrapped = stats.rewrapped,
            failures = stats.failures,
            duration_ms = stats.duration.as_millis(),
            "rewrap sweep complete"
        );

        Ok(stats)
    }

    /// Runs the re-wrap worker in an infinite loop, sleeping `poll` between sweeps.
    ///
    /// Never returns.  Intended to be spawned as a long-lived background task.
    pub async fn run_forever(&self, poll: Duration) -> ! {
        loop {
            match self.run_once().await {
                Ok(stats) if stats.failures > 0 => {
                    tracing::warn!(
                        failures = stats.failures,
                        scanned = stats.scanned,
                        rewrapped = stats.rewrapped,
                        "rewrap sweep completed with failures"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "rewrap sweep failed"),
            }
            tokio::time::sleep(poll).await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc, time::Duration};

    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        kek::{KekProvider, StaticKekProvider},
        key_store::{InMemoryKeyStore, KeyStore},
    };

    use super::{RewrapWorker, RewrapWorkerOptions};

    // ── Provider helpers ──────────────────────────────────────────────────────

    fn v1_provider() -> Arc<dyn KekProvider> {
        Arc::new(StaticKekProvider::single("v1", vec![0x42u8; 32]).unwrap())
    }

    fn v1_v2_provider() -> Arc<dyn KekProvider> {
        let mut keks = HashMap::new();
        keks.insert("v1".to_string(), Zeroizing::new(vec![0x42u8; 32]));
        keks.insert("v2".to_string(), Zeroizing::new(vec![0xDEu8; 32]));
        Arc::new(StaticKekProvider::new("v2", keks).unwrap())
    }

    fn fast_options() -> RewrapWorkerOptions {
        RewrapWorkerOptions {
            batch_size: 10,
            max_concurrency: 2,
            batch_pause: Duration::ZERO,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// An empty store produces all-zero stats.
    #[tokio::test]
    async fn run_once_on_empty_store_returns_zero_stats() {
        let store = Arc::new(InMemoryKeyStore::new_with_provider(v1_provider()));
        let worker = RewrapWorker::new(
            Arc::clone(&store) as Arc<dyn KeyStore>,
            v1_provider(),
            fast_options(),
        );

        let stats = worker.run_once().await.unwrap();

        assert_eq!(stats.scanned, 0);
        assert_eq!(stats.rewrapped, 0);
        assert_eq!(stats.failures, 0);
    }

    /// Three subjects inserted with `kek_id` `"v1"` are all re-wrapped when the
    /// primary is promoted to "v2".
    #[tokio::test]
    async fn run_once_rewraps_stale_entries() {
        let provider = v1_v2_provider();
        let store = Arc::new(InMemoryKeyStore::new_with_provider(Arc::clone(&provider)));

        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();
        let subject_c = Uuid::new_v4();
        store.insert_for_testing(subject_a, "v1");
        store.insert_for_testing(subject_b, "v1");
        store.insert_for_testing(subject_c, "v1");

        let worker = RewrapWorker::new(
            Arc::clone(&store) as Arc<dyn KeyStore>,
            Arc::clone(&provider),
            fast_options(),
        );

        let stats = worker.run_once().await.unwrap();

        assert_eq!(stats.scanned, 3);
        assert_eq!(stats.rewrapped, 3);
        assert_eq!(stats.failures, 0);

        // Confirm no subjects remain stale.
        let still_stale = store.list_stale_subjects("v2", 10, None).await.unwrap();
        assert!(
            still_stale.is_empty(),
            "all entries should have been re-wrapped to v2"
        );
    }

    /// A subject already wrapped under the current KEK is not visited at all.
    #[tokio::test]
    async fn run_once_skips_already_current_entries() {
        let provider = v1_provider();
        let store = Arc::new(InMemoryKeyStore::new_with_provider(Arc::clone(&provider)));
        let subject = Uuid::new_v4();
        store.get_or_create_key(&subject).await.unwrap();

        let worker = RewrapWorker::new(
            Arc::clone(&store) as Arc<dyn KeyStore>,
            Arc::clone(&provider),
            fast_options(),
        );

        let stats = worker.run_once().await.unwrap();

        assert_eq!(stats.scanned, 0, "current entry should not be scanned");
        assert_eq!(stats.rewrapped, 0);
        assert_eq!(stats.failures, 0);
    }

    /// A mix of stale and current entries: only the stale ones are counted.
    #[tokio::test]
    async fn run_once_returns_correct_counts() {
        let provider = v1_v2_provider();
        let store = Arc::new(InMemoryKeyStore::new_with_provider(Arc::clone(&provider)));

        let stale_a = Uuid::new_v4();
        let stale_b = Uuid::new_v4();
        let current = Uuid::new_v4();

        store.insert_for_testing(stale_a, "v1");
        store.insert_for_testing(stale_b, "v1");
        store.insert_for_testing(current, "v2"); // already under the primary

        let worker = RewrapWorker::new(
            Arc::clone(&store) as Arc<dyn KeyStore>,
            Arc::clone(&provider),
            fast_options(),
        );

        let stats = worker.run_once().await.unwrap();

        assert_eq!(stats.scanned, 2, "only stale entries should be scanned");
        assert_eq!(stats.rewrapped, 2);
        assert_eq!(stats.failures, 0);
    }
}
