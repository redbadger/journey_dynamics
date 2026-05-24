//! Generic transparent PII encryption and GDPR crypto-shredding for `cqrs-es`.
//!
//! # Overview
//!
//! [`CryptoShreddingEventRepository`] wraps any [`PersistedEventRepository`] and
//! intercepts the read and write paths to encrypt and decrypt PII fields.
//!
//! Which event types carry PII, where the subject ID lives, and how to
//! reassemble a payload after encryption or redaction is entirely determined by
//! the [`PiiEventCodec`] implementation supplied at construction time.  The
//! repository itself has no knowledge of any particular domain or event schema.
//!
//! # Write path
//!
//! For each event, [`PiiEventCodec::classify`] is called.  If it returns
//! `Some(PiiFields)` the PII blob is encrypted with AES-256-GCM under the
//! subject's DEK and the payload is replaced with the encrypted form.  Events
//! for which `classify` returns `None` are forwarded to the inner repository
//! unchanged.
//!
//! # Read path
//!
//! For each event, [`PiiEventCodec::extract_encrypted`] is called.  If it
//! returns `Some(EncryptedPiiExtract)` the repository looks up the DEK:
//! - DEK present  → decrypt and call [`PiiEventCodec::reconstruct`].
//! - DEK absent   → call [`PiiEventCodec::redact`] (subject forgotten).
//! - No sentinel  → event is plaintext / legacy, returned as-is.

use std::collections::HashMap;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es::Aggregate;
use cqrs_es::persist::{
    PersistedEventRepository, PersistenceError, ReplayStream, SerializedEvent, SerializedSnapshot,
};
use serde_json::Value;
use uuid::Uuid;

use crate::cipher::{EncryptedPayload, FieldCipher};
#[cfg(feature = "postgres")]
use crate::kek::{KekProvider, WrappedDek};
use crate::key_store::KeyStore;
#[cfg(feature = "postgres")]
use async_trait::async_trait;

// ── PiiEventCodec — types ─────────────────────────────────────────────────────

/// The base64-encoded ciphertext and nonce that the repository passes to
/// [`PiiFields::build_encrypted_payload`] after a successful encryption.
pub struct EncryptedPiiSentinel {
    /// Base64-encoded AES-256-GCM ciphertext (including the 16-byte tag).
    pub ciphertext_b64: String,
    /// Base64-encoded 96-bit (12-byte) AES-GCM nonce.
    pub nonce_b64: String,
}

/// Instructions returned by [`PiiEventCodec::classify`] for a single event on
/// the **write path**.
pub struct PiiFields {
    /// The data-subject identifier — used to look up or create the DEK.
    pub subject_id: Uuid,

    /// The JSON blob of PII fields to encrypt.  The entire value is serialised
    /// to bytes, encrypted, and base64-encoded.  On the read path this same
    /// JSON structure is returned to [`PiiEventCodec::reconstruct`] as
    /// `plaintext_pii`.
    pub plaintext_pii: Value,

    /// Builds the payload that will be persisted.
    ///
    /// The closure receives the [`EncryptedPiiSentinel`] containing the
    /// base64-encoded ciphertext and nonce and returns the complete
    /// `serde_json::Value` to store.  Non-PII fields (e.g. `person_ref`,
    /// `subject_id`) should be preserved by the closure; only the sensitive
    /// fields should be replaced by the sentinel values.
    pub build_encrypted_payload: Box<dyn FnOnce(EncryptedPiiSentinel) -> Value + Send>,
}

/// Encrypted PII extracted from a stored event by [`PiiEventCodec::extract_encrypted`]
/// on the **read path**.
pub struct EncryptedPiiExtract {
    /// The data-subject identifier — used to look up the DEK.
    pub subject_id: Uuid,
    /// Raw (decoded) ciphertext bytes.
    pub ciphertext: Vec<u8>,
    /// Raw (decoded) nonce bytes.
    pub nonce: Vec<u8>,
}

// ── PiiEventCodec — trait ─────────────────────────────────────────────────────

/// Describes how to locate and transform PII within a serialised event payload.
///
/// Implementors encode the domain-specific knowledge of:
/// - which event types carry PII,
/// - where the subject ID lives,
/// - which fields are sensitive and how they are structured,
/// - how to reassemble the payload after encryption or when redacting.
///
/// The trait is split into two sides:
///
/// - **Write path**: [`classify`](PiiEventCodec::classify) — called on the
///   unencrypted event before it is persisted.
/// - **Read path**: [`extract_encrypted`](PiiEventCodec::extract_encrypted),
///   [`reconstruct`](PiiEventCodec::reconstruct), and
///   [`redact`](PiiEventCodec::redact) — called on the stored (encrypted) event
///   when it is loaded.
pub trait PiiEventCodec: Send + Sync {
    /// **Write path.** Inspect an unencrypted event and return encryption
    /// instructions, or `None` if this event type carries no PII and should be
    /// stored verbatim.
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields>;

    /// **Read path.** Extract encrypted PII metadata from a stored (encrypted)
    /// event payload.
    ///
    /// Returns:
    /// - `Some(EncryptedPiiExtract)` when the event type carries PII **and** the
    ///   payload contains encryption sentinels.
    /// - `None` when the event type carries no PII, or when no sentinels are
    ///   present (legacy / plaintext event — pass through unchanged).
    fn extract_encrypted(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract>;

    /// **Read path.** Rebuild the event payload from decrypted PII bytes.
    ///
    /// `event` is the stored encrypted-form event (useful for extracting
    /// plaintext fields such as `person_ref` or `subject_id`).
    /// `plaintext_pii` is the JSON value that was originally supplied as
    /// [`PiiFields::plaintext_pii`] during encryption.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload cannot be reassembled from the decrypted
    /// PII (e.g. a required field is missing or malformed).
    fn reconstruct(
        &self,
        event: &SerializedEvent,
        plaintext_pii: &Value,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>>;

    /// **Read path.** Rebuild the event payload with redacted placeholders.
    ///
    /// Called when the DEK for this subject has been deleted (crypto-shredding).
    /// The PII is permanently irrecoverable; the implementation should return a
    /// payload that clearly signals redaction (e.g. `"[redacted]"` strings or
    /// `null` / empty-object values) while preserving non-PII plaintext fields.
    ///
    /// # Errors
    ///
    /// Returns an error if the redacted payload cannot be constructed.
    fn redact(
        &self,
        event: &SerializedEvent,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>>;
}

// ── CryptoShreddingEventRepository ───────────────────────────────────────────

// ── PersistHook ──────────────────────────────────────────────────────────────

/// Hook called within the transactional persist path.
///
/// Receives the **unencrypted** serialised events and a live Postgres
/// transaction. Implementations can inspect event payloads and perform
/// domain-specific writes (e.g. subject-lookup inserts) that will be
/// committed atomically with the event and DEK inserts.
///
/// If the hook returns an error, the entire transaction is rolled back.
///
/// Only available with the `postgres` feature.
#[cfg(feature = "postgres")]
#[async_trait]
pub trait PersistHook: Send + Sync {
    async fn on_persist(
        &self,
        events: &[SerializedEvent],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), PersistenceError>;
}

// ── CryptoShreddingEventRepository ───────────────────────────────────────────

/// Wraps an inner [`PersistedEventRepository`] and transparently encrypts /
/// decrypts PII-bearing event payloads for GDPR crypto-shredding.
///
/// See the [module-level documentation](self) for a description of the read and
/// write paths.
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    pub(crate) inner: R,
    key_store: Arc<dyn KeyStore>,
    cipher: Arc<FieldCipher>,
    codec: Arc<dyn PiiEventCodec>,
    /// Postgres connection pool for the transactional write path.
    /// When `Some`, `persist` manages its own transaction instead of
    /// delegating to the inner repository.
    #[cfg(feature = "postgres")]
    pool: Option<sqlx::Pool<sqlx::Postgres>>,
    /// KEK provider used to wrap/unwrap DEKs within the transaction.
    /// Required when `pool` is `Some`.
    #[cfg(feature = "postgres")]
    kek_provider: Option<Arc<dyn KekProvider>>,
    /// Hooks called within the transactional persist, in registration order.
    #[cfg(feature = "postgres")]
    persist_hooks: Vec<Arc<dyn PersistHook>>,
}

impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Create a new crypto-shredding repository wrapping `inner`.
    pub fn new(
        inner: R,
        key_store: Arc<dyn KeyStore>,
        cipher: FieldCipher,
        codec: Arc<dyn PiiEventCodec>,
    ) -> Self {
        Self {
            inner,
            key_store,
            cipher: Arc::new(cipher),
            codec,
            #[cfg(feature = "postgres")]
            pool: None,
            #[cfg(feature = "postgres")]
            kek_provider: None,
            #[cfg(feature = "postgres")]
            persist_hooks: Vec::new(),
        }
    }

    /// Returns a reference to the inner [`PersistedEventRepository`].
    ///
    /// Primarily useful in tests to inspect or inject raw (unencrypted) events,
    /// bypassing the crypto layer.
    pub const fn inner(&self) -> &R {
        &self.inner
    }
}

// ── Builder methods (postgres feature) ───────────────────────────────────

#[cfg(feature = "postgres")]
impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Enable the transactional write path.
    ///
    /// When set, `persist` will manage a single Postgres transaction that
    /// atomically commits DEKs, encrypted events, and any hook writes.
    /// The inner repository's `persist` is bypassed for writes; reads
    /// still delegate through it.
    ///
    /// `kek_provider` is used to wrap newly generated DEKs and to unwrap
    /// any DEK created by a concurrent caller within the same transaction.
    #[must_use]
    pub fn with_transactional_writes(
        mut self,
        pool: sqlx::Pool<sqlx::Postgres>,
        kek_provider: Arc<dyn KekProvider>,
    ) -> Self {
        self.pool = Some(pool);
        self.kek_provider = Some(kek_provider);
        self
    }

    /// Register a hook that participates in the persist transaction.
    ///
    /// Hooks receive the **unencrypted** events and a `&mut Transaction`
    /// so they can perform additional writes atomically alongside the
    /// event and DEK inserts. Multiple hooks are called in registration order.
    #[must_use]
    pub fn with_persist_hook(mut self, hook: Arc<dyn PersistHook>) -> Self {
        self.persist_hooks.push(hook);
        self
    }
}

impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    // ── Write helpers ─────────────────────────────────────────────────────────

    async fn encrypt_events(
        &self,
        events: &[SerializedEvent],
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let mut event = event.clone();
            if let Some(pii) = self.codec.classify(&event) {
                let dek = self
                    .key_store
                    .get_or_create_key(&pii.subject_id)
                    .await
                    .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

                // AAD = "<aggregate_id>:<sequence>" — binds ciphertext to this
                // event position, preventing transplant attacks.
                let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
                let plaintext = serde_json::to_vec(&pii.plaintext_pii)?;
                let encrypted = self.cipher.encrypt(&dek, &plaintext, &aad);

                let sentinel = EncryptedPiiSentinel {
                    ciphertext_b64: BASE64.encode(&encrypted.ciphertext),
                    nonce_b64: BASE64.encode(&encrypted.nonce),
                };

                event.payload = (pii.build_encrypted_payload)(sentinel);
            }
            out.push(event);
        }
        Ok(out)
    }

    // ── Read helpers ──────────────────────────────────────────────────────────

    async fn decrypt_events(
        &self,
        events: Vec<SerializedEvent>,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        // ── Pass 1: collect the unique subject IDs that need a DEK. ──────────
        //
        // Fetching all DEKs up front — before decrypting any event — means that
        // if a crypto-shred arrives mid-loop, every event for that subject is
        // consistently redacted rather than producing a torn read (some events
        // decrypted, later ones redacted). It also reduces round-trips from
        // O(events) to O(unique subjects).
        let mut dek_cache: HashMap<Uuid, Option<crate::cipher::KeyMaterial>> = HashMap::new();
        for event in &events {
            if let Some(extract) = self.codec.extract_encrypted(event) {
                if let std::collections::hash_map::Entry::Vacant(e) =
                    dek_cache.entry(extract.subject_id)
                {
                    let dek = self
                        .key_store
                        .get_key(&extract.subject_id)
                        .await
                        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
                    e.insert(dek);
                }
            }
        }

        // ── Pass 2: decrypt using the cached DEKs. ───────────────────────────
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let mut event = event;
            if let Some(extract) = self.codec.extract_encrypted(&event) {
                let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();

                event.payload = match dek_cache.get(&extract.subject_id).and_then(Option::as_ref) {
                    Some(dek) => {
                        let encrypted_payload = EncryptedPayload {
                            ciphertext: extract.ciphertext,
                            nonce: extract.nonce,
                        };
                        let plaintext_bytes = self
                            .cipher
                            .decrypt(dek, &encrypted_payload, &aad)
                            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
                        let plaintext_pii: Value = serde_json::from_slice(&plaintext_bytes)?;
                        self.codec
                            .reconstruct(&event, &plaintext_pii)
                            .map_err(PersistenceError::UnknownError)?
                    }
                    None => self
                        .codec
                        .redact(&event)
                        .map_err(PersistenceError::UnknownError)?,
                };
            }
            out.push(event);
        }
        Ok(out)
    }
}

// ── Transactional helpers (postgres feature) ───────────────────────────────

/// Maps a `sqlx::Error` from the transactional write path to a
/// [`PersistenceError`], translating a PostgreSQL unique-violation
/// (SQLSTATE `23505`) — raised on the `events_pkey` `(aggregate_type,
/// aggregate_id, sequence)` constraint when a concurrent writer commits
/// first — to [`PersistenceError::OptimisticLockError`]. `cqrs-es` then
/// surfaces this as `AggregateError::AggregateConflict`, enabling the
/// standard inline-retry pattern for concurrent writes against the same
/// aggregate. All other database errors fall through to `UnknownError`.
#[cfg(feature = "postgres")]
fn map_sqlx_error(err: sqlx::Error) -> PersistenceError {
    if let sqlx::Error::Database(db_err) = &err {
        if db_err.code().as_deref() == Some("23505") {
            return PersistenceError::OptimisticLockError;
        }
    }
    PersistenceError::UnknownError(Box::new(err))
}

#[cfg(feature = "postgres")]
impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Like [`encrypt_events`], but creates DEKs within the provided
    /// transaction so that key creation is atomic with event persistence.
    async fn encrypt_events_in_tx(
        &self,
        events: &[SerializedEvent],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let mut event = event.clone();
            if let Some(pii) = self.codec.classify(&event) {
                let dek = self.get_or_create_key_in_tx(&pii.subject_id, tx).await?;

                let aad = format!("{}:{}", event.aggregate_id, event.sequence).into_bytes();
                let plaintext = serde_json::to_vec(&pii.plaintext_pii)?;
                let encrypted = self.cipher.encrypt(&dek, &plaintext, &aad);

                let sentinel = EncryptedPiiSentinel {
                    ciphertext_b64: BASE64.encode(&encrypted.ciphertext),
                    nonce_b64: BASE64.encode(&encrypted.nonce),
                };

                event.payload = (pii.build_encrypted_payload)(sentinel);
            }
            out.push(event);
        }
        Ok(out)
    }

    /// Get or create a DEK within a transaction.
    ///
    /// Mirrors [`PostgresKeyStore::get_or_create_key`] exactly, but executes
    /// all SQL against the provided transaction instead of an independent pool
    /// connection, making the DEK INSERT atomic with the caller's transaction.
    async fn get_or_create_key_in_tx(
        &self,
        subject_id: &Uuid,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<crate::cipher::KeyMaterial, PersistenceError> {
        use sqlx::Row as _;

        // Invariant: kek_provider is always Some when pool is Some.
        let provider = self
            .kek_provider
            .as_ref()
            .expect("kek_provider must be set — call with_transactional_writes before persist");

        // Fast path: DEK already exists (committed previously, or inserted
        // by an earlier event in this same batch).
        let existing = sqlx::query(
            "SELECT key_id, wrapped_key, kek_id \
             FROM subject_encryption_keys WHERE subject_id = $1",
        )
        .bind(subject_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_error)?;

        if let Some(row) = existing {
            let material = provider
                .unwrap(&WrappedDek {
                    key_id: row.get("key_id"),
                    kek_id: row.get("kek_id"),
                    wrapped_key: row.get("wrapped_key"),
                })
                .await
                .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
            return Ok(material);
        }

        // Generate a fresh DEK, wrap it under the current primary KEK, and
        // INSERT within the transaction.
        let dek = FieldCipher::generate_dek();
        let kek = provider.current();
        let wrapped = provider
            .wrap(&kek, &dek)
            .await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

        let result = sqlx::query(
            "INSERT INTO subject_encryption_keys \
             (key_id, subject_id, wrapped_key, kek_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (subject_id) DO NOTHING",
        )
        .bind(dek.key_id)
        .bind(subject_id)
        .bind(&wrapped.wrapped_key)
        .bind(&wrapped.kek_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_error)?;

        if result.rows_affected() == 0 {
            // A concurrent transaction inserted first — re-read from the tx.
            let row = sqlx::query(
                "SELECT key_id, wrapped_key, kek_id \
                 FROM subject_encryption_keys WHERE subject_id = $1",
            )
            .bind(subject_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(map_sqlx_error)?;

            let material = provider
                .unwrap(&WrappedDek {
                    key_id: row.get("key_id"),
                    kek_id: row.get("kek_id"),
                    wrapped_key: row.get("wrapped_key"),
                })
                .await
                .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
            Ok(material)
        } else {
            Ok(dek)
        }
    }

    /// Executes the full write in a single Postgres transaction:
    /// DEK creation, event encryption, event INSERT, and hook writes.
    ///
    /// Called by `persist` when `self.pool` is `Some`.
    async fn persist_in_transaction<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        snapshot_update: Option<(String, Value, usize)>,
        pool: &sqlx::Pool<sqlx::Postgres>,
    ) -> Result<(), PersistenceError> {
        let mut tx = pool.begin().await.map_err(map_sqlx_error)?;

        // 1. Encrypt events, creating DEKs inside the transaction.
        let encrypted = self.encrypt_events_in_tx(events, &mut tx).await?;

        // 2. Insert encrypted events.
        for event in &encrypted {
            let payload = serde_json::to_value(&event.payload)?;
            let metadata = serde_json::to_value(&event.metadata)?;
            sqlx::query(
                "INSERT INTO events \
                 (aggregate_type, aggregate_id, sequence, \
                  event_type, event_version, payload, metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&event.aggregate_type)
            .bind(&event.aggregate_id)
            .bind(
                // sequence is a small positive integer; i64::MAX (~9.2e18) is unreachable.
                i64::try_from(event.sequence).expect("sequence fits in i64"),
            )
            .bind(&event.event_type)
            .bind(&event.event_version)
            .bind(&payload)
            .bind(&metadata)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        }

        // 3. Handle snapshot if present.
        //    Always `None` when using `PersistedEventStore::new_event_store`
        //    (the normal configuration), but handled for completeness.
        if let Some((aggregate_id, aggregate, current_snapshot)) = snapshot_update {
            // last_sequence is the event sequence number of the final event in this
            // batch — the point from which the event log must be replayed on top of
            // this snapshot. Defaulting to 0 when the batch is empty is safe because
            // an empty batch with a snapshot update cannot occur in practice.
            let last_sequence = encrypted.last().map_or(0, |e| {
                i64::try_from(e.sequence).expect("sequence fits in i64")
            });

            sqlx::query(
                "INSERT INTO snapshots \
                 (aggregate_type, aggregate_id, last_sequence, current_snapshot, payload) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (aggregate_type, aggregate_id) DO UPDATE \
                 SET last_sequence      = EXCLUDED.last_sequence, \
                     current_snapshot  = EXCLUDED.current_snapshot, \
                     payload           = EXCLUDED.payload",
            )
            .bind(A::TYPE)
            .bind(&aggregate_id)
            .bind(last_sequence)
            .bind(i64::try_from(current_snapshot).expect("snapshot index fits in i64"))
            .bind(&aggregate)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        }

        // 4. Call hooks with the UNENCRYPTED events inside the transaction.
        for hook in &self.persist_hooks {
            hook.on_persist(events, &mut tx).await?;
        }

        // 5. Commit — DEKs, events, and hook writes are now atomically visible.
        tx.commit().await.map_err(map_sqlx_error)?;

        Ok(())
    }
}

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
        // Snapshots are forwarded without encryption or decryption.
        //
        // Known limitation: if your aggregate state contains PII, snapshots will
        // store it in plaintext and crypto-shredding a subject will NOT redact PII
        // embedded in snapshots — only PII in individual events is managed here.
        self.inner.get_snapshot::<A>(aggregate_id).await
    }

    async fn persist<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        snapshot_update: Option<(String, Value, usize)>,
    ) -> Result<(), PersistenceError> {
        // Transactional path: manage the entire write — DEK creation, event
        // encryption, event INSERTs, and hook writes — in one transaction.
        // Only compiled and active when the `postgres` feature is enabled and
        // `with_transactional_writes` has been called.
        #[cfg(feature = "postgres")]
        if let Some(pool) = &self.pool {
            return self
                .persist_in_transaction::<A>(events, snapshot_update, pool)
                .await;
        }

        // Legacy path: encrypt then delegate to the inner repository.
        let encrypted = self.encrypt_events(events).await?;
        self.inner.persist::<A>(&encrypted, snapshot_update).await
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        // Collect decrypted events then feed a new stream.
        let events = self.get_events::<A>(aggregate_id).await?;
        let (mut feed, stream) = ReplayStream::new(events.len().max(1));
        for event in events {
            feed.push(Ok(event)).await?;
        }
        Ok(stream)
    }

    async fn stream_all_events<A: Aggregate>(&self) -> Result<ReplayStream, PersistenceError> {
        // `ReplayStream` only exposes a typed consumer (`next::<A>()`) that
        // immediately deserialises `SerializedEvent` into `EventEnvelope<A>`.
        // Because encrypted payloads would fail deserialisation, there is no
        // point at which this wrapper can intercept and decrypt them.
        //
        // Use `get_events` or `stream_events` per aggregate for decrypted access.
        Err(PersistenceError::UnknownError(
            "`CryptoShreddingEventRepository` does not support `stream_all_events` — \
             use `get_events` or `stream_events` per aggregate instead."
                .into(),
        ))
    }
}

// ── InMemoryEventRepository ───────────────────────────────────────────────────

/// An in-memory [`PersistedEventRepository`] backed by a `Mutex<Vec<SerializedEvent>>`.
///
/// Intended for use in tests. Snapshots are not supported.
///
/// Available when the `testing` Cargo feature is enabled or during `cfg(test)`.
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cqrs_es::{
        DomainEvent,
        event_sink::EventSink,
        persist::{PersistedEventRepository, SerializedEvent},
    };
    use serde_json::Value;
    use uuid::Uuid;

    use crate::cipher::FieldCipher;
    use crate::key_store::{InMemoryKeyStore, KeyStore};

    use super::{
        CryptoShreddingEventRepository, EncryptedPiiExtract, EncryptedPiiSentinel,
        InMemoryEventRepository, PiiEventCodec, PiiFields,
    };

    // ── TestEvent + TestAggregate ─────────────────────────────────────────────

    /// Minimal domain event used only in tests within this crate.
    ///
    /// Serde external tagging means `{ "TestPii": { ... } }` deserialises to
    /// `TestEvent::TestPii { ... }`, matching the payload shapes produced by
    /// [`TestPiiCodec`].
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
    enum TestEvent {
        TestPii { subject_id: String, secret: String },
        TestPlain { data: String },
    }

    impl DomainEvent for TestEvent {
        fn event_type(&self) -> String {
            match self {
                Self::TestPii { .. } => "TestPii".to_string(),
                Self::TestPlain { .. } => "TestPlain".to_string(),
            }
        }
        fn event_version(&self) -> String {
            "1.0".to_string()
        }
    }

    /// Minimal `Aggregate` implementation that satisfies the type parameter on
    /// `PersistedEventRepository` methods.  `TYPE` is `"Test"`, matching the
    /// `aggregate_type` field used in the helper event constructors.
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
            _command: (),
            _services: &(),
            _sink: &EventSink<Self>,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn apply(&mut self, _event: TestEvent) {}
    }

    // ── TestPiiCodec ──────────────────────────────────────────────────────────

    /// A codec that treats `"TestPii"` events as PII-bearing and all others as
    /// plain.
    ///
    /// Payload shape (unencrypted):
    /// ```json
    /// { "TestPii": { "subject_id": "<uuid>", "secret": "<string>" } }
    /// ```
    ///
    /// Payload shape (encrypted):
    /// ```json
    /// { "TestPii": { "subject_id": "<uuid>", "encrypted_pii": "<b64>", "nonce": "<b64>" } }
    /// ```
    struct TestPiiCodec;

    impl PiiEventCodec for TestPiiCodec {
        fn classify(&self, event: &SerializedEvent) -> Option<PiiFields> {
            if event.event_type != "TestPii" {
                return None;
            }

            let subject_id_str = event.payload["TestPii"]["subject_id"].as_str()?.to_string();
            let subject_id = Uuid::parse_str(&subject_id_str).ok()?;
            let plaintext_pii = serde_json::json!({
                "secret": event.payload["TestPii"]["secret"].clone(),
            });

            Some(PiiFields {
                subject_id,
                plaintext_pii,
                build_encrypted_payload: Box::new(
                    move |EncryptedPiiSentinel {
                              ciphertext_b64,
                              nonce_b64,
                          }| {
                        serde_json::json!({
                            "TestPii": {
                                "subject_id":    subject_id_str,
                                "encrypted_pii": ciphertext_b64,
                                "nonce":         nonce_b64,
                            }
                        })
                    },
                ),
            })
        }

        fn extract_encrypted(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
            if event.event_type != "TestPii" {
                return None;
            }
            // No sentinel → legacy plaintext event, pass through.
            event.payload["TestPii"].get("encrypted_pii")?;

            let subject_id =
                Uuid::parse_str(event.payload["TestPii"]["subject_id"].as_str()?).ok()?;
            let ciphertext = BASE64
                .decode(event.payload["TestPii"]["encrypted_pii"].as_str()?)
                .ok()?;
            let nonce = BASE64
                .decode(event.payload["TestPii"]["nonce"].as_str()?)
                .ok()?;

            Some(EncryptedPiiExtract {
                subject_id,
                ciphertext,
                nonce,
            })
        }

        fn reconstruct(
            &self,
            event: &SerializedEvent,
            plaintext_pii: &Value,
        ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
            let subject_id = event.payload["TestPii"]["subject_id"].clone();
            Ok(serde_json::json!({
                "TestPii": {
                    "subject_id": subject_id,
                    "secret":     plaintext_pii["secret"].clone(),
                }
            }))
        }

        fn redact(
            &self,
            event: &SerializedEvent,
        ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
            let subject_id = event.payload["TestPii"]["subject_id"].clone();
            Ok(serde_json::json!({
                "TestPii": {
                    "subject_id": subject_id,
                    "secret":     "[redacted]",
                }
            }))
        }
    }

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
        let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
        let codec = Arc::new(TestPiiCodec);
        CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            key_store,
            FieldCipher::new(),
            codec,
        )
    }

    fn make_repo_with_parts() -> (
        CryptoShreddingEventRepository<InMemoryEventRepository>,
        Arc<InMemoryKeyStore>,
    ) {
        let key_store = Arc::new(InMemoryKeyStore::new());
        let codec = Arc::new(TestPiiCodec);
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            FieldCipher::new(),
            codec,
        );
        (repo, key_store)
    }

    fn pii_event(aggregate_id: &str, sequence: usize, subject_id: Uuid) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Test".to_string(),
            "TestPii".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "TestPii": {
                    "subject_id": subject_id.to_string(),
                    "secret":     "hunter2",
                }
            }),
            serde_json::json!({}),
        )
    }

    fn plain_event(aggregate_id: &str, sequence: usize) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Test".to_string(),
            "TestPlain".to_string(),
            "1.0".to_string(),
            serde_json::json!({ "TestPlain": { "data": "no secrets here" } }),
            serde_json::json!({}),
        )
    }

    // ── Non-PII events ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_non_pii_events_pass_through_unchanged() {
        let repo = make_repo();
        let aggregate_id = "agg-plain-write";
        let event = plain_event(aggregate_id, 1);
        let original_payload = event.payload.clone();

        repo.persist::<TestAggregate>(&[event], None).await.unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(raw.len(), 1);
        assert_eq!(
            raw[0].payload, original_payload,
            "non-PII payload must be stored verbatim"
        );
    }

    #[tokio::test]
    async fn test_non_pii_events_pass_through_on_read() {
        let repo = make_repo();
        let aggregate_id = "agg-plain-read";
        let event = plain_event(aggregate_id, 1);
        let original_payload = event.payload.clone();

        repo.persist::<TestAggregate>(&[event], None).await.unwrap();

        let events = repo
            .get_events::<TestAggregate>(aggregate_id)
            .await
            .unwrap();
        assert_eq!(events[0].payload, original_payload);
    }

    // ── PII encryption on write ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_pii_fields() {
        let repo = make_repo();
        let aggregate_id = "agg-pii-encrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(&[pii_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(raw.len(), 1);

        let inner = &raw[0].payload["TestPii"];
        assert!(
            inner.get("encrypted_pii").is_some(),
            "persisted payload must contain encrypted_pii sentinel"
        );
        assert!(
            inner.get("nonce").is_some(),
            "persisted payload must contain nonce sentinel"
        );
        assert!(
            inner.get("secret").is_none(),
            "plaintext secret must not appear in the persisted payload"
        );
        // subject_id is kept in plaintext for DEK lookup on the read path.
        assert_eq!(
            inner["subject_id"].as_str().unwrap(),
            subject_id.to_string()
        );
    }

    #[tokio::test]
    async fn test_each_persist_produces_unique_ciphertext() {
        let repo = make_repo();
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(&[pii_event("agg-unique-1", 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<TestAggregate>(&[pii_event("agg-unique-2", 1, subject_id)], None)
            .await
            .unwrap();

        let raw = repo.inner.all_events();
        let ct1 = raw[0].payload["TestPii"]["encrypted_pii"].as_str().unwrap();
        let ct2 = raw[1].payload["TestPii"]["encrypted_pii"].as_str().unwrap();
        assert_ne!(
            ct1, ct2,
            "distinct encryptions must produce distinct ciphertexts"
        );
    }

    // ── PII decryption on read ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_events_decrypts_pii() {
        let repo = make_repo();
        let aggregate_id = "agg-pii-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(&[pii_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let events = repo
            .get_events::<TestAggregate>(aggregate_id)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);

        let inner = &events[0].payload["TestPii"];
        assert_eq!(
            inner["secret"].as_str().unwrap(),
            "hunter2",
            "decrypted payload must restore the original plaintext"
        );
        assert!(
            inner.get("encrypted_pii").is_none(),
            "encryption sentinel must not appear in the decrypted payload"
        );
    }

    // ── Redaction on key deletion ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_events_redacts_when_key_deleted() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "agg-pii-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(&[pii_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        // Crypto-shred the subject.
        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo
            .get_events::<TestAggregate>(aggregate_id)
            .await
            .unwrap();
        assert_eq!(
            events[0].payload["TestPii"]["secret"].as_str().unwrap(),
            "[redacted]",
            "PII must be redacted after the DEK is deleted"
        );
    }

    // ── Legacy / plaintext events pass through on read ────────────────────────

    #[tokio::test]
    async fn test_plaintext_pii_event_passes_through_on_read() {
        // An event stored without encryption sentinels (e.g. written before the
        // crypto layer was introduced) must be returned verbatim.
        let repo = make_repo();
        let aggregate_id = "agg-legacy-pii";
        let subject_id = Uuid::new_v4();

        // Bypass the crypto layer and write directly to the inner store.
        let legacy_payload = serde_json::json!({
            "TestPii": {
                "subject_id": subject_id.to_string(),
                "secret":     "legacy secret",
            }
        });
        repo.inner
            .persist::<TestAggregate>(
                &[SerializedEvent::new(
                    aggregate_id.to_string(),
                    1,
                    "Test".to_string(),
                    "TestPii".to_string(),
                    "1.0".to_string(),
                    legacy_payload.clone(),
                    serde_json::json!({}),
                )],
                None,
            )
            .await
            .unwrap();

        let events = repo
            .get_events::<TestAggregate>(aggregate_id)
            .await
            .unwrap();
        assert_eq!(
            events[0].payload, legacy_payload,
            "legacy plaintext event must be returned verbatim"
        );
    }

    // ── Event without subject_id passes through on write ──────────────────────

    #[tokio::test]
    async fn test_pii_event_without_subject_id_passes_through_on_write() {
        // If classify returns None (e.g. missing subject_id) the event is stored
        // unchanged — the codec returns None and the crypto layer is skipped.
        let repo = make_repo();
        let aggregate_id = "agg-no-subject";

        let no_subject_payload = serde_json::json!({
            "TestPii": { "secret": "some secret but no subject_id" }
        });
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Test".to_string(),
            "TestPii".to_string(),
            "1.0".to_string(),
            no_subject_payload.clone(),
            serde_json::json!({}),
        );

        repo.persist::<TestAggregate>(&[event], None).await.unwrap();

        let raw = repo.inner.all_events();
        assert_eq!(
            raw[0].payload, no_subject_payload,
            "event without subject_id must be stored verbatim"
        );
    }

    // ── Key isolation ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_two_subjects_shredded_independently() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "agg-two-subjects";
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        repo.persist::<TestAggregate>(
            &[
                pii_event(aggregate_id, 1, subject_a),
                pii_event(aggregate_id, 2, subject_b),
            ],
            None,
        )
        .await
        .unwrap();

        // Shred only subject_a.
        key_store.delete_key(&subject_a).await.unwrap();

        let events = repo
            .get_events::<TestAggregate>(aggregate_id)
            .await
            .unwrap();

        assert_eq!(
            events[0].payload["TestPii"]["secret"].as_str().unwrap(),
            "[redacted]",
            "subject_a's secret must be redacted"
        );
        assert_eq!(
            events[1].payload["TestPii"]["secret"].as_str().unwrap(),
            "hunter2",
            "subject_b's secret must still be readable"
        );
    }

    // ── get_last_events ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_last_events_decrypts_correctly() {
        let repo = make_repo();
        let aggregate_id = "agg-last-events";
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(
            &[
                plain_event(aggregate_id, 1),
                pii_event(aggregate_id, 2, subject_id),
                plain_event(aggregate_id, 3),
            ],
            None,
        )
        .await
        .unwrap();

        // Fetch the last 2 events (sequences 2 and 3).
        let events = repo
            .get_last_events::<TestAggregate>(aggregate_id, 2)
            .await
            .unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].payload["TestPii"]["secret"].as_str().unwrap(),
            "hunter2"
        );
    }

    // ── stream_events ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_stream_events_returns_decrypted_events() {
        let repo = make_repo();
        let aggregate_id = "agg-stream";
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(
            &[
                plain_event(aggregate_id, 1),
                pii_event(aggregate_id, 2, subject_id),
            ],
            None,
        )
        .await
        .unwrap();

        let mut stream = repo
            .stream_events::<TestAggregate>(aggregate_id)
            .await
            .unwrap();

        // Consume via ReplayStream::next, which deserialises into EventEnvelope<TestAggregate>.
        let _plain = stream
            .next::<TestAggregate>(&[])
            .await
            .expect("stream must yield event 1")
            .expect("event 1 must deserialise without error");

        let pii_envelope = stream
            .next::<TestAggregate>(&[])
            .await
            .expect("stream must yield event 2")
            .expect("event 2 must deserialise without error");

        match pii_envelope.payload {
            TestEvent::TestPii { secret, .. } => {
                assert_eq!(
                    secret, "hunter2",
                    "decrypted secret must round-trip through stream"
                );
            }
            other @ TestEvent::TestPlain { .. } => panic!("unexpected event variant: {other:?}"),
        }
    }

    // ── AAD binding ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_aad_binds_ciphertext_to_event_position() {
        // Directly tamper with the stored event's aggregate_id / sequence to
        // verify that decryption fails (wrong AAD).  This proves the repository
        // actually passes the event position as additional authenticated data.
        let repo = make_repo();
        let aggregate_id = "agg-aad-bind";
        let subject_id = Uuid::new_v4();

        repo.persist::<TestAggregate>(&[pii_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        // Tamper: change the stored sequence so AAD won't match on decrypt.
        {
            let mut events = repo.inner.events.lock().expect("mutex poisoned");
            events[0].sequence = 999;
        }

        let result = repo.get_events::<TestAggregate>(aggregate_id).await;
        assert!(
            result.is_err(),
            "decryption must fail when the event position has been tampered with"
        );
    }
}
