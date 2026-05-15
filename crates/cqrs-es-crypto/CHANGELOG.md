# Changelog

All notable changes to `cqrs-es-crypto` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4] - 2026-05-15

### Added

- Bumped `cqrs-es-crypto-derive` to 0.1.4 to pick up support for
  `#[pii(secret)]` fields of type `Vec<_>`, which now redact to the empty
  JSON array `[]` (see `cqrs-es-crypto-derive` 0.1.4).

## [0.1.3] - 2026-05-07

### Fixed

- Bumped `cqrs-es-crypto-derive` to 0.1.3 to pick up the fix for
  `#[pii(subject)]` field names other than `subject_id` being silently
  ignored by the encrypted-payload write path (see `cqrs-es-crypto-derive`
  0.1.3).

## [0.1.2] - 2026-05-06

### Fixed

- Bumped `cqrs-es-crypto-derive` to 0.1.2 to pick up the fix for
  non-string `#[pii(plaintext)]` fields being corrupted to the empty
  string in the persisted payload (see `cqrs-es-crypto-derive` 0.1.2).

## [0.1.1] - 2026-05-02

### Added

- `chrono` Cargo feature — implies `derive`; enables `chrono::NaiveDate` support
  in the `#[derive(PiiCodec)]` macro (see `cqrs-es-crypto-derive` 0.1.1).
- Documented all Cargo features (`derive`, `chrono`, `testing`) in the crate-level
  rustdoc.

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

[Unreleased]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.4...HEAD
[0.1.4]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.3...cqrs-es-crypto-v0.1.4
[0.1.3]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.2...cqrs-es-crypto-v0.1.3
[0.1.2]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.1...cqrs-es-crypto-v0.1.2
[0.1.1]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.0...cqrs-es-crypto-v0.1.1
[0.1.0]: https://github.com/redbadger/journey_dynamics/releases/tag/cqrs-es-crypto-v0.1.0
