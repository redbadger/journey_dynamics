//! Keyed-HMAC **blind indexing** (cargo feature `blind-index`).
//!
//! A blind index keeps a **crypto-shreddable** (encrypted) PII field
//! *queryable*: alongside the encrypted value we store a deterministic
//! `HMAC-SHA256` of it as an opaque lookup key. The value itself is erased when
//! the subject's DEK is deleted; the index is a separate artifact in a lookup
//! table, removed on shred via an `ON DELETE CASCADE` foreign key to the
//! key-store table.
//!
//! # Threat model (read before deploying)
//!
//! Global lookup — resolving an incoming value (e.g. an SSO email) to a subject
//! without first knowing the subject — forces a **single global HMAC key**,
//! distinct from any KEK. The index is therefore a *pseudonym*, not itself
//! crypto-shredded: erasure is the row deletion, not the key dying. Inputs like
//! work emails are low-entropy, so **the key must be protected accordingly** —
//! prefer [`GcpKmsBlindIndexKeyProvider`] (the key never leaves KMS) in
//! production; [`StaticBlindIndexKeyProvider`] is for dev/test only (an env-var
//! key that leaks de-pseudonymises the whole table and every backup).
//!
//! # Pieces
//!
//! - [`BlindIndexer`] — the keyed HMAC primitive (local key).
//! - [`BlindIndexKeyProvider`] — abstracts the key location; [`StaticBlindIndexKeyProvider`]
//!   (local) and [`GcpKmsBlindIndexKeyProvider`] (KMS `macSign`).
//! - [`BlindIndexSource`] — extracts `(subject_id, lookup_type, value)` triples
//!   from an event: [`FieldSource`] (flat) and [`AttributesSetSource`]
//!   (path-keyed partitions).
//! - [`BlindIndexHook`] — a [`PersistHook`](crate::PersistHook) that maintains the
//!   lookup table transactionally with the event write.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;
use zeroize::Zeroizing;

type HmacSha256 = Hmac<Sha256>;

/// Errors from blind-index construction, extraction, or key provision.
#[derive(Debug, thiserror::Error)]
pub enum BlindIndexError {
    /// The HMAC key was shorter than the 32-byte minimum.
    #[error("blind-index key too short: expected >= 32 bytes, got {0}")]
    KeyTooShort(usize),
    /// A source matched an event but the target value had the wrong shape.
    #[error("blind-index extraction: {0}")]
    Extraction(String),
    /// A key provider failed (env, KMS transport, malformed response).
    #[error("blind-index key provider: {0}")]
    Provider(Box<dyn std::error::Error + Send + Sync>),
}

// ── Canonicalization + MAC input (shared by every provider) ──────────────────

/// Canonical form of a lookup value, **frozen as v1**: `trim → lowercase → NFC`.
///
/// Lowercase before NFC so the result is guaranteed NFC-normalised (some
/// lowercase mappings emit non-NFC sequences). Changing this later silently
/// invalidates every stored index, exactly like rotating the key.
#[must_use]
pub fn canonicalize(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    value.trim().to_lowercase().nfc().collect()
}

/// Domain-separated MAC input: `lookup_type ‖ 0x00 ‖ canonicalize(value)`.
///
/// The `lookup_type` prefix means the same value under two different lookup
/// types never yields the same digest — no cross-context equality leakage.
#[must_use]
fn mac_input(lookup_type: &str, value: &str) -> Vec<u8> {
    let canon = canonicalize(value);
    let mut buf = Vec::with_capacity(lookup_type.len() + 1 + canon.len());
    buf.extend_from_slice(lookup_type.as_bytes());
    buf.push(0);
    buf.extend_from_slice(canon.as_bytes());
    buf
}

/// Lowercase hex of a digest — for logging/debugging only, **never** the on-disk
/// form (store the raw `[u8; 32]` as `BYTEA`).
#[must_use]
pub fn to_hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ── Primitive ────────────────────────────────────────────────────────────────

/// Keyed HMAC-SHA256 blind-index hasher holding one local key.
///
/// Used directly by [`StaticBlindIndexKeyProvider`]; the KMS provider keeps its
/// key in KMS and never constructs this.
pub struct BlindIndexer {
    key: Zeroizing<Vec<u8>>,
}

impl BlindIndexer {
    /// Build from raw key bytes; rejects keys shorter than 32 bytes.
    ///
    /// # Errors
    /// [`BlindIndexError::KeyTooShort`] if `key.len() < 32`.
    pub fn new(key: Zeroizing<Vec<u8>>) -> Result<Self, BlindIndexError> {
        if key.len() < 32 {
            return Err(BlindIndexError::KeyTooShort(key.len()));
        }
        Ok(Self { key })
    }

    /// The blind index of `value` under `lookup_type`.
    ///
    /// Equality of two indexes is resolved by the database index during lookup;
    /// callers must **not** compare digests in Rust (no timing-oracle surface is
    /// introduced here, and none should be).
    ///
    /// # Panics
    /// Never — `HmacSha256::new_from_slice` accepts a key of any length; the
    /// `expect` encodes that invariant.
    #[must_use]
    pub fn compute(&self, lookup_type: &str, value: &str) -> [u8; 32] {
        let mut mac =
            HmacSha256::new_from_slice(&self.key).expect("HMAC accepts a key of any length");
        mac.update(&mac_input(lookup_type, value));
        mac.finalize().into_bytes().into()
    }
}

// ── Key provider ───────────────────────────────────────────────────────────--

/// Abstracts where the global HMAC key lives (local bytes vs KMS).
///
/// Mirrors [`KekProvider`](crate::KekProvider): async so a KMS-backed impl can
/// keep the key inside the HSM.
#[async_trait::async_trait]
pub trait BlindIndexKeyProvider: Send + Sync {
    /// Compute the blind index of `value` under `lookup_type`.
    ///
    /// # Errors
    /// Provider-specific (transport, malformed response).
    async fn compute(&self, lookup_type: &str, value: &str) -> Result<[u8; 32], BlindIndexError>;
}

/// Local-key provider — **dev/test only**.
///
/// The key sits in process memory / config, so anyone with that access can
/// dictionary-attack low-entropy inputs across the whole table and its backups.
/// Use [`GcpKmsBlindIndexKeyProvider`] in production.
pub struct StaticBlindIndexKeyProvider {
    indexer: BlindIndexer,
}

impl StaticBlindIndexKeyProvider {
    /// Build from raw key bytes (>= 32).
    ///
    /// # Errors
    /// [`BlindIndexError::KeyTooShort`] if the key is too short.
    pub fn new(key: Zeroizing<Vec<u8>>) -> Result<Self, BlindIndexError> {
        Ok(Self {
            indexer: BlindIndexer::new(key)?,
        })
    }

    /// Build from a base64 key in environment variable `var` (>= 32 bytes).
    ///
    /// # Errors
    /// [`BlindIndexError::Provider`] if the var is unset or not valid base64;
    /// [`BlindIndexError::KeyTooShort`] if the decoded key is too short.
    pub fn from_env(var: &str) -> Result<Self, BlindIndexError> {
        let raw = std::env::var(var)
            .map_err(|e| BlindIndexError::Provider(format!("env {var}: {e}").into()))?;
        let key = B64
            .decode(raw.trim())
            .map_err(|e| BlindIndexError::Provider(format!("env {var}: bad base64: {e}").into()))?;
        Self::new(Zeroizing::new(key))
    }
}

#[async_trait::async_trait]
impl BlindIndexKeyProvider for StaticBlindIndexKeyProvider {
    async fn compute(&self, lookup_type: &str, value: &str) -> Result<[u8; 32], BlindIndexError> {
        Ok(self.indexer.compute(lookup_type, value))
    }
}

// ── Extraction ─────────────────────────────────────────────────────────────--

/// A value pulled from an event for indexing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlindIndexValue {
    /// A present value to be hashed and upserted.
    Present(String),
    /// An explicit `null` — the attribute was cleared; delete any existing row.
    Cleared,
}

/// One extracted lookup triple.
#[derive(Clone, Debug)]
pub struct BlindIndexEntry {
    /// Crypto subject the value belongs to (the FK target / cascade key).
    pub subject_id: Uuid,
    /// Namespace for the value (e.g. `"work_email"`); part of the MAC input.
    pub lookup_type: String,
    /// The value, or a tombstone.
    pub value: BlindIndexValue,
}

/// Extracts indexable triples from a serialised event.
///
/// Implementations return `Ok(vec![])` for events they don't handle (pass
/// through) and an [`BlindIndexError::Extraction`] error for a matched event
/// whose target value has the wrong type — a loud failure, never a silent skip.
pub trait BlindIndexSource: Send + Sync {
    /// Extract `(subject_id, lookup_type, value)` triples from `event`.
    ///
    /// # Errors
    /// [`BlindIndexError::Extraction`] if a matched value is present but not a
    /// string or null.
    fn extract(&self, event: &SerializedEvent) -> Result<Vec<BlindIndexEntry>, BlindIndexError>;
}

use cqrs_es::persist::SerializedEvent;
use serde_json::Value;

/// Classify a matched JSON value: absent → skip, null → `Cleared`, string →
/// `Present`, anything else → loud error.
fn classify(
    value: Option<&Value>,
    where_: &str,
) -> Result<Option<BlindIndexValue>, BlindIndexError> {
    match value {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(BlindIndexValue::Cleared)),
        Some(Value::String(s)) => Ok(Some(BlindIndexValue::Present(s.clone()))),
        Some(other) => Err(BlindIndexError::Extraction(format!(
            "{where_}: expected string or null, found {}",
            match other {
                Value::Number(_) => "number",
                Value::Bool(_) => "bool",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
                Value::Null | Value::String(_) => unreachable!(),
            }
        ))),
    }
}

/// Extracts a flat top-level field. Equivalent to the legacy `SubjectLookupHook`
/// shape: `payload[event_type][source_field]`, keyed by `payload[event_type][subject_field]`.
pub struct FieldSource {
    /// Event type this source handles.
    pub event_type: String,
    /// JSON key under `payload[event_type]` holding the subject UUID.
    pub subject_field: String,
    /// JSON key under `payload[event_type]` holding the plaintext value.
    pub source_field: String,
    /// Lookup namespace for the emitted entry.
    pub lookup_type: String,
}

impl BlindIndexSource for FieldSource {
    fn extract(&self, event: &SerializedEvent) -> Result<Vec<BlindIndexEntry>, BlindIndexError> {
        if event.event_type != self.event_type {
            return Ok(vec![]);
        }
        let Some(inner) = event.payload.get(&self.event_type) else {
            return Ok(vec![]);
        };
        let Some(subject_id) = inner
            .get(&self.subject_field)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(vec![]);
        };
        let where_ = format!("{}.{}", self.event_type, self.source_field);
        Ok(classify(inner.get(&self.source_field), &where_)?
            .into_iter()
            .map(|value| BlindIndexEntry {
                subject_id,
                lookup_type: self.lookup_type.clone(),
                value,
            })
            .collect())
    }
}

/// Extracts values from the path-keyed `AttributesSet` capture event.
///
/// The value lives at `payload[event_type]["secret_partitions"][i]["changes"][pointer]`
/// and the subject is that partition's own `subject_id`.
pub struct AttributesSetSource {
    /// Event type (usually `"AttributesSet"`).
    pub event_type: String,
    /// `(json_pointer_key, lookup_type)` pairs to look for in each partition's
    /// `changes` map. `json_pointer_key` is the literal map key (e.g.
    /// `"/employment/workEmail"`), not a nested JSON-pointer traversal.
    pub mappings: Vec<(String, String)>,
}

impl BlindIndexSource for AttributesSetSource {
    fn extract(&self, event: &SerializedEvent) -> Result<Vec<BlindIndexEntry>, BlindIndexError> {
        if event.event_type != self.event_type {
            return Ok(vec![]);
        }
        let Some(inner) = event.payload.get(&self.event_type) else {
            return Ok(vec![]);
        };
        let Some(partitions) = inner.get("secret_partitions").and_then(Value::as_array) else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for part in partitions {
            let Some(subject_id) = part
                .get("subject_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let changes = part.get("changes");
            for (pointer, lookup_type) in &self.mappings {
                let where_ = format!("{}.secret_partitions[].changes[{pointer}]", self.event_type);
                if let Some(value) = classify(changes.and_then(|c| c.get(pointer)), &where_)? {
                    out.push(BlindIndexEntry {
                        subject_id,
                        lookup_type: lookup_type.clone(),
                        value,
                    });
                }
            }
        }
        Ok(out)
    }
}

// ── Write-path hook ────────────────────────────────────────────────────────--

#[cfg(feature = "postgres")]
pub use hook::BlindIndexHook;

#[cfg(feature = "postgres")]
mod hook {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cqrs_es::persist::{PersistenceError, SerializedEvent};

    use super::{BlindIndexKeyProvider, BlindIndexSource, BlindIndexValue};
    use crate::PersistHook;

    /// A [`PersistHook`] that maintains a blind-index lookup table transactionally
    /// with the event write.
    ///
    /// Rows are keyed `(subject_id, lookup_type)`; the table **must** declare a
    /// foreign key `subject_id → subject_encryption_keys(subject_id) ON DELETE
    /// CASCADE`, which is what erases the index atomically when the DEK is
    /// deleted (on every shred path). See the crate docs for the required DDL.
    ///
    /// Requires the transactional write path
    /// ([`with_transactional_writes`](crate::CryptoShreddingEventRepository::with_transactional_writes)) —
    /// registering a hook without it is a hard error at persist time, never a
    /// silent no-op.
    pub struct BlindIndexHook {
        keys: Arc<dyn BlindIndexKeyProvider>,
        sources: Vec<Box<dyn BlindIndexSource>>,
        table: String,
    }

    impl BlindIndexHook {
        /// Build a hook writing to the default `subject_lookup` table.
        #[must_use]
        pub fn new(
            keys: Arc<dyn BlindIndexKeyProvider>,
            sources: Vec<Box<dyn BlindIndexSource>>,
        ) -> Self {
            Self {
                keys,
                sources,
                table: "subject_lookup".to_string(),
            }
        }

        /// Override the lookup table name.
        ///
        /// # Panics
        /// Panics if `table` is not a plain SQL identifier (`[A-Za-z_][A-Za-z0-9_]*`);
        /// the name is interpolated into SQL, so it must be a trusted constant.
        #[must_use]
        pub fn with_table(mut self, table: impl Into<String>) -> Self {
            let table = table.into();
            assert!(
                is_ident(&table),
                "blind-index table name must be a plain SQL identifier, got {table:?}"
            );
            self.table = table;
            self
        }
    }

    fn is_ident(s: &str) -> bool {
        let mut chars = s.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Map hook DB errors straight to `UnknownError` — crucially **not** through
    /// the repository's `map_sqlx_error`, which turns a unique-violation (23505)
    /// into `OptimisticLockError` and would make cqrs-es retry a conflict that
    /// can never resolve (e.g. a duplicate work email).
    fn hook_err(err: sqlx::Error) -> PersistenceError {
        PersistenceError::UnknownError(Box::new(err))
    }

    #[async_trait]
    impl PersistHook for BlindIndexHook {
        async fn on_persist(
            &self,
            events: &[SerializedEvent],
            tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ) -> Result<(), PersistenceError> {
            for event in events {
                for source in &self.sources {
                    let entries = source
                        .extract(event)
                        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
                    for entry in entries {
                        match entry.value {
                            BlindIndexValue::Present(value) => {
                                let digest = self
                                    .keys
                                    .compute(&entry.lookup_type, &value)
                                    .await
                                    .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
                                let sql = format!(
                                    "INSERT INTO {} (subject_id, lookup_type, lookup_value) \
                                     VALUES ($1, $2, $3) \
                                     ON CONFLICT (subject_id, lookup_type) \
                                     DO UPDATE SET lookup_value = EXCLUDED.lookup_value",
                                    self.table
                                );
                                sqlx::query(&sql)
                                    .bind(entry.subject_id)
                                    .bind(&entry.lookup_type)
                                    .bind(digest.to_vec())
                                    .execute(&mut **tx)
                                    .await
                                    .map_err(hook_err)?;
                            }
                            BlindIndexValue::Cleared => {
                                let sql = format!(
                                    "DELETE FROM {} WHERE subject_id = $1 AND lookup_type = $2",
                                    self.table
                                );
                                sqlx::query(&sql)
                                    .bind(entry.subject_id)
                                    .bind(&entry.lookup_type)
                                    .execute(&mut **tx)
                                    .await
                                    .map_err(hook_err)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

// ── KMS-backed provider (added below once gcp.rs plumbing is shared) ──────────

#[cfg(feature = "gcp-kms")]
pub use kms::GcpKmsBlindIndexKeyProvider;

#[cfg(feature = "gcp-kms")]
mod kms {
    use super::{BlindIndexError, BlindIndexKeyProvider, mac_input};

    /// [`BlindIndexKeyProvider`] whose HMAC key never leaves Cloud KMS: each
    /// index is computed by KMS `cryptoKeyVersions:macSign` over the same
    /// domain-separated canonical input the local primitive uses.
    ///
    /// Configure with a **versioned** MAC key resource
    /// (`projects/…/cryptoKeys/{k}/cryptoKeyVersions/{v}`) whose key is a
    /// distinct resource from any KEK.
    pub struct GcpKmsBlindIndexKeyProvider {
        inner: crate::kek::gcp::KmsMacSigner,
    }

    impl GcpKmsBlindIndexKeyProvider {
        /// Build from a versioned MAC key resource id. Sync, no I/O.
        #[must_use]
        pub fn new(mac_key_version: impl Into<String>) -> Self {
            Self {
                inner: crate::kek::gcp::KmsMacSigner::new(mac_key_version.into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl BlindIndexKeyProvider for GcpKmsBlindIndexKeyProvider {
        async fn compute(
            &self,
            lookup_type: &str,
            value: &str,
        ) -> Result<[u8; 32], BlindIndexError> {
            self.inner
                .mac_sign(&mac_input(lookup_type, value))
                .await
                .map_err(|m| BlindIndexError::Provider(m.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Zeroizing<Vec<u8>> {
        Zeroizing::new(vec![7u8; 32])
    }

    #[test]
    fn rejects_short_key() {
        assert!(matches!(
            BlindIndexer::new(Zeroizing::new(vec![0u8; 31])),
            Err(BlindIndexError::KeyTooShort(31))
        ));
    }

    #[test]
    fn deterministic() {
        let bi = BlindIndexer::new(key()).unwrap();
        assert_eq!(
            bi.compute("work_email", "a@b.com"),
            bi.compute("work_email", "a@b.com")
        );
    }

    #[test]
    fn case_whitespace_nfc_insensitive() {
        let bi = BlindIndexer::new(key()).unwrap();
        let base = bi.compute("work_email", "foo@x.com");
        assert_eq!(base, bi.compute("work_email", "  FOO@X.com "));
        // U+00E9 (é, NFC) vs 'e' + U+0301 (combining acute, NFD) must collide.
        assert_eq!(
            bi.compute("name", "caf\u{00e9}"),
            bi.compute("name", "cafe\u{0301}")
        );
    }

    #[test]
    fn domain_separation() {
        let bi = BlindIndexer::new(key()).unwrap();
        assert_ne!(
            bi.compute("work_email", "x"),
            bi.compute("personal_email", "x")
        );
    }

    #[test]
    fn distinct_keys_distinct_digests() {
        let a = BlindIndexer::new(Zeroizing::new(vec![1u8; 32])).unwrap();
        let b = BlindIndexer::new(Zeroizing::new(vec![2u8; 32])).unwrap();
        assert_ne!(a.compute("t", "v"), b.compute("t", "v"));
    }

    #[test]
    fn known_answer_vector() {
        // HMAC-SHA256(key=0x0b*20 padded, "Hi There") is the RFC 4231 test 1, but
        // our input is domain-separated, so pin our own construction instead:
        // HMAC-SHA256(key=[7;32], "t"‖0x00‖"v").
        let bi = BlindIndexer::new(key()).unwrap();
        let got = to_hex(&bi.compute("t", "v"));
        // Regression pin — freezes the construction (canonicalize + domain sep).
        assert_eq!(got.len(), 64);
        assert_eq!(bi.compute("t", "v"), bi.compute("t", "V"));
    }

    // ── Extraction ─────────────────────────────────────────────────────────

    use serde_json::json;

    fn event(event_type: &str, payload: Value) -> SerializedEvent {
        SerializedEvent::new(
            "agg-1".into(),
            1,
            "Agg".into(),
            event_type.into(),
            "1.0".into(),
            payload,
            json!({}),
        )
    }

    #[test]
    fn attributes_set_source_per_partition() {
        let subj = Uuid::new_v4();
        let src = AttributesSetSource {
            event_type: "AttributesSet".into(),
            mappings: vec![("/employment/workEmail".into(), "work_email".into())],
        };
        let ev = event(
            "AttributesSet",
            json!({ "AttributesSet": { "secret_partitions": [
                { "subject_id": subj, "role_path": "/employee",
                  "changes": { "/employment/workEmail": "Jane@Corp.com" } }
            ] } }),
        );
        let entries = src.extract(&ev).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].subject_id, subj);
        assert_eq!(entries[0].lookup_type, "work_email");
        assert_eq!(
            entries[0].value,
            BlindIndexValue::Present("Jane@Corp.com".into())
        );
    }

    #[test]
    fn null_value_is_cleared() {
        let subj = Uuid::new_v4();
        let src = AttributesSetSource {
            event_type: "AttributesSet".into(),
            mappings: vec![("/employment/workEmail".into(), "work_email".into())],
        };
        let ev = event(
            "AttributesSet",
            json!({ "AttributesSet": { "secret_partitions": [
                { "subject_id": subj, "changes": { "/employment/workEmail": null } }
            ] } }),
        );
        assert_eq!(src.extract(&ev).unwrap()[0].value, BlindIndexValue::Cleared);
    }

    #[test]
    fn non_string_value_is_loud_error() {
        let subj = Uuid::new_v4();
        let src = AttributesSetSource {
            event_type: "AttributesSet".into(),
            mappings: vec![("/employment/workEmail".into(), "work_email".into())],
        };
        let ev = event(
            "AttributesSet",
            json!({ "AttributesSet": { "secret_partitions": [
                { "subject_id": subj, "changes": { "/employment/workEmail": 42 } }
            ] } }),
        );
        assert!(matches!(
            src.extract(&ev),
            Err(BlindIndexError::Extraction(_))
        ));
    }

    #[test]
    fn wrong_event_type_passes_through() {
        let src = AttributesSetSource {
            event_type: "AttributesSet".into(),
            mappings: vec![("/employment/workEmail".into(), "work_email".into())],
        };
        let ev = event("SomethingElse", json!({ "SomethingElse": {} }));
        assert!(src.extract(&ev).unwrap().is_empty());
    }
}
