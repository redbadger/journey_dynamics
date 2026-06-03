//! Key Encryption Key (KEK) provider abstraction for DEK wrap/unwrap.
//!
//! # Design
//!
//! A [`KekProvider`] is the single point of contact for everything that touches a KEK:
//! wrapping a newly-generated DEK before it is stored, and unwrapping a stored DEK when
//! it is read back.  Keeping wrap/unwrap behind a trait means the implementation can be
//! swapped between a local in-memory KEK (current behaviour, [`StaticKekProvider`]) and
//! a cloud KMS (`AwsKmsKekProvider`, etc.) without changing any downstream code.
//!
//! # Versioning
//!
//! Every wrapped DEK carries the string id of the KEK version that produced it (stored in
//! `subject_encryption_keys.kek_id`).  This allows multiple KEK versions to coexist in the
//! database during a rotation: older rows continue to unwrap with the version that wrote
//! them while new rows are written with the current primary.
//!
//! The id format is opaque — providers choose whatever stable string makes sense:
//!
//! | Provider | Example id |
//! |---|---|
//! | [`StaticKekProvider`] | `"env:v1"`, `"env:v2"` |
//! | AWS KMS | `"aws-kms:arn:aws:kms:eu-west-2:123:key/abc/versions/1"` |
//! | GCP KMS | `"gcp-kms:projects/p/locations/l/keyRings/r/cryptoKeys/k/cryptoKeyVersions/3"` |
//! | `HashiCorp` Vault Transit | `"vault:transit/keys/journeys:7"` |
//!
//! # Rotation flow (summary)
//!
//! 1. Introduce a new KEK version alongside the current one.
//! 2. Promote the new version to primary — new DEKs are wrapped under it immediately.
//! 3. Re-wrap existing DEKs lazily on read and via the background [`RewrapWorker`](crate::rewrap::RewrapWorker).
//! 4. Once the database contains zero rows for the old `kek_id`, retire it at the vault.
//!
//! See `docs/KEK_ROTATION_DESIGN.md` for the full design.

use std::collections::HashMap;

use async_trait::async_trait;
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::cipher::{CryptoError, KeyMaterial};

// ─────────────────────────────────────────────────────────────────────────────
// WrappedDek
// ─────────────────────────────────────────────────────────────────────────────

/// A DEK as it exists at rest — wrapped ciphertext plus the id of the KEK version
/// that produced the wrap.  Both fields are persisted in `subject_encryption_keys`.
#[derive(Clone, Debug)]
pub struct WrappedDek {
    /// The DEK's own stable identifier (its `key_id` UUID).  Unchanged by re-wraps.
    pub key_id: Uuid,
    /// The id of the KEK version that produced `wrapped_key`.  Changes on re-wrap.
    pub kek_id: String,
    /// Opaque ciphertext bytes produced by the provider's `wrap` implementation.
    pub wrapped_key: Vec<u8>,
}

// ─────────────────────────────────────────────────────────────────────────────
// KekHandle
// ─────────────────────────────────────────────────────────────────────────────

/// A lightweight handle that identifies a specific KEK version and carries any
/// provider-internal state needed to use it.
///
/// The `id` field is the string stored in `subject_encryption_keys.kek_id`.
/// All other fields are provider-specific and opaque to callers.
#[derive(Clone, Debug)]
pub struct KekHandle {
    /// The stable, human-readable id of this KEK version.
    pub id: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// KekError
// ─────────────────────────────────────────────────────────────────────────────

/// Errors produced by [`KekProvider`] operations.
#[derive(Debug, Error)]
pub enum KekError {
    /// The requested KEK version is not known to this provider (may have been retired).
    #[error("Unknown KEK version: {0}")]
    UnknownVersion(String),

    /// The wrap operation failed.
    #[error("Wrap failed: {0}")]
    Wrap(Box<dyn std::error::Error + Send + Sync>),

    /// The unwrap operation failed (corrupt data or wrong KEK version).
    #[error("Unwrap failed: {0}")]
    Unwrap(Box<dyn std::error::Error + Send + Sync>),

    /// A transport / network error communicating with a remote vault.
    #[error("Vault transport error: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync>),
}

impl From<CryptoError> for KekError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::KeyUnwrapFailed => Self::Unwrap(Box::new(e)),
            other => Self::Wrap(Box::new(other)),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// KekProvider trait
// ─────────────────────────────────────────────────────────────────────────────

/// Abstracts all access to Key Encryption Key material.
///
/// Implementations may hold key material locally (e.g. [`StaticKekProvider`]) or
/// delegate to a remote vault where the material never leaves the HSM (e.g.
/// `AwsKmsKekProvider`).  Downstream code must not distinguish between the two.
///
/// The trait is `async` throughout because cloud-KMS implementations make network calls.
/// Local implementations simply return immediately without awaiting anything.
#[async_trait]
pub trait KekProvider: Send + Sync {
    /// Returns a handle to the **current primary** KEK version.
    ///
    /// All new DEK wraps must use this version.  The handle's `id` is what gets stored
    /// in `subject_encryption_keys.kek_id`.
    fn current(&self) -> KekHandle;

    /// Returns a handle for the specified KEK version id, or `None` if that version is
    /// no longer accessible (has been retired from the provider).
    ///
    /// The provider must be able to return handles for every version that still has
    /// wrapped DEKs in the database; retiring a version before all its DEKs have been
    /// re-wrapped will make those DEKs permanently unreadable.
    fn by_id(&self, id: &str) -> Option<KekHandle>;

    /// Wrap (encrypt) a freshly-generated DEK using the supplied KEK handle.
    ///
    /// Callers nearly always pass `provider.current()`.  The background re-wrap worker
    /// pins to `provider.current()` at the start of each sweep so that a second rotation
    /// mid-sweep does not cause it to re-wrap with a stale version.
    ///
    /// # Errors
    ///
    /// Returns [`KekError::UnknownVersion`] if the handle's id is not recognised.
    /// Returns [`KekError::Wrap`] or [`KekError::Transport`] for other failures.
    async fn wrap(&self, kek: &KekHandle, dek: &KeyMaterial) -> Result<WrappedDek, KekError>;

    /// Unwrap (decrypt) a stored DEK using the KEK version recorded on the row.
    ///
    /// The provider looks up the correct key version via `wrapped.kek_id`.
    ///
    /// # Errors
    ///
    /// Returns [`KekError::UnknownVersion`] if the version has been retired.
    /// Returns [`KekError::Unwrap`] if the ciphertext is corrupt or the wrong key is used.
    async fn unwrap(&self, wrapped: &WrappedDek) -> Result<KeyMaterial, KekError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// StaticKekProvider
// ─────────────────────────────────────────────────────────────────────────────

/// A [`KekProvider`] backed by one or more in-memory KEK versions (e.g. loaded from
/// environment variables).  Suitable for development, testing, and deployments where a
/// cloud KMS is not available.
///
/// # Environment-variable schema
///
/// ```text
/// JOURNEY_KEK_PRIMARY=v2
/// JOURNEY_KEK_v1=<base64-encoded 32-byte key>   # still readable for legacy rows
/// JOURNEY_KEK_v2=<base64-encoded 32-byte key>   # used for new wraps
/// ```
///
/// `from_env` reads all variables that match `<PREFIX>_<id>` and uses
/// `<PREFIX>_PRIMARY` to identify the current primary version.
///
/// # Rotation
///
/// To rotate:
/// 1. Add `<PREFIX>_v2=<new key>` and roll it out to all replicas (behaviour unchanged).
/// 2. Set `<PREFIX>_PRIMARY=v2` and roll out (new DEKs use v2; old DEKs still unwrap from v1).
/// 3. Run the re-wrap sweeper until zero rows carry `kek_id = "v1"`.
/// 4. Remove `<PREFIX>_v1` (the old key is no longer needed).
pub struct StaticKekProvider {
    primary: String,
    /// Map from version id to 32-byte AES-KWP key material.
    keks: HashMap<String, Zeroizing<Vec<u8>>>,
}

impl StaticKekProvider {
    /// Build from explicit entries.  `primary` must be a key in `keks`.
    ///
    /// # Errors
    ///
    /// Returns [`KekError::UnknownVersion`] if `primary` is not present in `keks`.
    pub fn new(
        primary: impl Into<String>,
        keks: HashMap<String, Zeroizing<Vec<u8>>>,
    ) -> Result<Self, KekError> {
        let primary = primary.into();
        if !keks.contains_key(&primary) {
            return Err(KekError::UnknownVersion(primary));
        }
        Ok(Self { primary, keks })
    }

    /// Build from environment variables with the given `prefix`.
    ///
    /// Reads:
    /// - `<PREFIX>_PRIMARY` — id of the current primary KEK version (required).
    /// - `<PREFIX>_<id>` for every other variable matching the prefix — raw base64 key
    ///   bytes, must decode to exactly 32 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if `<PREFIX>_PRIMARY` is not set, if no matching key variables are
    /// found, if any value is not valid base64, or if any decoded value is not 32 bytes.
    pub fn from_env(prefix: &str) -> Result<Self, KekError> {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

        let primary_var = format!("{prefix}_PRIMARY");
        let primary = std::env::var(&primary_var).map_err(|_| {
            KekError::Transport(format!("Environment variable {primary_var} is not set").into())
        })?;

        let mut keks: HashMap<String, Zeroizing<Vec<u8>>> = HashMap::new();
        for (key, val) in std::env::vars() {
            // Match variables of the form PREFIX_<something>, but NOT PREFIX_PRIMARY itself.
            let Some(rest) = key.strip_prefix(&format!("{prefix}_")) else {
                continue;
            };
            if rest == "PRIMARY" {
                continue;
            }
            let bytes = BASE64.decode(val.trim()).map_err(|e| {
                KekError::Transport(
                    format!("Environment variable {key} is not valid base64: {e}").into(),
                )
            })?;
            if bytes.len() != 32 {
                return Err(KekError::Transport(
                    format!(
                        "Environment variable {key} must decode to exactly 32 bytes, got {}",
                        bytes.len()
                    )
                    .into(),
                ));
            }
            keks.insert(rest.to_string(), Zeroizing::new(bytes));
        }

        if keks.is_empty() {
            return Err(KekError::Transport(
                format!("No KEK variables found with prefix {prefix}_").into(),
            ));
        }

        Self::new(primary, keks)
    }

    /// Build using a single version — the common case for existing deployments.
    ///
    /// Equivalent to `from_env` but the version id and key bytes are supplied directly.
    /// Useful in tests and in `state.rs` during the initial migration to the new API.
    ///
    /// # Errors
    ///
    /// Returns [`KekError::Wrap`] if `key_bytes` is not exactly 32 bytes.
    pub fn single(id: impl Into<String>, key_bytes: Vec<u8>) -> Result<Self, KekError> {
        if key_bytes.len() != 32 {
            return Err(KekError::Wrap(
                format!("KEK must be exactly 32 bytes, got {}", key_bytes.len()).into(),
            ));
        }
        let id = id.into();
        let mut keks = HashMap::new();
        keks.insert(id.clone(), Zeroizing::new(key_bytes));
        Ok(Self { primary: id, keks })
    }

    /// Expose the set of known version ids — useful for boot-time sanity checks.
    pub fn known_ids(&self) -> impl Iterator<Item = &str> {
        self.keks.keys().map(String::as_str)
    }

    fn kwp_wrap(kek_bytes: &[u8], dek: &KeyMaterial) -> Result<Vec<u8>, KekError> {
        use aes_kw::{KeyInit as KwpKeyInit, KwpAes256};
        let cipher: KwpAes256 = KwpKeyInit::new_from_slice(kek_bytes)
            .expect("KEK is always 32 bytes — validated in StaticKekProvider::new");
        let mut buf = vec![0u8; 40]; // 32-byte DEK → 40 bytes wrapped
        cipher
            .wrap_key(&dek.key, &mut buf)
            .map_err(|e| KekError::Wrap(e.to_string().into()))?;
        Ok(buf)
    }

    fn kwp_unwrap(
        kek_bytes: &[u8],
        key_id: Uuid,
        wrapped_key: &[u8],
    ) -> Result<KeyMaterial, KekError> {
        use aes_kw::{KeyInit as KwpKeyInit, KwpAes256};
        let cipher: KwpAes256 = KwpKeyInit::new_from_slice(kek_bytes)
            .expect("KEK is always 32 bytes — validated in StaticKekProvider::new");
        let buf_len = wrapped_key.len().saturating_sub(8);
        let mut buf = vec![0u8; buf_len];
        cipher
            .unwrap_key(wrapped_key, &mut buf)
            .map_err(|e| KekError::Unwrap(e.to_string().into()))?;
        Ok(KeyMaterial {
            key_id,
            key: Zeroizing::new(buf),
        })
    }
}

#[async_trait]
impl KekProvider for StaticKekProvider {
    fn current(&self) -> KekHandle {
        KekHandle {
            id: self.primary.clone(),
        }
    }

    fn by_id(&self, id: &str) -> Option<KekHandle> {
        if self.keks.contains_key(id) {
            Some(KekHandle { id: id.to_string() })
        } else {
            None
        }
    }

    async fn wrap(&self, kek: &KekHandle, dek: &KeyMaterial) -> Result<WrappedDek, KekError> {
        let kek_bytes = self
            .keks
            .get(&kek.id)
            .ok_or_else(|| KekError::UnknownVersion(kek.id.clone()))?;
        let wrapped_key = Self::kwp_wrap(kek_bytes, dek)?;
        Ok(WrappedDek {
            key_id: dek.key_id,
            kek_id: kek.id.clone(),
            wrapped_key,
        })
    }

    async fn unwrap(&self, wrapped: &WrappedDek) -> Result<KeyMaterial, KekError> {
        let kek_bytes = self
            .keks
            .get(&wrapped.kek_id)
            .ok_or_else(|| KekError::UnknownVersion(wrapped.kek_id.clone()))?;
        Self::kwp_unwrap(kek_bytes, wrapped.key_id, &wrapped.wrapped_key)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cipher::FieldCipher;

    fn make_provider_v1() -> StaticKekProvider {
        StaticKekProvider::single("v1", vec![0x42u8; 32]).unwrap()
    }

    fn make_provider_v1_v2() -> StaticKekProvider {
        let mut keks = HashMap::new();
        keks.insert("v1".to_string(), Zeroizing::new(vec![0x42u8; 32]));
        keks.insert("v2".to_string(), Zeroizing::new(vec![0xDEu8; 32]));
        StaticKekProvider::new("v2", keks).unwrap()
    }

    // ── StaticKekProvider::single ─────────────────────────────────────────────

    #[test]
    fn single_accepts_32_byte_key() {
        assert!(StaticKekProvider::single("v1", vec![0u8; 32]).is_ok());
    }

    #[test]
    fn single_rejects_short_key() {
        assert!(StaticKekProvider::single("v1", vec![0u8; 31]).is_err());
    }

    #[test]
    fn single_rejects_long_key() {
        assert!(StaticKekProvider::single("v1", vec![0u8; 33]).is_err());
    }

    // ── StaticKekProvider::new ────────────────────────────────────────────────

    #[test]
    fn new_rejects_unknown_primary() {
        let mut keks = HashMap::new();
        keks.insert("v1".to_string(), Zeroizing::new(vec![0x42u8; 32]));
        let result = StaticKekProvider::new("v99", keks);
        assert!(
            matches!(result, Err(KekError::UnknownVersion(id)) if id == "v99"),
            "must reject a primary that is not in the keks map"
        );
    }

    // ── current / by_id ───────────────────────────────────────────────────────

    #[test]
    fn current_returns_primary_id() {
        let provider = make_provider_v1();
        assert_eq!(provider.current().id, "v1");
    }

    #[test]
    fn by_id_returns_some_for_known_version() {
        let provider = make_provider_v1_v2();
        assert!(provider.by_id("v1").is_some());
        assert!(provider.by_id("v2").is_some());
    }

    #[test]
    fn by_id_returns_none_for_unknown_version() {
        let provider = make_provider_v1();
        assert!(provider.by_id("v99").is_none());
    }

    #[test]
    fn current_v2_provider_exposes_correct_primary() {
        let provider = make_provider_v1_v2();
        assert_eq!(provider.current().id, "v2");
    }

    // ── wrap / unwrap round-trip ──────────────────────────────────────────────

    #[tokio::test]
    async fn wrap_unwrap_round_trip_single_version() {
        let provider = make_provider_v1();
        let dek = FieldCipher::generate_dek();
        let original_key_id = dek.key_id;
        let original_bytes: Vec<u8> = dek.key.to_vec();

        let kek = provider.current();
        let wrapped = provider.wrap(&kek, &dek).await.unwrap();

        assert_eq!(wrapped.kek_id, "v1");
        assert_eq!(wrapped.key_id, original_key_id);

        let unwrapped = provider.unwrap(&wrapped).await.unwrap();
        assert_eq!(unwrapped.key_id, original_key_id);
        assert_eq!(*unwrapped.key, original_bytes);
    }

    #[tokio::test]
    async fn wrap_with_old_version_unwraps_after_promotion() {
        // Simulate a row that was wrapped under v1 and must still unwrap after v2 becomes primary.
        let provider_v1 = make_provider_v1();
        let dek = FieldCipher::generate_dek();
        let original_bytes: Vec<u8> = dek.key.to_vec();

        let kek_v1 = provider_v1.current();
        let wrapped = provider_v1.wrap(&kek_v1, &dek).await.unwrap();

        // Now the multi-version provider (v2 is primary, v1 still readable).
        let provider_v1_v2 = make_provider_v1_v2();
        let unwrapped = provider_v1_v2.unwrap(&wrapped).await.unwrap();
        assert_eq!(*unwrapped.key, original_bytes);
    }

    #[tokio::test]
    async fn unwrap_fails_with_retired_version() {
        // Simulate the error when the KEK version has been removed from the provider.
        let provider_v1 = make_provider_v1();
        let dek = FieldCipher::generate_dek();
        let kek = provider_v1.current();
        let wrapped = provider_v1.wrap(&kek, &dek).await.unwrap();

        // A provider that only knows about v2 — cannot unwrap a v1 row.
        let provider_v2_only = StaticKekProvider::single("v2", vec![0xDEu8; 32]).unwrap();
        let result = provider_v2_only.unwrap(&wrapped).await;
        assert!(
            matches!(result, Err(KekError::UnknownVersion(_))),
            "must fail when the KEK version has been retired"
        );
    }

    #[tokio::test]
    async fn wrap_fails_with_unknown_kek_handle() {
        let provider = make_provider_v1();
        let dek = FieldCipher::generate_dek();
        let bad_handle = KekHandle {
            id: "v99".to_string(),
        };
        let result = provider.wrap(&bad_handle, &dek).await;
        assert!(
            matches!(result, Err(KekError::UnknownVersion(_))),
            "must fail when the KekHandle id is not in the provider's map"
        );
    }

    #[tokio::test]
    async fn new_dek_wraps_under_primary() {
        // When v2 is primary, new DEKs must be wrapped under v2.
        let provider = make_provider_v1_v2();
        let dek = FieldCipher::generate_dek();
        let kek = provider.current();
        let wrapped = provider.wrap(&kek, &dek).await.unwrap();
        assert_eq!(
            wrapped.kek_id, "v2",
            "new DEKs must be wrapped under the primary"
        );
    }

    // ── from_env ──────────────────────────────────────────────────────────────

    #[test]
    fn from_env_succeeds_with_valid_vars() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

        // Use a test-specific prefix to avoid collisions with real env vars.
        let prefix = "TEST_KEKPROV_VALID";
        // SAFETY: test-only; these tests must not run concurrently (cargo test is
        // single-threaded per binary by default).  The variables are cleaned up
        // before the function returns, so no other thread can observe them.
        unsafe {
            std::env::set_var(format!("{prefix}_PRIMARY"), "v1");
            std::env::set_var(format!("{prefix}_v1"), BASE64.encode(vec![0x42u8; 32]));
        }

        let result = StaticKekProvider::from_env(prefix);
        assert!(
            result.is_ok(),
            "from_env should succeed with valid variables"
        );

        // Clean up.
        unsafe {
            std::env::remove_var(format!("{prefix}_PRIMARY"));
            std::env::remove_var(format!("{prefix}_v1"));
        }
    }

    #[test]
    fn from_env_fails_without_primary_var() {
        let prefix = "TEST_KEKPROV_NOPRIMARY";
        // No _PRIMARY variable set — from_env must fail.
        let result = StaticKekProvider::from_env(prefix);
        assert!(
            result.is_err(),
            "from_env should fail when _PRIMARY is missing"
        );
    }

    #[test]
    fn from_env_fails_with_wrong_key_length() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

        let prefix = "TEST_KEKPROV_SHORTKEY";
        // SAFETY: same rationale as from_env_succeeds_with_valid_vars.
        unsafe {
            std::env::set_var(format!("{prefix}_PRIMARY"), "v1");
            std::env::set_var(format!("{prefix}_v1"), BASE64.encode(vec![0u8; 16])); // 16, not 32
        }

        let result = StaticKekProvider::from_env(prefix);
        assert!(
            result.is_err(),
            "from_env should fail when a key is not 32 bytes"
        );

        unsafe {
            std::env::remove_var(format!("{prefix}_PRIMARY"));
            std::env::remove_var(format!("{prefix}_v1"));
        }
    }
}
