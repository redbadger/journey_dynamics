# Changelog

All notable changes to `cqrs-es-crypto` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-04-29

### Added

- `CryptoShreddingEventRepository<R>` — wraps any `PersistedEventRepository` with
  transparent AES-256-GCM PII field encryption and GDPR crypto-shredding on the read
  and write paths.
- `PiiEventCodec` trait — domain-supplied codec that tells the repository which event
  types carry PII, where the subject ID lives, and how to reassemble or redact payloads.
- `PiiCipher` — AES-256-GCM field encryption and AES-256-KWP (RFC 5649) key wrapping,
  with `zeroize`-on-drop for all key material.
- `PostgresKeyStore` — per-subject Data Encryption Key (DEK) storage backed by a
  `subject_encryption_keys` Postgres table. Supports `get_or_create_key`,
  `get_key`, and `delete_key` (the shredding operation).
- `InMemoryKeyStore` — in-process key store for use in tests (enabled via the
  `testing` Cargo feature).
- `InMemoryEventRepository` — in-process event repository for unit tests without a
  database (enabled via the `testing` Cargo feature).
- `derive` Cargo feature — re-exports `#[derive(PiiCodec)]` from the
  `cqrs-es-crypto-derive` companion crate.

### Known limitations

- `stream_all_events` is not supported by `CryptoShreddingEventRepository` and
  returns an error. The `ReplayStream` API does not expose raw `SerializedEvent`
  items, so there is no point at which decryption can be applied. Use `get_events`
  or `stream_events` per aggregate instead.
- Aggregate snapshots are not encrypted. If your aggregate state contains PII it
  will be stored in plaintext, and crypto-shredding a subject will not redact it.

[Unreleased]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.0...HEAD
[0.1.0]: https://github.com/redbadger/journey_dynamics/releases/tag/cqrs-es-crypto-v0.1.0
