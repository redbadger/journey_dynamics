//! `cqrs-es-crypto` — transparent PII encryption and GDPR crypto-shredding for [`cqrs-es`].
//!
//! # Overview
//!
//! This crate wraps any [`cqrs_es::persist::PersistedEventRepository`] with a
//! crypto-shredding layer that:
//!
//! - **Encrypts** PII fields in designated event types on the write path using
//!   AES-256-GCM, keyed by a per-subject Data Encryption Key (DEK).
//! - **Decrypts** PII fields transparently on the read path when the DEK is present.
//! - **Redacts** PII fields when the DEK has been deleted (crypto-shredded), making
//!   the data permanently irrecoverable without touching individual events.
//!
//! Which event types carry PII, and how their payloads are structured, is defined by
//! the caller through the [`PiiEventCodec`] trait. The crate itself has no knowledge
//! of any particular domain or event schema.
//!
//! # Known limitations
//!
//! - **`stream_all_events`** is not supported: [`CryptoShreddingEventRepository`]
//!   returns an error if called, because the [`cqrs_es::persist::ReplayStream`] API
//!   does not expose raw [`cqrs_es::persist::SerializedEvent`] items for interception.
//!   Use [`CryptoShreddingEventRepository::get_events`] or
//!   [`CryptoShreddingEventRepository::stream_events`] per aggregate instead.
//! - **Snapshots** are not encrypted. If your aggregate state contains PII, snapshots
//!   will store it in plaintext and crypto-shredding will not redact it.
//!
//! # Key management
//!
//! Each subject gets a unique DEK wrapped by a Key Encryption Key (KEK) via
//! [`StaticKekProvider`] (env-var backed) or any [`KekProvider`] implementation.
//! [`PostgresKeyStore`] persists wrapped DEKs in `subject_encryption_keys`.
//! Deleting a row is the shredding operation.  The `kek_id` column records which
//! KEK version wrapped each DEK, enabling zero-downtime KEK rotation.
//!
//! The required DDL is:
//!
//! ```sql
//! CREATE TABLE subject_encryption_keys (
//!     key_id       UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
//!     subject_id   UUID      NOT NULL UNIQUE,
//!     wrapped_key  BYTEA     NOT NULL,
//!     kek_id       TEXT      NOT NULL,
//!     rewrapped_at TIMESTAMP,
//!     created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
//! );
//! CREATE INDEX ON subject_encryption_keys (kek_id);
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use cqrs_es_crypto::{
//!     CryptoShreddingEventRepository, EncryptedPiiSentinel, FieldCipher,
//!     PiiEventCodec, PiiFields, PostgresKeyStore, StaticKekProvider,
//! };
//!
//! // 1. Implement PiiEventCodec for your domain.
//! struct MyCodec;
//! impl PiiEventCodec for MyCodec { /* ... */ }
//!
//! // 2. Build the crypto repository around your inner repository.
//! let provider = Arc::new(StaticKekProvider::single("v1", kek_bytes)?);
//! let key_store = Arc::new(PostgresKeyStore::new(pool.clone(), Arc::clone(&provider)));
//! let codec = Arc::new(MyCodec);
//! let repo = CryptoShreddingEventRepository::new(
//!     inner_repo, key_store, FieldCipher::new(), codec,
//! );
//! ```
//!
//! # Cargo features
//!
//! - `derive`: enables `#[derive(PiiCodec)]` from `cqrs-es-crypto-derive`.
//! - `chrono`: implies `derive`; teaches the derive macro to redact
//!   `chrono::NaiveDate` secret fields to `"0000-01-01"` by default.
//!   Per-field overrides are available via
//!   `#[pii(secret, redact = "...")]`. See the derive crate's docs.
//! - `testing`: exposes [`InMemoryEventRepository`] for downstream tests.

pub mod cipher;
pub mod kek;
pub mod key_store;
pub mod repository;
pub mod rewrap;

// ── Cipher ──────────────────────────────────────────────────────────────────────────────

#[allow(deprecated)] // PiiCipher is re-exported for backwards compatibility.
pub use cipher::{CryptoError, EncryptedPayload, FieldCipher, KeyMaterial, PiiCipher};

// ── KEK provider ───────────────────────────────────────────────────────────────────────────

pub use kek::{KekError, KekHandle, KekProvider, StaticKekProvider, WrappedDek};

// ── Key store ─────────────────────────────────────────────────────────────────

pub use key_store::{
    InMemoryKeyStore, KeyStore, KeyStoreError, PostgresKeyStore, PostgresKeyStoreOptions,
};

// ── Re-wrap worker ──────────────────────────────────────────────────────────────────────

pub use rewrap::{RewrapStats, RewrapWorker, RewrapWorkerOptions};

// ── Repository ────────────────────────────────────────────────────────────────

pub use repository::{
    CryptoShreddingEventRepository, EncryptedPiiExtract, EncryptedPiiSentinel, PiiEventCodec,
    PiiFields,
};

// ── Testing helpers (opt-in via the `testing` feature) ───────────────────────

#[cfg(any(test, feature = "testing"))]
pub use repository::InMemoryEventRepository;

// ── Derive macro (opt-in via the `derive` feature) ───────────────────────────

#[cfg(feature = "derive")]
pub use cqrs_es_crypto_derive::PiiCodec;
