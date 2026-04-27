//! Stub for `repository.rs` — full implementation is introduced in Phase 2.
//!
//! Declares the public types re-exported from `lib.rs` so the crate compiles
//! and `cargo check` / `cargo test` can validate `cipher.rs` and `key_store.rs`
//! independently.

use std::sync::Arc;

use cqrs_es::Aggregate;
use cqrs_es::persist::{
    PersistedEventRepository, PersistenceError, ReplayStream, SerializedEvent, SerializedSnapshot,
};
use serde_json::Value;
use uuid::Uuid;

use crate::cipher::PiiCipher;
use crate::key_store::KeyStore;

// ── PiiEventCodec ─────────────────────────────────────────────────────────────

/// The base64-encoded ciphertext and nonce to embed in the persisted payload.
pub struct EncryptedPiiSentinel {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
}

/// Instructions for encrypting a single event's PII fields.
pub struct PiiFields {
    /// The data-subject identifier — used to look up or create the DEK.
    pub subject_id: Uuid,

    /// The JSON blob of PII to encrypt (serialised to bytes and fed to AES-256-GCM).
    pub plaintext_pii: Value,

    /// Builds the payload to persist from the encryption sentinels.
    ///
    /// The non-PII fields (e.g. `person_ref`, `subject_id`) are preserved by
    /// this function; only the PII fields are replaced with the sentinel.
    pub build_encrypted_payload: Box<dyn FnOnce(EncryptedPiiSentinel) -> Value + Send>,
}

/// Describes how to locate and transform PII within a serialised event payload.
///
/// Implementors encode the domain-specific knowledge of which event types carry
/// PII, where the subject ID lives, which fields are sensitive, and how to
/// reassemble the payload after encryption or when redacting.
pub trait PiiEventCodec: Send + Sync {
    /// Inspect a serialised event and return encryption instructions, or `None`
    /// if this event type carries no PII and should be stored verbatim.
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields>;

    /// Rebuild the event payload from decrypted PII bytes.
    ///
    /// `plaintext_pii` is the JSON value that was originally passed as
    /// `PiiFields::plaintext_pii` during encryption.
    fn reconstruct(
        &self,
        event: &SerializedEvent,
        plaintext_pii: &Value,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>>;

    /// Rebuild the event payload with redacted placeholders (called when the DEK
    /// has been deleted and the subject's data is permanently irrecoverable).
    fn redact(
        &self,
        event: &SerializedEvent,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>>;
}

// ── CryptoShreddingEventRepository ───────────────────────────────────────────

/// Wraps an inner [`PersistedEventRepository`] and transparently encrypts /
/// decrypts PII-bearing event payloads for GDPR crypto-shredding.
///
/// Which event types carry PII, and how their payloads are structured, is
/// determined entirely by the [`PiiEventCodec`] implementation supplied at
/// construction time.
///
/// # Write path
///
/// Events for which [`PiiEventCodec::classify`] returns `Some` are encrypted
/// with a per-subject DEK before being forwarded to the inner repository.
/// All other events are forwarded unmodified.
///
/// # Read path
///
/// Events are decrypted when the DEK is present, or redacted via
/// [`PiiEventCodec::redact`] when the DEK has been deleted (subject forgotten).
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    pub(crate) inner: R,
    key_store: Arc<dyn KeyStore>,
    cipher: Arc<PiiCipher>,
    codec: Arc<dyn PiiEventCodec>,
}

impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Create a new crypto-shredding repository wrapping `inner`.
    pub fn new(
        inner: R,
        key_store: Arc<dyn KeyStore>,
        cipher: PiiCipher,
        codec: Arc<dyn PiiEventCodec>,
    ) -> Self {
        Self {
            inner,
            key_store,
            cipher: Arc::new(cipher),
            codec,
        }
    }
}

impl<R: PersistedEventRepository> PersistedEventRepository for CryptoShreddingEventRepository<R> {
    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        // Placeholder — full implementation in Phase 2.
        self.inner.get_events::<A>(aggregate_id).await
    }

    async fn get_last_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
        last_sequence: usize,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        self.inner
            .get_last_events::<A>(aggregate_id, last_sequence)
            .await
    }

    async fn get_snapshot<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Option<SerializedSnapshot>, PersistenceError> {
        self.inner.get_snapshot::<A>(aggregate_id).await
    }

    async fn persist<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        snapshot_update: Option<(String, Value, usize)>,
    ) -> Result<(), PersistenceError> {
        self.inner.persist::<A>(events, snapshot_update).await
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        self.inner.stream_events::<A>(aggregate_id).await
    }

    async fn stream_all_events<A: Aggregate>(&self) -> Result<ReplayStream, PersistenceError> {
        self.inner.stream_all_events::<A>().await
    }
}

// ── InMemoryEventRepository ───────────────────────────────────────────────────

/// An in-memory [`PersistedEventRepository`] backed by a `Mutex<Vec<SerializedEvent>>`.
///
/// Intended for use in tests. Snapshots are not supported.
/// Available when the `testing` feature is enabled or during `cfg(test)`.
#[cfg(any(test, feature = "testing"))]
pub struct InMemoryEventRepository {
    events: std::sync::Mutex<Vec<SerializedEvent>>,
}

#[cfg(any(test, feature = "testing"))]
impl Default for InMemoryEventRepository {
    fn default() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl InMemoryEventRepository {
    /// Returns a clone of every stored [`SerializedEvent`] in insertion order.
    pub fn all_events(&self) -> Vec<SerializedEvent> {
        self.events
            .lock()
            .expect("InMemoryEventRepository mutex poisoned")
            .clone()
    }
}

#[cfg(any(test, feature = "testing"))]
impl PersistedEventRepository for InMemoryEventRepository {
    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        Ok(self
            .events
            .lock()
            .map_err(|_| PersistenceError::UnknownError("mutex poisoned".into()))?
            .iter()
            .filter(|e| e.aggregate_id == aggregate_id)
            .cloned()
            .collect())
    }

    async fn get_last_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
        last_sequence: usize,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let all: Vec<SerializedEvent> = self
            .events
            .lock()
            .map_err(|_| PersistenceError::UnknownError("mutex poisoned".into()))?
            .iter()
            .filter(|e| e.aggregate_id == aggregate_id)
            .cloned()
            .collect();
        let len = all.len();
        Ok(all
            .into_iter()
            .skip(len.saturating_sub(last_sequence))
            .collect())
    }

    async fn get_snapshot<A: Aggregate>(
        &self,
        _aggregate_id: &str,
    ) -> Result<Option<SerializedSnapshot>, PersistenceError> {
        Ok(None)
    }

    async fn persist<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        _snapshot_update: Option<(String, Value, usize)>,
    ) -> Result<(), PersistenceError> {
        self.events
            .lock()
            .map_err(|_| PersistenceError::UnknownError("mutex poisoned".into()))?
            .extend_from_slice(events);
        Ok(())
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        let events: Vec<SerializedEvent> = self
            .events
            .lock()
            .map_err(|_| PersistenceError::UnknownError("mutex poisoned".into()))?
            .iter()
            .filter(|e| e.aggregate_id == aggregate_id)
            .cloned()
            .collect();
        let (mut feed, stream) = ReplayStream::new(events.len().max(1));
        for event in events {
            feed.push(Ok(event)).await?;
        }
        Ok(stream)
    }

    async fn stream_all_events<A: Aggregate>(&self) -> Result<ReplayStream, PersistenceError> {
        let events: Vec<SerializedEvent> = self
            .events
            .lock()
            .map_err(|_| PersistenceError::UnknownError("mutex poisoned".into()))?
            .clone();
        let (mut feed, stream) = ReplayStream::new(events.len().max(1));
        for event in events {
            feed.push(Ok(event)).await?;
        }
        Ok(stream)
    }
}
