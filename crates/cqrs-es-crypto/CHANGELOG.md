# Changelog

All notable changes to `cqrs-es-crypto` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Unique-constraint violations on the transactional write path no longer
  surface as `UnknownError`** — when `with_transactional_writes` is enabled,
  a Postgres `events_pkey` violation (SQLSTATE `23505`) is now mapped to
  `PersistenceError::OptimisticLockError`, matching the behaviour of the
  legacy delegating path. `cqrs-es` surfaces this as
  `AggregateError::AggregateConflict`, so the standard inline-retry pattern
  for concurrent writes against the same aggregate now triggers as intended
  instead of being abandoned as an unexpected error.

## [0.2.1] - 2026-05-19

### Added

- `KeyStore::delete_key_in_tx` (`postgres` feature) — deletes the DEK for a
  subject within a caller-supplied `sqlx::Transaction`, so DEK removal can be
  committed atomically in a single Postgres transaction alongside other PII
  deletions (e.g. a `subject_lookup` row). `PostgresKeyStore` provides a full
  implementation; all other stores fall back to `delete_key` via the default
  impl. Postgres-backed custom implementations should override this method to
  preserve the atomicity guarantee.

### Fixed

- **Torn reads on the decryption path** — all DEKs for an event batch are now
  pre-fetched in a first pass before any event is decrypted. A crypto-shred
  arriving mid-batch is therefore applied consistently to every event for that
  subject rather than leaving earlier events decrypted and later ones redacted.
  The change also reduces key-store round-trips from O(events) to
  O(unique subjects).
- **`last_sequence` hardcoded to `0` in snapshot upsert** — the snapshot
  `INSERT … ON CONFLICT` statement now records the sequence number of the last
  event in the persist batch as `last_sequence` instead of always writing `0`.
  The previous behaviour caused the aggregate replay watermark to always point
  to the start of the event log, defeating the purpose of snapshotting. The
  `ON CONFLICT` clause was also corrected to reference `EXCLUDED.*` for all
  updated columns.

## [0.2.0] - 2026-05-19

This release adds zero-downtime KEK rotation and an atomic transactional write
path. A `KekProvider` abstraction replaces the old single-key approach, with
`StaticKekProvider` as the built-in implementation (env-var backed, supports
multiple named versions). `PostgresKeyStore` now persists a `kek_id` alongside
each wrapped DEK, and a new `RewrapWorker` re-wraps stale DEKs in the
background once a new KEK version is promoted to primary. The write path gains
an opt-in transactional mode (`with_transactional_writes`) and a `PersistHook`
trait so that domain-specific side-writes (e.g. subject-lookup table inserts)
are committed atomically with the events. `PiiCipher` is deprecated in favour
of the split `FieldCipher` + `KekProvider` design. The `postgres` feature gate
is new — existing users will need to add it explicitly.

### Added

- `KekProvider` trait — abstraction for Key Encryption Key management with
  `current()`, `by_id()`, `wrap()`, and `unwrap()` methods. Implementations
  supply the active primary KEK handle and look up KEK versions by ID,
  enabling zero-downtime rotation.
- `KekHandle` — lightweight reference to a named KEK version (carries only
  the `id` string, not raw key bytes).
- `StaticKekProvider` — in-process `KekProvider` backed by a map of named
  32-byte keys. Exposes `new` (multi-version map), `single` (one version),
  and `from_env` (reads `KEK_PRIMARY` / `KEK_<ID>` environment variables) constructors.
- `WrappedDek` — the unit persisted by `PostgresKeyStore`: a tuple of
  `(key_id, kek_id, wrapped_key)` used to track which KEK version wrapped
  each DEK.
- `KekError` — dedicated error type for KEK operations (`UnknownVersion`,
  `Wrap`, `Unwrap`, `Transport`).
- `KeyStore::list_stale_subjects` — cursor-paginated query returning subject
  IDs whose DEK is still wrapped under a non-primary KEK version, enabling
  background re-wrap sweeps.
- `KeyStore::rewrap_key` — atomically unwraps a subject's DEK with the old
  KEK version and re-wraps it under the current primary, returning `true` if
  the key was actually updated.
- `PostgresKeyStoreOptions` — optional configuration struct for
  `PostgresKeyStore`. Currently exposes `lazy_rewrap` (default `true`),
  which spawns a background task to re-wrap stale DEKs on each read.
- `PostgresKeyStore::new_with_options` — constructor accepting
  `PostgresKeyStoreOptions` for callers that need non-default settings
  (e.g. deterministic tests with `lazy_rewrap: false`).
- `InMemoryKeyStore::new_with_provider` — constructor accepting an
  `Arc<dyn KekProvider>`, enabling in-memory tests that exercise
  multi-version KEK logic.
- `InMemoryKeyStore::insert_for_testing` — test helper that pre-populates
  the store with a subject entry under a specific `kek_id`, making it
  straightforward to set up stale-key scenarios without calling
  `get_or_create_key`.
- `RewrapWorker` — background worker that drives `list_stale_subjects` and
  `rewrap_key` across all stale subjects in cursor-paginated, bounded-
  concurrency batches. Drive with `run_once` (single sweep) or
  `run_forever` (polls on a configurable timer).
- `RewrapWorkerOptions` — configuration for `RewrapWorker`: `batch_size`,
  `max_concurrency`, and `batch_pause`.
- `RewrapStats` — statistics returned by `RewrapWorker::run_once`:
  `scanned`, `rewrapped`, `failures`, and `duration`.
- `postgres` Cargo feature — gates all Postgres-specific items
  (`PostgresKeyStore`, `PostgresKeyStoreOptions`, `PersistHook`). Previously
  these were compiled unconditionally.
- `CryptoShreddingEventRepository::with_transactional_writes` — builder
  method (requires `postgres` feature) that enables an atomic Postgres
  transaction per `persist` call, committing DEKs, encrypted events, and
  any hook writes in a single transaction instead of delegating to the inner
  repository's non-atomic persist.
- `PersistHook` trait (requires `postgres` feature) — called within the
  transactional persist path. Receives the unencrypted serialised events and
  a live `&mut Transaction`; returning an error rolls back the entire
  transaction. Useful for domain-specific side-writes (e.g. subject-lookup
  table inserts) that must be atomic with event persistence.
- `CryptoShreddingEventRepository::with_persist_hook` — registers a
  `PersistHook`; multiple hooks are called in registration order.
- DB migration `20260515132849_kek_versioning` — adds `kek_id TEXT NOT NULL`
  and `rewrapped_at TIMESTAMP` columns to `subject_encryption_keys`, and a
  supporting index on `kek_id`. Required before upgrading to 0.2.

### Changed

- `PostgresKeyStore::new` now accepts `Arc<dyn KekProvider>` as its second
  argument instead of a raw 32-byte KEK `Vec<u8>`. Callers must construct a
  `StaticKekProvider` (or custom implementation) and pass it in.
- `CryptoShreddingEventRepository::new` now accepts `FieldCipher` instead of
  the deprecated `PiiCipher`. `FieldCipher` has no KEK argument — wrapping
  is handled entirely by the `KekProvider` inside `PostgresKeyStore`.

### Deprecated

- `PiiCipher` — mixed field encryption (AES-256-GCM) and DEK wrapping
  (AES-256-KWP) in a single struct. These concerns are now separated: use
  `FieldCipher` for field encryption and a `KekProvider` implementation for
  DEK wrapping. `PiiCipher` remains available for this release but will be
  removed in a future version.

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

[Unreleased]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.2.1...HEAD
[0.2.1]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.2.0...cqrs-es-crypto-v0.2.1
[0.2.0]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.4...cqrs-es-crypto-v0.2.0
[0.1.4]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.3...cqrs-es-crypto-v0.1.4
[0.1.3]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.2...cqrs-es-crypto-v0.1.3
[0.1.2]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.1...cqrs-es-crypto-v0.1.2
[0.1.1]: https://github.com/redbadger/journey_dynamics/compare/cqrs-es-crypto-v0.1.0...cqrs-es-crypto-v0.1.1
[0.1.0]: https://github.com/redbadger/journey_dynamics/releases/tag/cqrs-es-crypto-v0.1.0
