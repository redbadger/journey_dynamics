# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

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

### Migration

See [`docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](docs/PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md)
for a full migration guide including before/after code snippets, a quick
reference table, and common gotchas.

---

> **Deprecation notice (Phase C — upcoming):** `JourneyCommand::Capture`,
> `JourneyCommand::CapturePersonDetails`, `JourneyEvent::Modified`,
> `JourneyEvent::PersonDetailsUpdated`, `JourneyEvent::StepProgressed`,
> `Journey::current_step()`, and `JourneyView::current_step` will be marked
> `#[deprecated]` in the next release. They will continue to compile and run;
> the compiler will emit warnings nudging callers toward the new surface. No
> code will be removed in that release. See the migration guide for details on
> clearing the warnings when you are ready.
