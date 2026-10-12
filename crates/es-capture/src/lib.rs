//! # es-capture
//!
//! A reusable, domain-agnostic **progressive-capture spine** for event-sourced
//! systems that need per-subject crypto-shredding (GDPR right-to-erasure) under
//! dynamic, externalised rules.
//!
//! This crate holds the machinery that is generic across domains — JSON-pointer
//! attribute handling, privacy classification, the subject registry, validation,
//! the optional decision-engine seam, and the CQRS assembly — so that a new
//! event-sourced domain is mostly *configuration + types + (optional) rules*
//! rather than new aggregate code.
//!
//! See [`docs/REUSABLE_ES_FOUNDATION.md`] for the design and extraction plan.
//!
//! # Re-exports
//!
//! [`jsonptr`], [`cqrs_es`] and [`cqrs_es_crypto`] types appear throughout the
//! public API (`PointerBuf` paths, the `Aggregate` impl, the PII codec). They
//! are re-exported so a consumer can name them at the exact versions this
//! crate was built against.
//!
//! [`docs/REUSABLE_ES_FOUNDATION.md`]: https://github.com/redbadger/journey_dynamics/blob/main/docs/REUSABLE_ES_FOUNDATION.md

pub mod aggregate;
pub mod attribute_schema;
pub mod attributes_set_codec;
pub mod capture;
pub mod decision_engine;
pub mod json_path;
pub mod schema_validator;
pub mod subject_registry;

pub use cqrs_es;
pub use cqrs_es_crypto;
pub use jsonptr;
