//! [`CryptoShreddingEventRepository`] — transparent PII encryption for persisted event streams.
//!
//! Wraps any [`PersistedEventRepository`] and intercepts:
//! - **Write path** (`persist`): encrypts PII fields in `PersonCaptured` and
//!   `PersonDetailsUpdated` events under the subject's Data Encryption Key (DEK).
//! - **Read path** (`get_events`, `get_last_events`, `stream_events`): decrypts, or redacts
//!   when the DEK has been deleted (crypto-shredding).
//!
//! `Modified` events carry only shared, non-PII data and are **never** encrypted.
//! Each PII event carries its own `subject_id` in plaintext, so the crypto layer is
//! entirely stateless with respect to journey–subject relationships — no mapping table
//! is needed.
//!
//! # Event-type conventions
//!
//! | Variant                        | `event_type` string      | Payload outer key        |
//! |--------------------------------|--------------------------|--------------------------|
//! | `JourneyEvent::PersonCaptured` | `"PersonCaptured"`       | `"PersonCaptured"`       |
//! | `JourneyEvent::PersonDetailsUpdated` | `"PersonDetailsUpdated"` | `"PersonDetailsUpdated"` |

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es::{
    Aggregate,
    persist::{
        PersistedEventRepository, PersistenceError, ReplayStream, SerializedEvent,
        SerializedSnapshot,
    },
};
use serde_json::Value;
use uuid::Uuid;

use super::{
    cipher::{EncryptedPayload, PiiCipher},
    key_store::KeyStore,
};

// ── Event-type strings (from DomainEvent::event_type()) ───────────────────

const PERSON_CAPTURED: &str = "PersonCaptured";
const PERSON_DETAILS_UPDATED: &str = "PersonDetailsUpdated";

// ── Payload outer keys (serde external enum tagging) ──────────────────────

const PC_KEY: &str = "PersonCaptured";
const PD_KEY: &str = "PersonDetailsUpdated";

// ── Internal error type ────────────────────────────────────────────────────

/// Private error type that unifies all failure modes in the crypto layer and
/// converts them to [`PersistenceError`].
#[derive(Debug, thiserror::Error)]
enum RepoError {
    #[error("{0}")]
    InvalidPayload(&'static str),
    #[error("UUID parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("Crypto error: {0}")]
    Crypto(#[from] super::cipher::CryptoError),
    #[error("Key store error: {0}")]
    KeyStore(#[from] super::key_store::KeyStoreError),
    #[error("Event repository lock poisoned — another thread panicked while holding the lock")]
    LockPoisoned,
}

impl<T> From<std::sync::PoisonError<T>> for RepoError {
    fn from(_: std::sync::PoisonError<T>) -> Self {
        Self::LockPoisoned
    }
}

impl From<RepoError> for PersistenceError {
    fn from(e: RepoError) -> Self {
        Self::UnknownError(Box::new(e))
    }
}

// ── InMemoryEventRepository ────────────────────────────────────────────────

/// An in-memory [`PersistedEventRepository`] backed by a `Mutex<Vec<SerializedEvent>>`.
///
/// Intended for use in unit tests. Snapshots are not supported.
pub struct InMemoryEventRepository {
    events: std::sync::Mutex<Vec<SerializedEvent>>,
}

impl Default for InMemoryEventRepository {
    fn default() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl InMemoryEventRepository {
    /// Returns a clone of every stored [`SerializedEvent`] in insertion order.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn all_events(&self) -> Vec<SerializedEvent> {
        self.events
            .lock()
            .expect("InMemoryEventRepository mutex poisoned")
            .clone()
    }

    /// Acquires the mutex, mapping a [`PoisonError`] to a [`PersistenceError`].
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, Vec<SerializedEvent>>, PersistenceError> {
        self.events
            .lock()
            .map_err(|e| PersistenceError::from(RepoError::from(e)))
    }
}

impl PersistedEventRepository for InMemoryEventRepository {
    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        Ok(self
            .locked()?
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
        let filtered: Vec<SerializedEvent> = {
            let events = self.locked()?;
            events
                .iter()
                .filter(|e| e.aggregate_id == aggregate_id)
                .cloned()
                .collect()
        };
        let len = filtered.len();
        Ok(filtered
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
        self.locked()?.extend_from_slice(events);
        Ok(())
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        let events: Vec<SerializedEvent> = self
            .locked()?
            .iter()
            .filter(|e| e.aggregate_id == aggregate_id)
            .cloned()
            .collect();
        // tokio channel capacity must be >= 1.
        let (mut feed, stream) = ReplayStream::new(events.len().max(1));
        for event in events {
            feed.push(Ok(event)).await?;
        }
        Ok(stream)
    }

    async fn stream_all_events<A: Aggregate>(&self) -> Result<ReplayStream, PersistenceError> {
        let events: Vec<SerializedEvent> = self.locked()?.clone();
        let (mut feed, stream) = ReplayStream::new(events.len().max(1));
        for event in events {
            feed.push(Ok(event)).await?;
        }
        Ok(stream)
    }
}

// ── CryptoShreddingEventRepository ────────────────────────────────────────

/// Wraps an inner [`PersistedEventRepository`] and transparently encrypts/decrypts
/// PII-bearing event payloads for GDPR crypto-shredding.
///
/// # Write path
///
/// - **`PersonCaptured`**: encrypts `name`, `email`, and `phone` into a single
///   `encrypted_pii` blob using AES-256-GCM. `person_ref` and `subject_id` remain
///   in plaintext so the correct DEK can be located on the read path without
///   decrypting anything first.
/// - **`PersonDetailsUpdated`**: encrypts the entire `data` field. `person_ref` and
///   `subject_id` remain in plaintext.
/// - All other events (including `Modified`): **passed through unmodified**. `Modified`
///   events carry only shared, non-PII journey data.
///
/// # Read path
///
/// - Encrypted events: decrypted when the DEK is present in the key store.
/// - Encrypted events whose DEK has been deleted (subject forgotten): redacted — PII
///   fields are replaced with `"[redacted]"` / `null` / `{}`.
/// - Events without encryption sentinels (plaintext / legacy): returned unmodified.
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    pub(crate) inner: R,
    key_store: Arc<dyn KeyStore>,
    cipher: Arc<PiiCipher>,
}

impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Create a new crypto-shredding repository wrapping `inner`.
    pub fn new(inner: R, key_store: Arc<dyn KeyStore>, cipher: PiiCipher) -> Self {
        Self {
            inner,
            key_store,
            cipher: Arc::new(cipher),
        }
    }

    // ── Write helpers ─────────────────────────────────────────────────────

    async fn encrypt_events(
        &self,
        events: &[SerializedEvent],
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let event = event.clone();
            let event = match event.event_type.as_str() {
                PERSON_CAPTURED => self.encrypt_person_captured(event).await?,
                PERSON_DETAILS_UPDATED => self.encrypt_person_details_updated(event).await?,
                _ => event, // Modified and all others are always stored in plaintext.
            };
            out.push(event);
        }
        Ok(out)
    }

    /// Encrypts the PII identity fields (`name`, `email`, `phone`) of a `PersonCaptured`
    /// event into a single authenticated blob.
    ///
    /// `person_ref` and `subject_id` are kept in plaintext so the read path can locate
    /// the correct DEK without decrypting anything first.
    ///
    /// If `subject_id` is absent from the payload (legacy event without a subject), the
    /// event is returned unmodified.
    async fn encrypt_person_captured(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        // subject_id must be present. If missing (legacy event), pass through unmodified.
        let subject_id_str = match event.payload[PC_KEY]["subject_id"].as_str() {
            Some(s) => s.to_string(),
            None => return Ok(event),
        };
        let subject_id = Uuid::parse_str(&subject_id_str)
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        // person_ref is not PII — keep it in plaintext alongside subject_id.
        let person_ref_str = event.payload[PC_KEY]["person_ref"]
            .as_str()
            .unwrap_or("")
            .to_string();

        // Get or create the DEK for this subject.
        let dek = self
            .key_store
            .get_or_create_key(&subject_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        // Gather PII fields from the payload.
        let pc = event.payload[PC_KEY].as_object().ok_or_else(|| {
            PersistenceError::from(RepoError::InvalidPayload(
                "PersonCaptured payload is not an object",
            ))
        })?;

        let pii = serde_json::json!({
            "name":  pc.get("name") .cloned().unwrap_or(Value::Null),
            "email": pc.get("email").cloned().unwrap_or(Value::Null),
            "phone": pc.get("phone").cloned().unwrap_or(Value::Null),
        });

        // AAD = "<aggregate_id>:<sequence>" — binds ciphertext to this event position.
        let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
        let plaintext = serde_json::to_vec(&pii)?;
        let encrypted = self.cipher.encrypt(&dek, &plaintext, &aad);

        event.payload = serde_json::json!({
            "PersonCaptured": {
                "person_ref":    person_ref_str,
                "subject_id":    subject_id_str,
                "encrypted_pii": BASE64.encode(&encrypted.ciphertext),
                "nonce":         BASE64.encode(&encrypted.nonce),
            }
        });

        Ok(event)
    }

    /// Encrypts the `data` field of a `PersonDetailsUpdated` event.
    ///
    /// `person_ref` and `subject_id` are kept in plaintext. If `subject_id` is absent
    /// (legacy event), the event is returned unmodified.
    async fn encrypt_person_details_updated(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        // subject_id must be present. If missing (legacy event), pass through unmodified.
        let subject_id_str = match event.payload[PD_KEY]["subject_id"].as_str() {
            Some(s) => s.to_string(),
            None => return Ok(event),
        };
        let subject_id = Uuid::parse_str(&subject_id_str)
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        // person_ref is not PII — keep in plaintext.
        let person_ref_str = event.payload[PD_KEY]["person_ref"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let dek = self
            .key_store
            .get_or_create_key(&subject_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        let data = event.payload[PD_KEY]["data"].clone();

        let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
        let plaintext = serde_json::to_vec(&data)?;
        let encrypted = self.cipher.encrypt(&dek, &plaintext, &aad);

        event.payload = serde_json::json!({
            "PersonDetailsUpdated": {
                "person_ref":     person_ref_str,
                "subject_id":     subject_id_str,
                "encrypted_data": BASE64.encode(&encrypted.ciphertext),
                "nonce":          BASE64.encode(&encrypted.nonce),
            }
        });

        Ok(event)
    }

    // ── Read helpers ──────────────────────────────────────────────────────

    async fn decrypt_events(
        &self,
        events: Vec<SerializedEvent>,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let event = match event.event_type.as_str() {
                PERSON_CAPTURED => self.decrypt_person_captured(event).await?,
                PERSON_DETAILS_UPDATED => self.decrypt_person_details_updated(event).await?,
                _ => event,
            };
            out.push(event);
        }
        Ok(out)
    }

    /// Decrypts a `PersonCaptured` event, or redacts it if the DEK has been deleted.
    ///
    /// Events without an `encrypted_pii` sentinel (plaintext / legacy) are returned as-is.
    async fn decrypt_person_captured(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        // No sentinel → plaintext event, pass through.
        if event.payload[PC_KEY].get("encrypted_pii").is_none() {
            return Ok(event);
        }

        let subject_id_str = event.payload[PC_KEY]["subject_id"]
            .as_str()
            .ok_or_else(|| {
                PersistenceError::from(RepoError::InvalidPayload(
                    "encrypted PersonCaptured is missing subject_id",
                ))
            })?
            .to_string();
        // person_ref is stored in plaintext — restore it verbatim.
        let person_ref_str = event.payload[PC_KEY]["person_ref"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let subject_id = Uuid::parse_str(&subject_id_str)
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        let dek = self
            .key_store
            .get_key(&subject_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        match dek {
            Some(dek) => {
                let ciphertext = BASE64
                    .decode(
                        event.payload[PC_KEY]["encrypted_pii"]
                            .as_str()
                            .unwrap_or_default(),
                    )
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;
                let nonce = BASE64
                    .decode(event.payload[PC_KEY]["nonce"].as_str().unwrap_or_default())
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

                let encrypted = EncryptedPayload { ciphertext, nonce };
                let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
                let plaintext = self
                    .cipher
                    .decrypt(&dek, &encrypted, &aad)
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

                let pii: Value = serde_json::from_slice(&plaintext)?;

                event.payload = serde_json::json!({
                    "PersonCaptured": {
                        "person_ref": person_ref_str,
                        "subject_id": subject_id_str,
                        "name":       pii["name"],
                        "email":      pii["email"],
                        "phone":      pii["phone"],
                    }
                });
            }
            None => {
                // Key deleted — subject was forgotten. Redact.
                event.payload = serde_json::json!({
                    "PersonCaptured": {
                        "person_ref": person_ref_str,
                        "subject_id": subject_id_str,
                        "name":       "[redacted]",
                        "email":      "[redacted]",
                        "phone":      null,
                    }
                });
            }
        }

        Ok(event)
    }

    /// Decrypts a `PersonDetailsUpdated` event, or redacts it if the DEK has been deleted.
    ///
    /// Events without an `encrypted_data` sentinel (plaintext / legacy) are returned as-is.
    async fn decrypt_person_details_updated(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        // No sentinel → plaintext event, pass through.
        if event.payload[PD_KEY].get("encrypted_data").is_none() {
            return Ok(event);
        }

        let subject_id_str = event.payload[PD_KEY]["subject_id"]
            .as_str()
            .ok_or_else(|| {
                PersistenceError::from(RepoError::InvalidPayload(
                    "encrypted PersonDetailsUpdated is missing subject_id",
                ))
            })?
            .to_string();
        let person_ref_str = event.payload[PD_KEY]["person_ref"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let subject_id = Uuid::parse_str(&subject_id_str)
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        let dek = self
            .key_store
            .get_key(&subject_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        match dek {
            Some(dek) => {
                let ciphertext = BASE64
                    .decode(
                        event.payload[PD_KEY]["encrypted_data"]
                            .as_str()
                            .unwrap_or_default(),
                    )
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;
                let nonce = BASE64
                    .decode(event.payload[PD_KEY]["nonce"].as_str().unwrap_or_default())
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

                let encrypted = EncryptedPayload { ciphertext, nonce };
                let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
                let plaintext = self
                    .cipher
                    .decrypt(&dek, &encrypted, &aad)
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

                let data: Value = serde_json::from_slice(&plaintext)?;

                event.payload = serde_json::json!({
                    "PersonDetailsUpdated": {
                        "person_ref": person_ref_str,
                        "subject_id": subject_id_str,
                        "data":       data,
                    }
                });
            }
            None => {
                // Key deleted — subject was forgotten. Redact.
                event.payload = serde_json::json!({
                    "PersonDetailsUpdated": {
                        "person_ref": person_ref_str,
                        "subject_id": subject_id_str,
                        "data":       {},
                    }
                });
            }
        }

        Ok(event)
    }
}

// ── PersistedEventRepository impl ─────────────────────────────────────────

impl<R: PersistedEventRepository> PersistedEventRepository for CryptoShreddingEventRepository<R> {
    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let events = self.inner.get_events::<A>(aggregate_id).await?;
        self.decrypt_events(events).await
    }

    async fn get_last_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
        last_sequence: usize,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let events = self
            .inner
            .get_last_events::<A>(aggregate_id, last_sequence)
            .await?;
        self.decrypt_events(events).await
    }

    async fn get_snapshot<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Option<SerializedSnapshot>, PersistenceError> {
        // Snapshots are not encrypted — the aggregate state contains no PII.
        self.inner.get_snapshot::<A>(aggregate_id).await
    }

    async fn persist<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        snapshot_update: Option<(String, Value, usize)>,
    ) -> Result<(), PersistenceError> {
        let encrypted = self.encrypt_events(events).await?;
        self.inner.persist::<A>(&encrypted, snapshot_update).await
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        // Collect decrypted events, then feed a new stream.
        let events = self.get_events::<A>(aggregate_id).await?;
        let (mut feed, stream) = ReplayStream::new(events.len().max(1));
        for event in events {
            feed.push(Ok(event)).await?;
        }
        Ok(stream)
    }

    async fn stream_all_events<A: Aggregate>(&self) -> Result<ReplayStream, PersistenceError> {
        // NOTE: This delegates to the inner without decryption. `stream_all_events` is used
        // only by `QueryReplay::replay_all`, which is not currently wired in this application.
        // A production-ready implementation would need to decrypt each event before pushing.
        self.inner.stream_all_events::<A>().await
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cqrs_es::persist::{PersistedEventRepository, SerializedEvent};
    use uuid::Uuid;

    use crate::crypto::cipher::PiiCipher;
    use crate::crypto::key_store::{InMemoryKeyStore, KeyStore};
    use crate::domain::journey::Journey;

    use super::{CryptoShreddingEventRepository, InMemoryEventRepository};

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Build a repo backed by in-memory test doubles.
    fn make_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
        let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
        let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
        CryptoShreddingEventRepository::new(InMemoryEventRepository::default(), key_store, cipher)
    }

    /// Build a repo and return a handle to the key store for inspection / shredding.
    fn make_repo_with_parts() -> (
        CryptoShreddingEventRepository<InMemoryEventRepository>,
        Arc<InMemoryKeyStore>,
    ) {
        let key_store = Arc::new(InMemoryKeyStore::new());
        let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            cipher,
        );
        (repo, key_store)
    }

    fn person_captured_event(
        aggregate_id: &str,
        sequence: usize,
        subject_id: Uuid,
    ) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref": "passenger_0",
                    "subject_id": subject_id.to_string(),
                    "name":       "Alice Smith",
                    "email":      "alice@example.com",
                    "phone":      "+44-7700-900000"
                }
            }),
            serde_json::json!({}),
        )
    }

    fn person_details_updated_event(
        aggregate_id: &str,
        sequence: usize,
        subject_id: Uuid,
    ) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonDetailsUpdated": {
                    "person_ref": "passenger_0",
                    "subject_id": subject_id.to_string(),
                    "data": {
                        "passportNumber": "GB123456789",
                        "dateOfBirth":    "1990-05-15",
                        "nationality":    "GB"
                    }
                }
            }),
            serde_json::json!({}),
        )
    }

    fn modified_event(aggregate_id: &str, sequence: usize) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "JourneyModified".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "Modified": {
                    "step": "search",
                    "data": {
                        "tripType":    "round-trip",
                        "origin":      "LHR",
                        "destination": "JFK"
                    }
                }
            }),
            serde_json::json!({}),
        )
    }

    fn started_event(aggregate_id: &str, sequence: usize) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "JourneyOpened".to_string(),
            "1.0".to_string(),
            serde_json::json!({ "Started": { "id": aggregate_id } }),
            serde_json::json!({}),
        )
    }

    // ── Non-PII events ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_non_pii_events_pass_through_unchanged() {
        let repo = make_repo();
        let aggregate_id = "journey-pass-through";
        let event = started_event(aggregate_id, 1);
        let original_payload = event.payload.clone();

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(raw.len(), 1);
        assert_eq!(
            raw[0].payload, original_payload,
            "non-PII event payload must not be modified"
        );
    }

    #[tokio::test]
    async fn test_modified_events_always_pass_through_unmodified() {
        // Modified events carry only shared non-PII data and must never be encrypted,
        // regardless of whether a subject has been captured for the journey.
        let repo = make_repo();
        let aggregate_id = "journey-modified-plain";
        let subject_id = Uuid::new_v4();

        // Capture a person first (DEK now exists in the key store).
        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let mod_event = modified_event(aggregate_id, 2);
        let original_payload = mod_event.payload.clone();

        repo.persist::<Journey>(&[mod_event], None).await.unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(
            raw[1].payload, original_payload,
            "Modified event must be stored in plaintext even after a subject is captured"
        );
        assert!(
            raw[1].payload["Modified"]["data"]
                .get("encrypted_data")
                .is_none(),
            "Modified data must never contain an encrypted_data sentinel"
        );
    }

    // ── PersonCaptured — write path ───────────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_person_captured_pii_fields() {
        let repo = make_repo();
        let aggregate_id = "journey-pc-encrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(raw.len(), 1);
        let pc = &raw[0].payload["PersonCaptured"];

        // PII must NOT be stored in plaintext.
        assert!(pc.get("name").is_none(), "name must not be in plaintext");
        assert!(pc.get("email").is_none(), "email must not be in plaintext");
        assert!(pc.get("phone").is_none(), "phone must not be in plaintext");

        // Encryption envelope must be present.
        assert!(
            pc.get("encrypted_pii").is_some(),
            "encrypted_pii must be present"
        );
        assert!(pc.get("nonce").is_some(), "nonce must be present");

        // Non-PII fields remain in plaintext.
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
            "subject_id must remain in plaintext"
        );
        assert_eq!(
            pc["person_ref"].as_str().unwrap(),
            "passenger_0",
            "person_ref must remain in plaintext"
        );
    }

    #[tokio::test]
    async fn test_person_captured_without_subject_id_passes_through_on_write() {
        // A PersonCaptured without a subject_id field must be stored unmodified.
        let repo = make_repo();
        let aggregate_id = "journey-legacy-pc-write";
        let legacy_payload = serde_json::json!({
            "PersonCaptured": {
                "name":  "Bob Jones",
                "email": "bob@example.com",
                "phone": null
            }
        });
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            legacy_payload.clone(),
            serde_json::json!({}),
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(
            raw[0].payload, legacy_payload,
            "PersonCaptured without subject_id must be stored unmodified"
        );
    }

    // ── PersonCaptured — read path ────────────────────────────────────────

    #[tokio::test]
    async fn test_get_events_decrypts_person_captured() {
        let repo = make_repo();
        let aggregate_id = "journey-pc-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(events.len(), 1);
        let pc = &events[0].payload["PersonCaptured"];

        assert_eq!(pc["name"].as_str().unwrap(), "Alice Smith");
        assert_eq!(pc["email"].as_str().unwrap(), "alice@example.com");
        assert_eq!(pc["phone"].as_str().unwrap(), "+44-7700-900000");
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str()
        );
        assert_eq!(pc["person_ref"].as_str().unwrap(), "passenger_0");
        assert!(
            pc.get("encrypted_pii").is_none(),
            "encrypted_pii must not appear after decryption"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_person_captured_when_key_deleted() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "journey-pc-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        // Simulate crypto-shredding.
        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let pc = &events[0].payload["PersonCaptured"];

        assert_eq!(pc["name"].as_str().unwrap(), "[redacted]");
        assert_eq!(pc["email"].as_str().unwrap(), "[redacted]");
        assert!(pc["phone"].is_null(), "phone must be null after shredding");
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
            "subject_id must remain readable for audit purposes"
        );
        assert_eq!(
            pc["person_ref"].as_str().unwrap(),
            "passenger_0",
            "person_ref must remain readable after shredding"
        );
    }

    #[tokio::test]
    async fn test_plaintext_person_captured_passes_through_on_read() {
        // Legacy event without encrypted_pii — must be returned verbatim.
        let repo = make_repo();
        let aggregate_id = "journey-legacy-pc-read";
        let legacy_payload = serde_json::json!({
            "PersonCaptured": {
                "name":  "Carol White",
                "email": "carol@example.com",
                "phone": null
            }
        });

        // Inject directly into the inner store, bypassing the crypto write path.
        repo.inner
            .persist::<Journey>(
                &[SerializedEvent::new(
                    aggregate_id.to_string(),
                    1,
                    "Journey".to_string(),
                    "PersonCaptured".to_string(),
                    "1.0".to_string(),
                    legacy_payload.clone(),
                    serde_json::json!({}),
                )],
                None,
            )
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload, legacy_payload,
            "legacy plaintext PersonCaptured must be returned unmodified"
        );
    }

    // ── PersonDetailsUpdated — write path ─────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_person_details_updated() {
        let repo = make_repo();
        let aggregate_id = "journey-pd-encrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(raw.len(), 1);
        let pd = &raw[0].payload["PersonDetailsUpdated"];

        // PII must NOT be stored in plaintext.
        assert!(
            pd.get("data")
                .is_none_or(|d| d.get("passportNumber").is_none()),
            "passportNumber must not appear in plaintext"
        );

        // Encryption envelope must be present.
        assert!(
            pd.get("encrypted_data").is_some(),
            "encrypted_data must be present"
        );
        assert!(pd.get("nonce").is_some(), "nonce must be present");

        // Non-PII fields remain in plaintext.
        assert_eq!(
            pd["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str()
        );
        assert_eq!(pd["person_ref"].as_str().unwrap(), "passenger_0");
    }

    #[tokio::test]
    async fn test_person_details_updated_without_subject_id_passes_through() {
        // A PersonDetailsUpdated without subject_id must be stored unmodified.
        let repo = make_repo();
        let aggregate_id = "journey-pd-legacy-write";
        let legacy_payload = serde_json::json!({
            "PersonDetailsUpdated": {
                "person_ref": "passenger_0",
                "data": { "passportNumber": "XX999" }
            }
        });
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            legacy_payload.clone(),
            serde_json::json!({}),
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(raw[0].payload, legacy_payload);
    }

    // ── PersonDetailsUpdated — read path ──────────────────────────────────

    #[tokio::test]
    async fn test_get_events_decrypts_person_details_updated() {
        let repo = make_repo();
        let aggregate_id = "journey-pd-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(events.len(), 1);
        let pd = &events[0].payload["PersonDetailsUpdated"];

        assert_eq!(
            pd["data"]["passportNumber"].as_str().unwrap(),
            "GB123456789"
        );
        assert_eq!(pd["data"]["dateOfBirth"].as_str().unwrap(), "1990-05-15");
        assert_eq!(pd["data"]["nationality"].as_str().unwrap(), "GB");
        assert_eq!(
            pd["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str()
        );
        assert_eq!(pd["person_ref"].as_str().unwrap(), "passenger_0");
        assert!(
            pd.get("encrypted_data").is_none(),
            "encrypted_data must not appear after decryption"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_person_details_updated_when_key_deleted() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "journey-pd-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let pd = &events[0].payload["PersonDetailsUpdated"];

        assert_eq!(
            pd["data"],
            serde_json::json!({}),
            "data must be empty after shredding"
        );
        assert_eq!(
            pd["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
            "subject_id must remain readable for audit purposes"
        );
        assert_eq!(
            pd["person_ref"].as_str().unwrap(),
            "passenger_0",
            "person_ref must remain readable after shredding"
        );
    }

    #[tokio::test]
    async fn test_plaintext_person_details_updated_passes_through_on_read() {
        let repo = make_repo();
        let aggregate_id = "journey-pd-legacy-read";
        let plaintext_payload = serde_json::json!({
            "PersonDetailsUpdated": {
                "person_ref": "passenger_0",
                "subject_id": Uuid::new_v4().to_string(),
                "data": { "passportNumber": "GB000000001" }
            }
        });

        repo.inner
            .persist::<Journey>(
                &[SerializedEvent::new(
                    aggregate_id.to_string(),
                    1,
                    "Journey".to_string(),
                    "PersonDetailsUpdated".to_string(),
                    "1.0".to_string(),
                    plaintext_payload.clone(),
                    serde_json::json!({}),
                )],
                None,
            )
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload, plaintext_payload,
            "legacy plaintext PersonDetailsUpdated must be returned unmodified"
        );
    }

    // ── Multi-subject scenarios ───────────────────────────────────────────

    #[tokio::test]
    async fn test_single_key_deletion_shreds_all_journeys_for_subject() {
        // The same subject captured in two journeys: deleting the single DEK
        // must make both journeys' PersonCaptured events unreadable.
        let (repo, key_store) = make_repo_with_parts();
        let subject_id = Uuid::new_v4();
        let journey_a = "journey-xj-a";
        let journey_b = "journey-xj-b";

        repo.persist::<Journey>(&[person_captured_event(journey_a, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(&[person_captured_event(journey_b, 1, subject_id)], None)
            .await
            .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events_a = repo.get_events::<Journey>(journey_a).await.unwrap();
        let events_b = repo.get_events::<Journey>(journey_b).await.unwrap();

        assert_eq!(
            events_a[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]",
            "journey A PersonCaptured must be redacted after key deletion"
        );
        assert_eq!(
            events_b[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]",
            "journey B PersonCaptured must be redacted after key deletion"
        );
    }

    #[tokio::test]
    async fn test_two_subjects_in_one_journey_shredded_independently() {
        // Two subjects captured in the same journey: shredding subject A must leave
        // subject B's events fully readable, and must not affect shared Modified events.
        let (repo, key_store) = make_repo_with_parts();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();
        let aggregate_id = "journey-two-subjects";

        let pc_a = person_captured_event(aggregate_id, 1, subject_a);
        // Manually build a PersonCaptured for subject B with a different person_ref.
        let pc_b = SerializedEvent::new(
            aggregate_id.to_string(),
            2,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref": "passenger_1",
                    "subject_id": subject_b.to_string(),
                    "name":       "Bob Jones",
                    "email":      "bob@example.com",
                    "phone":      null
                }
            }),
            serde_json::json!({}),
        );
        let mod_ev = modified_event(aggregate_id, 3);
        let original_mod_payload = mod_ev.payload.clone();

        repo.persist::<Journey>(&[pc_a, pc_b, mod_ev], None)
            .await
            .unwrap();

        // Shred only subject A.
        key_store.delete_key(&subject_a).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();

        // Subject A's PersonCaptured must be redacted.
        let ev_a = events
            .iter()
            .find(|e| e.payload["PersonCaptured"]["person_ref"].as_str() == Some("passenger_0"))
            .unwrap();
        assert_eq!(
            ev_a.payload["PersonCaptured"]["name"].as_str().unwrap(),
            "[redacted]"
        );

        // Subject B's PersonCaptured must still be readable.
        let ev_b = events
            .iter()
            .find(|e| e.payload["PersonCaptured"]["person_ref"].as_str() == Some("passenger_1"))
            .unwrap();
        assert_eq!(
            ev_b.payload["PersonCaptured"]["name"].as_str().unwrap(),
            "Bob Jones"
        );

        // The Modified event must be completely untouched.
        let mod_event = events
            .iter()
            .find(|e| e.event_type == "JourneyModified")
            .unwrap();
        assert_eq!(mod_event.payload, original_mod_payload);
    }

    // ── get_last_events ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_last_events_decrypts_correctly() {
        let repo = make_repo();
        let aggregate_id = "journey-last-events";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 2, subject_id)],
            None,
        )
        .await
        .unwrap();

        // Fetch only the last event (the PersonDetailsUpdated).
        let events = repo
            .get_last_events::<Journey>(aggregate_id, 1)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "PersonDetailsUpdated");
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"]["passportNumber"]
                .as_str()
                .unwrap(),
            "GB123456789"
        );
    }

    // ── stream_events ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_stream_events_returns_decrypted_domain_events() {
        let repo = make_repo();
        let aggregate_id = "journey-stream";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let mut stream = repo.stream_events::<Journey>(aggregate_id).await.unwrap();
        let envelope = stream
            .next::<Journey>(&[])
            .await
            .expect("stream must yield an event")
            .expect("event must deserialize without error");

        match envelope.payload {
            crate::domain::events::JourneyEvent::PersonCaptured {
                person_ref: _,
                name,
                email,
                phone,
                subject_id: sid,
            } => {
                assert_eq!(name, "Alice Smith");
                assert_eq!(email, "alice@example.com");
                assert_eq!(phone.as_deref(), Some("+44-7700-900000"));
                assert_eq!(sid, subject_id);
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    // ── AAD binding ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_aad_binds_ciphertext_to_event_position() {
        // Two PersonCaptured events at different sequence numbers must produce different
        // ciphertexts even for identical plaintext — both from different AAD and a fresh nonce.
        let repo = make_repo();
        let aggregate_id = "journey-aad";
        let subject_id = Uuid::new_v4();

        let ev1 = person_captured_event(aggregate_id, 1, subject_id);
        let ev2 = person_captured_event(aggregate_id, 2, subject_id);
        repo.persist::<Journey>(&[ev1, ev2], None).await.unwrap();

        let raw = repo.inner.all_events();
        let ct1 = raw[0].payload["PersonCaptured"]["encrypted_pii"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["PersonCaptured"]["encrypted_pii"]
            .as_str()
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "identical plaintext at different sequence numbers must produce different ciphertexts"
        );
    }

    #[tokio::test]
    async fn test_aad_binds_person_details_ciphertext_to_event_position() {
        let repo = make_repo();
        let aggregate_id = "journey-aad-pd";
        let subject_id = Uuid::new_v4();

        let ev1 = person_details_updated_event(aggregate_id, 1, subject_id);
        let ev2 = person_details_updated_event(aggregate_id, 2, subject_id);
        repo.persist::<Journey>(&[ev1, ev2], None).await.unwrap();

        let raw = repo.inner.all_events();
        let ct1 = raw[0].payload["PersonDetailsUpdated"]["encrypted_data"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["PersonDetailsUpdated"]["encrypted_data"]
            .as_str()
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "identical plaintext at different positions must produce different ciphertexts"
        );
    }
}
