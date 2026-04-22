//! [`CryptoShreddingEventRepository`] — transparent PII encryption for persisted event streams.
//!
//! Wraps any [`PersistedEventRepository`] and intercepts:
//! - **Write path** (`persist`): encrypts `PersonCaptured` PII fields and `Modified` data fields
//!   for journeys that have an associated subject.
//! - **Read path** (`get_events`, `get_last_events`, `stream_events`): decrypts, or redacts when
//!   the DEK has been deleted (crypto-shredding).
//!
//! # Event-type conventions
//!
//! | Variant | `event_type` string | Payload outer key |
//! |---|---|---|
//! | `JourneyEvent::PersonCaptured` | `"PersonCaptured"` | `"PersonCaptured"` |
//! | `JourneyEvent::Modified` | `"JourneyModified"` | `"Modified"` |
//!
//! The payload outer key is the serde enum variant name (external tagging).

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es::Aggregate;
use cqrs_es::persist::{
    PersistedEventRepository, PersistenceError, ReplayStream, SerializedEvent, SerializedSnapshot,
};
use serde_json::Value;
use uuid::Uuid;

use super::cipher::{EncryptedPayload, PiiCipher};
use super::key_store::KeyStore;
use super::subject_mapping::SubjectMapping;

// ── Event-type strings (from DomainEvent::event_type()) ───────────────────

const PERSON_CAPTURED: &str = "PersonCaptured";
const JOURNEY_MODIFIED: &str = "JourneyModified";

// ── Payload outer keys (serde external enum tagging) ──────────────────────

const PC_KEY: &str = "PersonCaptured";
const MOD_KEY: &str = "Modified";

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
    #[error("Subject mapping error: {0}")]
    SubjectMapping(#[from] super::subject_mapping::MappingError),
}

impl From<RepoError> for PersistenceError {
    fn from(e: RepoError) -> Self {
        PersistenceError::UnknownError(Box::new(e))
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
        self.events.lock().unwrap().clone()
    }
}

impl PersistedEventRepository for InMemoryEventRepository {
    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        Ok(self
            .events
            .lock()
            .unwrap()
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
        let events = self.events.lock().unwrap();
        let filtered: Vec<SerializedEvent> = events
            .iter()
            .filter(|e| e.aggregate_id == aggregate_id)
            .cloned()
            .collect();
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
        let mut stored = self.events.lock().unwrap();
        stored.extend_from_slice(events);
        Ok(())
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        let events: Vec<SerializedEvent> = self
            .events
            .lock()
            .unwrap()
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
        let events: Vec<SerializedEvent> = self.events.lock().unwrap().clone();
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
/// - **`PersonCaptured`**: records the journey → subject mapping; encrypts `name`, `email`,
///   and `phone` into a single `encrypted_pii` blob. `subject_id` is kept in plaintext.
/// - **`JourneyModified`**: if the journey has an associated subject, encrypts the entire
///   `data` field. Otherwise the event is stored in plaintext.
/// - All other events: passed through unmodified.
///
/// # Read path
/// - **Encrypted `PersonCaptured`**: decrypted when the DEK is available; fields redacted
///   to `"[redacted]"` / `null` when the key has been deleted (subject forgotten).
/// - **Encrypted `JourneyModified`**: decrypted when the DEK is available; `data` set to `{}`
///   after shredding.
/// - Events without an `encrypted_pii` / `encrypted_data` sentinel (legacy plaintext events):
///   returned unmodified.
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    pub(crate) inner: R,
    key_store: Arc<dyn KeyStore>,
    subject_mapping: Arc<dyn SubjectMapping>,
    cipher: Arc<PiiCipher>,
}

impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Create a new crypto-shredding repository wrapping `inner`.
    pub fn new(
        inner: R,
        key_store: Arc<dyn KeyStore>,
        subject_mapping: Arc<dyn SubjectMapping>,
        cipher: PiiCipher,
    ) -> Self {
        Self {
            inner,
            key_store,
            subject_mapping,
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
                JOURNEY_MODIFIED => self.maybe_encrypt_modified(event).await?,
                _ => event,
            };
            out.push(event);
        }
        Ok(out)
    }

    /// Encrypts the PII fields of a `PersonCaptured` event and records the
    /// journey → subject association.
    ///
    /// If `subject_id` is absent from the payload (pre-Phase-3 legacy event),
    /// the event is returned unmodified.
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

        // Record the journey → subject mapping so Modified events can be encrypted.
        self.subject_mapping
            .associate(&event.aggregate_id, &subject_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

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
                "subject_id":    subject_id_str,
                "encrypted_pii": BASE64.encode(&encrypted.ciphertext),
                "nonce":         BASE64.encode(&encrypted.nonce),
            }
        });

        Ok(event)
    }

    /// Encrypts the `data` field of a `JourneyModified` event if the journey has an
    /// associated subject. Events without a mapping are stored in plaintext.
    async fn maybe_encrypt_modified(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        let subject_id = self
            .subject_mapping
            .get_subject(&event.aggregate_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        let Some(subject_id) = subject_id else {
            return Ok(event); // No subject yet — pass through.
        };

        let dek = self
            .key_store
            .get_or_create_key(&subject_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        let step = event.payload[MOD_KEY]["step"].clone();
        let data = event.payload[MOD_KEY]["data"].clone();

        let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
        let plaintext = serde_json::to_vec(&data)?;
        let encrypted = self.cipher.encrypt(&dek, &plaintext, &aad);

        event.payload = serde_json::json!({
            "Modified": {
                "step": step,
                "data": {
                    "encrypted_data": BASE64.encode(&encrypted.ciphertext),
                    "nonce":          BASE64.encode(&encrypted.nonce),
                }
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
                JOURNEY_MODIFIED => self.maybe_decrypt_modified(event).await?,
                _ => event,
            };
            out.push(event);
        }
        Ok(out)
    }

    /// Decrypts a `PersonCaptured` event, or redacts it if the DEK has been deleted.
    ///
    /// Events without an `encrypted_pii` sentinel (legacy plaintext) are returned as-is.
    async fn decrypt_person_captured(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        // No sentinel → legacy plaintext event, pass through.
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

    /// Decrypts the `data` field of a `JourneyModified` event, or sets `data` to `{}` if
    /// the DEK has been deleted or no subject mapping exists.
    ///
    /// Events without an `encrypted_data` sentinel in `data` are returned as-is.
    async fn maybe_decrypt_modified(
        &self,
        mut event: SerializedEvent,
    ) -> Result<SerializedEvent, PersistenceError> {
        // No sentinel → plaintext event, pass through.
        if event.payload[MOD_KEY]["data"]
            .get("encrypted_data")
            .is_none()
        {
            return Ok(event);
        }

        let step = event.payload[MOD_KEY]["step"].clone();

        let subject_id = self
            .subject_mapping
            .get_subject(&event.aggregate_id)
            .await
            .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

        let dek = match subject_id {
            Some(sid) => self
                .key_store
                .get_key(&sid)
                .await
                .map_err(|e| PersistenceError::from(RepoError::from(e)))?,
            None => None,
        };

        match dek {
            Some(dek) => {
                let ciphertext = BASE64
                    .decode(
                        event.payload[MOD_KEY]["data"]["encrypted_data"]
                            .as_str()
                            .unwrap_or_default(),
                    )
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;
                let nonce = BASE64
                    .decode(
                        event.payload[MOD_KEY]["data"]["nonce"]
                            .as_str()
                            .unwrap_or_default(),
                    )
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

                let encrypted = EncryptedPayload { ciphertext, nonce };
                let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
                let plaintext = self
                    .cipher
                    .decrypt(&dek, &encrypted, &aad)
                    .map_err(|e| PersistenceError::from(RepoError::from(e)))?;

                let data: Value = serde_json::from_slice(&plaintext)?;

                event.payload = serde_json::json!({
                    "Modified": {
                        "step": step,
                        "data": data,
                    }
                });
            }
            None => {
                // Key deleted or no mapping — data is permanently gone.
                event.payload = serde_json::json!({
                    "Modified": {
                        "step": step,
                        "data": {},
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
        // Snapshots are not encrypted in this phase (aggregate state contains no PII).
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
        // A production-ready implementation would need the inner store to expose raw
        // `SerializedEvent` access so each event can be decrypted before being pushed to the
        // returned stream.
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
    use crate::crypto::subject_mapping::{InMemorySubjectMapping, SubjectMapping};
    use crate::domain::journey::Journey;

    use super::{CryptoShreddingEventRepository, InMemoryEventRepository};

    // ── Helpers ───────────────────────────────────────────────────────────

    /// Build a repo backed by in-memory test doubles.
    /// Use [`make_repo_with_parts`] when you need handles to the key store or mapping.
    fn make_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
        let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
        let subject_mapping: Arc<dyn SubjectMapping> = Arc::new(InMemorySubjectMapping::new());
        let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
        CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            key_store,
            subject_mapping,
            cipher,
        )
    }

    /// Build a repo and return handles to the key store and subject mapping for inspection.
    fn make_repo_with_parts() -> (
        CryptoShreddingEventRepository<InMemoryEventRepository>,
        Arc<InMemoryKeyStore>,
        Arc<InMemorySubjectMapping>,
    ) {
        let key_store = Arc::new(InMemoryKeyStore::new());
        let subject_mapping = Arc::new(InMemorySubjectMapping::new());
        let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            Arc::clone(&subject_mapping) as Arc<dyn SubjectMapping>,
            cipher,
        );
        (repo, key_store, subject_mapping)
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
                    "subject_id": subject_id.to_string(),
                    "name":       "Alice Smith",
                    "email":      "alice@example.com",
                    "phone":      "+44-7700-900000"
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

        // subject_id remains in plaintext.
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
        );
    }

    #[tokio::test]
    async fn test_person_captured_without_subject_id_passes_through_on_write() {
        // A PersonCaptured that pre-dates Phase 3 (no subject_id field) must not be encrypted.
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

    #[tokio::test]
    async fn test_person_captured_records_subject_mapping() {
        let (repo, _ks, subject_mapping) = make_repo_with_parts();
        let aggregate_id = "journey-mapping";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let mapped = subject_mapping.get_subject(aggregate_id).await.unwrap();
        assert_eq!(
            mapped,
            Some(subject_id),
            "journey → subject mapping must be recorded after PersonCaptured"
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
        assert!(
            pc.get("encrypted_pii").is_none(),
            "encrypted_pii must not appear after decryption"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_person_captured_when_key_deleted() {
        let (repo, key_store, _sm) = make_repo_with_parts();
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

    // ── JourneyModified — write path ──────────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_modified_when_subject_is_mapped() {
        let repo = make_repo();
        let aggregate_id = "journey-mod-encrypt";
        let subject_id = Uuid::new_v4();

        // Establish mapping via PersonCaptured.
        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        repo.persist::<Journey>(&[modified_event(aggregate_id, 2)], None)
            .await
            .unwrap();

        let raw = repo.inner.all_events();
        let data = &raw[1].payload["Modified"]["data"];

        assert!(
            data.get("encrypted_data").is_some(),
            "data must be encrypted when a subject mapping exists"
        );
        assert!(data.get("nonce").is_some(), "nonce must be present");
        assert!(
            data.get("tripType").is_none(),
            "tripType must not appear in plaintext"
        );
        assert_eq!(
            raw[1].payload["Modified"]["step"].as_str().unwrap(),
            "search",
            "step must remain in plaintext"
        );
    }

    #[tokio::test]
    async fn test_persist_does_not_encrypt_modified_without_subject() {
        let repo = make_repo();
        let aggregate_id = "journey-mod-plain";

        // No PersonCaptured first → no subject mapping.
        let event = modified_event(aggregate_id, 1);
        let original_data = event.payload["Modified"]["data"].clone();

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner.all_events();
        let data = &raw[0].payload["Modified"]["data"];

        assert!(
            data.get("encrypted_data").is_none(),
            "data must NOT be encrypted when there is no subject mapping"
        );
        assert_eq!(*data, original_data, "data must be stored unmodified");
    }

    // ── JourneyModified — read path ───────────────────────────────────────

    #[tokio::test]
    async fn test_get_events_decrypts_modified() {
        let repo = make_repo();
        let aggregate_id = "journey-mod-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(&[modified_event(aggregate_id, 2)], None)
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let mod_event = events
            .iter()
            .find(|e| e.event_type == "JourneyModified")
            .expect("JourneyModified must be present");
        let data = &mod_event.payload["Modified"]["data"];

        assert_eq!(data["tripType"].as_str().unwrap(), "round-trip");
        assert_eq!(data["origin"].as_str().unwrap(), "LHR");
        assert_eq!(data["destination"].as_str().unwrap(), "JFK");
        assert!(
            data.get("encrypted_data").is_none(),
            "encrypted_data must not appear after decryption"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_modified_when_key_deleted() {
        let (repo, key_store, _sm) = make_repo_with_parts();
        let aggregate_id = "journey-mod-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(&[modified_event(aggregate_id, 2)], None)
            .await
            .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let mod_event = events
            .iter()
            .find(|e| e.event_type == "JourneyModified")
            .expect("JourneyModified must be present");

        assert_eq!(
            mod_event.payload["Modified"]["data"],
            serde_json::json!({}),
            "data must be empty after shredding"
        );
        assert_eq!(
            mod_event.payload["Modified"]["step"].as_str().unwrap(),
            "search",
            "step must remain readable after shredding"
        );
    }

    #[tokio::test]
    async fn test_plaintext_modified_passes_through_on_read() {
        // A Modified event stored before the crypto layer (no encrypted_data key) must pass
        // through unmodified.
        let repo = make_repo();
        let aggregate_id = "journey-mod-legacy-read";
        let plain_payload = serde_json::json!({
            "Modified": {
                "step": "search",
                "data": { "origin": "CDG" }
            }
        });

        repo.inner
            .persist::<Journey>(
                &[SerializedEvent::new(
                    aggregate_id.to_string(),
                    1,
                    "Journey".to_string(),
                    "JourneyModified".to_string(),
                    "1.0".to_string(),
                    plain_payload.clone(),
                    serde_json::json!({}),
                )],
                None,
            )
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload, plain_payload,
            "legacy plaintext Modified must be returned unmodified"
        );
    }

    // ── Cross-journey shredding ───────────────────────────────────────────

    #[tokio::test]
    async fn test_single_key_deletion_shreds_all_journeys_for_subject() {
        let (repo, key_store, _sm) = make_repo_with_parts();
        let subject_id = Uuid::new_v4();
        let journey_a = "journey-xj-a";
        let journey_b = "journey-xj-b";

        // Same subject captured in two different journeys.
        repo.persist::<Journey>(&[person_captured_event(journey_a, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(&[person_captured_event(journey_b, 1, subject_id)], None)
            .await
            .unwrap();

        // Shred once.
        key_store.delete_key(&subject_id).await.unwrap();

        let events_a = repo.get_events::<Journey>(journey_a).await.unwrap();
        let events_b = repo.get_events::<Journey>(journey_b).await.unwrap();

        assert_eq!(
            events_a[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]",
            "journey A must be redacted after key deletion"
        );
        assert_eq!(
            events_b[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]",
            "journey B must be redacted after key deletion"
        );
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
        repo.persist::<Journey>(&[modified_event(aggregate_id, 2)], None)
            .await
            .unwrap();

        // Fetch only the last event (the Modified).
        let events = repo
            .get_last_events::<Journey>(aggregate_id, 1)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "JourneyModified");
        assert_eq!(
            events[0].payload["Modified"]["data"]["origin"]
                .as_str()
                .unwrap(),
            "LHR"
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
}
