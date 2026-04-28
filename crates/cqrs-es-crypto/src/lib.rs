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
//! # Key management
//!
//! DEKs are wrapped (encrypted) with a Key Encryption Key (KEK) using AES-256-KWP
//! (RFC 5649) before storage. The [`PostgresKeyStore`] persists wrapped DEKs in a
//! `subject_encryption_keys` table. Deleting a row is the shredding operation.
//!
//! The required DDL is:
//!
//! ```sql
//! CREATE TABLE subject_encryption_keys (
//!     key_id      UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
//!     subject_id  UUID      NOT NULL UNIQUE,
//!     wrapped_key BYTEA     NOT NULL,
//!     created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
//! );
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use cqrs_es_crypto::{
//!     CryptoShreddingEventRepository, PiiCipher, PiiEventCodec, PiiFields,
//!     EncryptedPiiSentinel, PostgresKeyStore,
//! };
//!
//! // 1. Implement PiiEventCodec for your domain.
//! struct MyCodec;
//! impl PiiEventCodec for MyCodec { /* ... */ }
//!
//! // 2. Build the crypto repository around your inner repository.
//! let cipher = PiiCipher::new(kek_bytes)?;
//! let key_store = Arc::new(PostgresKeyStore::new(pool.clone(), PiiCipher::new(kek_bytes)?));
//! let codec = Arc::new(MyCodec);
//! let repo = CryptoShreddingEventRepository::new(inner_repo, key_store, cipher, codec);
//! ```

pub mod cipher;
pub mod key_store;
pub mod repository;

// ── Cipher ────────────────────────────────────────────────────────────────────

pub use cipher::{CryptoError, EncryptedPayload, KeyMaterial, PiiCipher};

// ── Key store ─────────────────────────────────────────────────────────────────

pub use key_store::{InMemoryKeyStore, KeyStore, KeyStoreError, PostgresKeyStore};

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
