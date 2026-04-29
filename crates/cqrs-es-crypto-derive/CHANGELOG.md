# Changelog

All notable changes to `cqrs-es-crypto-derive` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-derive-v0.1.0...HEAD
[0.1.0]: https://github.com/redbadger/journey_dynamics/releases/tag/cqrs-es-crypto-derive-v0.1.0
