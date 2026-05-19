# Changelog

All notable changes to `cqrs-es-crypto-derive` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1] - 2026-05-19

Companion bump to track `cqrs-es-crypto` 0.2.1. No changes to the
`#[derive(PiiCodec)]` macro itself.

## [0.2.0] - 2026-05-19

Companion bump to track `cqrs-es-crypto` 0.2.0. The `#[derive(PiiCodec)]`
macro is unchanged.

### Changed

- Bumped to 0.2.0 to track `cqrs-es-crypto` 0.2.0. No changes to the
  `#[derive(PiiCodec)]` macro itself.
- Integration tests updated to use `FieldCipher` instead of the now-deprecated
  `PiiCipher` (see `cqrs-es-crypto` 0.2.0).

## [0.1.4] - 2026-05-15

### Added

- `#[pii(secret)]` fields of type `Vec<_>` are now supported out of the box.
  Their default redaction value is the empty JSON array `[]`, mirroring the
  existing `Option<_> → null` and `serde_json::Value → {}` defaults. The
  empty array deserializes back into an empty `Vec<_>` on the read path
  after a subject's DEK has been deleted (crypto-shredding), so a
  redacted event remains structurally valid for `serde_json::from_value`.
  The previous `#[pii(secret, redact = "...")]` escape hatch could not be
  used for `Vec<_>` fields because its argument was always emitted as a
  JSON string and could not produce a typed array. Like `String`,
  `Option<_>`, and `serde_json::Value`, the `Vec<_>` default is fixed and
  cannot be overridden by a `redact = "..."` argument.

## [0.1.3] - 2026-05-07

### Fixed

- The `#[pii(subject)]` field name is now honoured throughout the generated
  codec. The `classify` arm previously hardcoded the literal `"subject_id"`
  as the JSON key in the encrypted-payload write path, while the read,
  reconstruct, and redact arms correctly used the variant's actual
  identifier. This asymmetry meant the macro only produced consistent
  payloads when the field happened to be named `subject_id`. The subject
  field can now be named anything (`subject_id`, `user_id`, `customer_ref`,
  etc.) and the chosen identifier is used as the JSON key in every read
  and write site.

## [0.1.2] - 2026-05-06

### Fixed

- `#[pii(plaintext)]` field values are now preserved verbatim in the
  persisted payload regardless of their JSON type. The previous
  implementation coerced every plaintext field through
  `as_str().unwrap_or("")` when building the encrypted-form event,
  silently corrupting non-string fields (integers, `Option<String>::None`,
  booleans, etc.) to the empty string and breaking deserialization on
  read-back. Plaintext fields are now cloned as-is, matching the
  behaviour already present in the `reconstruct` and `redact` arms.

## [0.1.1] - 2026-05-02

### Added

- `chrono` Cargo feature — enables automatic redaction of `chrono::NaiveDate`
  secret fields to the sentinel `"0000-01-01"` when a subject's DEK has been
  deleted.
- `#[pii(secret, redact = "...")]` attribute — allows an explicit per-field
  redaction sentinel override for types whose default is not fixed by the
  crate's contract (e.g. `NaiveDate` and custom newtypes). Attempting to
  override `String`, `Option<_>`, or `serde_json::Value` fields is a
  compile error.

## [0.1.0] - 2026-04-29

### Added

- `#[derive(PiiCodec)]` — proc-macro that generates a `{Name}PiiCodec` struct and a
  complete `PiiEventCodec` implementation from an annotated event enum.
- `#[pii(event_type = "...")]` variant attribute — marks a variant as PII-bearing and
  associates it with its `DomainEvent::event_type()` string.
- `#[pii(sentinel = "...")]` variant attribute — overrides the name of the
  encrypted-blob field in the stored JSON payload (defaults to `"encrypted_pii"`).
- Field role attributes:
  - `#[pii(subject)]` — the data-subject UUID kept in plaintext for DEK lookup.
  - `#[pii(plaintext)]` — a non-PII field preserved verbatim through all codec paths.
  - `#[pii(secret)]` — a PII field that is encrypted on write and decrypted or
    redacted on read.
- Automatic redaction value inference for `#[pii(secret)]` fields:
  - `String` → `"[redacted]"`
  - `Option<_>` → `null`
  - `serde_json::Value` → `{}`
- Span-accurate `compile_error!` diagnostics for all invalid annotation combinations
  (missing `event_type`, unannotated fields, missing subject, missing secret, etc.).

[Unreleased]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.2.1...HEAD
[0.2.1]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.2.0...cqrs-es-crypto-derive-v0.2.1
[0.2.0]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.1.4...cqrs-es-crypto-derive-v0.2.0
[0.1.4]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.1.3...cqrs-es-crypto-derive-v0.1.4
[0.1.3]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.1.2...cqrs-es-crypto-derive-v0.1.3
[0.1.2]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.1.1...cqrs-es-crypto-derive-v0.1.2
[0.1.1]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.1.0...cqrs-es-crypto-derive-v0.1.1
[0.1.0]: https://github.com/redbadger/journey_dynamics/releases/tag/cqrs-es-crypto-derive-v0.1.0
