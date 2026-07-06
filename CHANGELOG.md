# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

> ⚠️ **BREAKING CHANGE — not backward compatible with persisted data.**
>
> This release replaces the legacy command and event API with the path-keyed
> `SetAttributes` / subject-registration model and removes the old surface
> entirely:
>
> - **Removed commands:** `Capture`, `CapturePerson`, `CapturePersonDetails`.
> - **Removed events:** `Modified`, `PersonDetailsUpdated`, `PersonCaptured`,
>   `StepProgressed`.
> - **Removed aggregate state:** the `PersonSlot` / `Journey::persons` map and
>   `Journey::current_step`.
>
> The aggregate **no longer knows how to replay the removed events**, so
> **event streams recorded by previous versions are not compatible and must be
> re-written** — replayed through a one-off migration that maps the old events
> onto the new shapes (`Started`, `SubjectRegistered`, `SubjectBound`,
> `AttributesSet`, …) — before upgrading. There is no in-place
> back-compatibility path. See [Migration](#migration).

### Added

- **Domain spine extracted to the new `es-capture` crate** — the generic
  progressive-capture machinery (the aggregate, the command/event/error enums,
  attribute classification, the subject registry, the PII codec, the optional
  decision-engine seam, validation, and JSON-pointer helpers) now lives in the
  reusable `es-capture` crate. `journey_dynamics` is a thin specialisation of it
  (`Journey = CaptureAggregate<JourneyConfig>`) and adds no aggregate code of its
  own — domain specificity is its attribute schema, JSON schema, rules, and
  views. See [`crates/es-capture/CHANGELOG.md`](crates/es-capture/CHANGELOG.md),
  [`crates/es-capture/README.md`](crates/es-capture/README.md), and
  [`docs/REUSABLE_ES_FOUNDATION.md`](docs/REUSABLE_ES_FOUNDATION.md).

- **`RegisterSubject` command** — registers a data subject (`email` →
  `subject_id` mapping) within a journey. Re-registering the same subject with
  the same email is a no-op; registering with a different email updates the
  stored address. Email is required so the subject can be located by GDPR
  erasure requests.

- **`BindSubject` command** — binds an already-registered subject to a role
  path (e.g. `"persons/passenger_0"`). Rejects binding a role path that is
  already bound to a *different* subject; rebinding the same subject to the
  same path is idempotent.

- **`RegisterAndBindSubject` command** — convenience composite that performs a
  `RegisterSubject` followed by `BindSubject` in a single command, avoiding a
  round-trip when both are needed together. This is the replacement for the
  removed `CapturePerson` command.

- **`SubjectRegistered` / `SubjectBound` events** — the domain events emitted
  by the commands above. `SubjectRegistered { subject_id, email }` feeds the
  subject-lookup projection used by `find_journeys_by_subject`; `SubjectBound
  { role_path, subject_id }` records the role-path → subject binding.

- **`SetAttributes` command** — a single command that accepts a flat map of
  JSON Pointer → value, replacing the removed `Capture` /
  `CapturePersonDetails` commands. A single submission may touch attributes
  belonging to multiple data subjects; each subject's PII is encrypted under
  its own DEK in one atomic operation.

- **`AttributesSet` event** — the corresponding domain event emitted by
  `SetAttributes`. Carries a `plaintext` map (non-sensitive paths) and a
  `secret_partitions` list (one entry per subject whose secret attributes were
  updated).

- **`AttributeSchema` / `PiiClass`** — per-path PII classification. A path is
  either `Plaintext` (stored verbatim in `shared_data`) or
  `Secret { subject }` (encrypted under the DEK belonging to the subject
  resolved by `subject`). Supports exact-path lookup and wildcard namespace
  patterns. The default schema is `permissive` (all paths → plaintext) unless
  `JOURNEY_ATTRIBUTE_SCHEMA_PATH` is set.

- **`WorkflowEvaluated.phase`** — an optional `phase` label returned by the
  decision engine alongside `suggested_actions`. Replaces `current_step` as
  the recommended way to drive UI state.

- **HTTP nested-sugar form for `SetAttributes`** — the extractor accepts a
  nested JSON object and flattens it server-side to the canonical
  `{ "changes": { … } }` form:

  ```json
  { "SetAttributes": { "search": { "origin": "LHR" } } }
  ```

  is equivalent to:

  ```json
  { "SetAttributes": { "changes": { "search/origin": "LHR" } } }
  ```

- **Subject-registration indexes** — the baseline schema indexes
  `SubjectRegistered` and `SubjectBound` events to support
  `find_journeys_by_subject` on the new write path.

### Changed

- **`SecretPartitionData` is now keyed by `role_path`** — the
  `person_ref: String` field is replaced by `role_path: PointerBuf` (the full
  JSON-Pointer schema path, e.g. `/persons/passenger_0`), which is used as the
  crypto label (AAD) so the partition identity is meaningful on the read path.
  This is
  a breaking on-disk change: partitions written with the old `person_ref` field
  are not read back (see the breaking-change note above).

- **`SubjectLookupHook` projects `SubjectRegistered`** when maintaining the
  `subject_lookup` table, so subjects registered via the new commands are
  discoverable by email for erasure requests.

### Removed

- **`JourneyCommand::Capture`** — the step-scoped non-PII capture command has
  been deleted. Replace all usages with `JourneyCommand::SetAttributes` using
  paths under `<step>/…` (e.g. `search/origin`). Deprecated since 0.3.0.

- **`JourneyCommand::CapturePerson`** — the person-slot capture command has
  been deleted. Replace with `JourneyCommand::RegisterAndBindSubject` (or
  `RegisterSubject` + `BindSubject`) and send `name` / `phone` as path-keyed
  attributes via `SetAttributes` under `persons/<ref>/…`. Deprecated since
  0.4.0.

- **`JourneyCommand::CapturePersonDetails`** — the free-form PII details
  command has been deleted. Replace with `JourneyCommand::SetAttributes` using
  paths under `persons/<ref>/…`. Deprecated since 0.3.0.

- **Legacy events `Modified`, `PersonDetailsUpdated`, `PersonCaptured`, and
  `StepProgressed`** — removed from the event enum. The aggregate can no longer
  replay them, so **historical event streams that contain them must be re-written**
  before upgrading (see the breaking-change note at the top of this release).

- **`PersonSlot` struct and `Journey::persons` field** — the per-person slot
  map (`BTreeMap<String, PersonSlot>`) has been removed from the aggregate.
  Subject identity is now held exclusively in `Journey::subjects` (keyed by
  `subject_id`) and role-path bindings in `Journey::bindings`. Consumers that
  read `PersonSlot` fields must switch to `JourneyView::shared_data` under
  `persons/<ref>/…`.

- **`Journey::current_step` field** — removed from the aggregate state. Read
  `WorkflowDecisionView.phase` instead. `JourneyView::current_step` is no longer
  populated, since its source event `StepProgressed` has also been removed.

- **Legacy subject-lookup fallback in `SetAttributes`** — `SetAttributes` no
  longer falls back to the old `persons` map when resolving a secret role path.
  All subjects must be registered via `RegisterAndBindSubject` (or
  `RegisterSubject` + `BindSubject`) before sending secret path-keyed
  attributes for that role.

### Deprecated

- `JourneyView::current_step`. No longer populated (its source event
  `StepProgressed` has been removed); read `WorkflowDecisionView.phase` instead.
  The field remains on the view for one release before removal.

### Migration

See [`docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md)
for a full migration guide including before/after code snippets, a quick
reference table, and common gotchas.
