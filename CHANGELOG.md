# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

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
  round-trip when both are needed together. This is the recommended
  replacement for the deprecated `CapturePerson` command.

- **`SubjectRegistered` / `SubjectBound` events** — the domain events emitted
  by the commands above. `SubjectRegistered { subject_id, email }` feeds the
  subject-lookup projection used by `find_journeys_by_subject`; `SubjectBound
  { role_path, subject_id }` records the role-path → subject binding.

- **`SetAttributes` command** — a single command that accepts a flat map of
  `AttributePath → Value` and replaces the step-scoped `Capture` /
  `CapturePersonDetails` commands. A single submission may touch attributes
  belonging to multiple data subjects; each subject's PII is encrypted under
  its own DEK in one atomic operation.

- **`AttributesSet` event** — the corresponding domain event emitted by
  `SetAttributes`. Carries a `plaintext` map (non-sensitive paths) and a
  `secret_partitions` list (one entry per subject whose secret attributes were
  updated). Existing projectors that pattern-match on `Modified` /
  `PersonDetailsUpdated` continue to fire for commands that use the legacy
  surface.

- **`AttributePath` newtype** — a validated, slash-separated path string (e.g.
  `"search/origin"`, `"persons/passenger_0/passportNumber"`). Validates that
  the string is non-empty, has no leading/trailing `/`, no empty segments, and
  contains only printable characters. Implements `Display`, `FromStr`,
  `Serialize`, `Deserialize`, `Ord`, `Hash`.

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

- **`json_path` helpers** — `set_at_path`, `get_at_path`, `flatten`,
  `rehydrate` for reading and writing deeply nested `serde_json::Value` trees
  using `AttributePath` keys.

- **Subject-registration migration** (`20260606000001_subject_registration`) —
  adds indexes on `SubjectRegistered` and `SubjectBound` events to support
  `find_journeys_by_subject` on the new write path, and updates the
  `journey_person` table documentation. Existing journeys remain covered by
  the pre-existing `PersonCaptured` index.

### Changed

- **`SecretPartitionData` is now keyed by `role_path`** — the
  `person_ref: String` field is replaced by `role_path: AttributePath` (the
  full schema path, e.g. `"persons/passenger_0"`), which is used as the crypto
  label (AAD) so the partition identity is meaningful on the read path. A
  custom `Deserialize` impl preserves backward compatibility: events written
  with the old `person_ref` field are read by synthesising
  `"persons/{person_ref}"` as the role path.

- **`SubjectLookupHook` projects `SubjectRegistered`** in addition to the
  legacy `PersonCaptured` event when maintaining the `subject_lookup` table,
  so subjects registered via the new commands are discoverable by email for
  erasure requests.

- **`CapturePerson` now emits `SubjectRegistered` + `SubjectBound`** instead of
  `PersonCaptured`, and **silently discards the `name` and `phone` fields** —
  the new subject model carries only the `email` (for erasure lookup) and the
  role-path binding. To retain a subject's name or phone, send them as
  path-keyed attributes via `SetAttributes` (e.g. `persons/<ref>/firstName`).
  `PersonCaptured` is now emitted only by historical replay.

### Deprecated

- `JourneyCommand::CapturePerson`. Use `JourneyCommand::RegisterAndBindSubject`
  (or `RegisterSubject` + `BindSubject`) followed by
  `JourneyCommand::SetAttributes` for path-keyed PII fields instead.

- `JourneyCommand::Capture` and `JourneyCommand::CapturePersonDetails`.
  Use `JourneyCommand::SetAttributes` instead.
- `JourneyEvent::Modified`, `JourneyEvent::PersonDetailsUpdated`, and
  `JourneyEvent::StepProgressed`. New writes should emit
  `JourneyEvent::AttributesSet`. Legacy events continue to replay.
- `Journey::current_step()`, `JourneyView::current_step`, and
  `PersonSlot.details` accessors / fields. Read from `shared_data`
  under the relevant path-keyed attributes instead.

All deprecated items remain fully functional and will keep working in
this and future releases until an explicit removal RFC is accepted.

### Migration

See [`docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md)
for a full migration guide including before/after code snippets, a quick
reference table, and common gotchas.
