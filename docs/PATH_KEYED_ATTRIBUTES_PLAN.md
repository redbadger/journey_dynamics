# Path-Keyed Attributes — Implementation Plan

**Companion to:** [`PATH_KEYED_ATTRIBUTES_DESIGN.md`](./PATH_KEYED_ATTRIBUTES_DESIGN.md)
**Audience:** coding agents executing the migration in small, verifiable steps
**Last updated:** 2026-05-25

---

## How to use this plan

The migration is decomposed into numbered steps. **Every step must leave the
workspace in a state where:**

1. `cargo check --workspace --all-targets` succeeds.
2. `cargo test --workspace` succeeds (excluding the two Postgres integration
   tests in `crates/journey_dynamics/tests/` that require a running database —
   those should still compile and any that ran before must still pass when a
   database is available).
3. The `journey_dynamics` binary (`cargo run -p journey_dynamics --bin
   journey_dynamics`) starts cleanly against a fresh database created from the
   current migrations.
4. The `flight-booking` example builds (`cargo check -p flight-booking
   --all-targets`).

If a step cannot satisfy this, split it further before proceeding.

The plan is divided into three phases:

| Phase | Goal                                                                 |
| ----- | -------------------------------------------------------------------- |
| **A** | Introduce new types and the new `SetAttributes` command **alongside** existing commands. Old code paths remain fully functional. |
| **B** | Switch the flight-booking example and HTTP surface to the new model. Add an event up-caster for old events. |
| **C** | Remove deprecated commands, events, fields, columns, and migrations. |

After Phase A every test continues to pass. After Phase B the example uses
`SetAttributes` exclusively but `Capture`/`CapturePersonDetails` still work for
back-compat. After Phase C the legacy surface is gone.

### Conventions

- Each step starts with **Pre-flight** (what to read) and ends with
  **Validation** (what to run) and **Done when** (acceptance).
- Steps inside a phase are ordered; do not reorder unless explicitly noted.
- Where a step removes public API, mark it `#[deprecated]` first and remove in
  Phase C.
- Keep commits small and conventional: one step = one PR-sized commit.

---

## Phase A — Additive groundwork (no behaviour change)

### A1. Introduce `AttributePath` newtype

**Pre-flight.** Read `crates/journey_dynamics/src/domain/mod.rs`.

**Do.**

- Add a new module `crates/journey_dynamics/src/domain/attribute_path.rs`
  exposing:

  ```rust
  pub struct AttributePath(String);
  ```

  with:

  - `pub fn new(s: impl Into<String>) -> Result<Self, AttributePathError>`
    validating that the string is non-empty, has no leading/trailing `/`,
    has no empty segments, and contains only printable characters.
  - `pub fn segments(&self) -> impl Iterator<Item = &str>`.
  - `pub fn as_str(&self) -> &str`.
  - `pub fn starts_with(&self, prefix: &AttributePath) -> bool`.
  - `Display`, `FromStr`, `Serialize`, `Deserialize`, `Ord`, `PartialOrd`,
    `Hash`, `Clone`, `Debug`, `Eq`, `PartialEq`.
  - `AttributePathError` as a `thiserror::Error` enum.

- Re-export from `domain/mod.rs`: `pub mod attribute_path; pub use
  attribute_path::{AttributePath, AttributePathError};`.

- Unit tests covering: round-trip parse/display, segments iterator,
  rejection of empty / `"/foo"` / `"foo/"` / `"a//b"`, `starts_with` true and
  false cases.

**Validation.** `cargo test -p journey_dynamics domain::attribute_path`.

**Done when.** The new module has ≥ 8 unit tests, all green. Nothing else
changes.

---

### A2. Introduce `PiiClass` and `AttributeSchema`

**Pre-flight.** Read `crates/journey_dynamics/src/services/schema_validator.rs`.

**Do.**

- New module `crates/journey_dynamics/src/domain/attribute_schema.rs` with:

  ```rust
  pub enum PiiClass {
      Plaintext,
      Secret { subject: AttributePath },
  }

  pub struct AttributeSchema {
      paths: BTreeMap<AttributePath, PiiClass>,
      json_schema: Option<serde_json::Value>,
  }
  ```

  Methods:

  - `pub fn new(paths: BTreeMap<AttributePath, PiiClass>, json_schema:
    Option<Value>) -> Self`.
  - `pub fn classify(&self, path: &AttributePath) -> Option<&PiiClass>`.
  - `pub fn json_schema(&self) -> Option<&Value>`.
  - `pub fn known_paths(&self) -> impl Iterator<Item = &AttributePath>`.

- Add a `Classification` result type used downstream (mirror the design doc's
  shape, but keep it inside `journey_dynamics` for now — we will hoist it into
  `cqrs-es-crypto` in step A6):

  ```rust
  pub struct Classification {
      pub plaintext: BTreeMap<AttributePath, Value>,
      pub secret_by_subject: BTreeMap<Uuid, BTreeMap<AttributePath, Value>>,
      pub unknown: Vec<AttributePath>,
  }
  ```

- A pure `fn classify_changes(schema: &AttributeSchema, changes:
  &BTreeMap<AttributePath, Value>, subject_lookup: impl Fn(&AttributePath)
  -> Option<Uuid>) -> Classification`.

- Tests:

  - All plaintext → empty `secret_by_subject`, populated `plaintext`.
  - Mixed plaintext + secret for one subject → populated single-subject
    map.
  - Two subjects in one batch → two keys in `secret_by_subject`.
  - Unknown path → ends up in `unknown` and is not lost.
  - Secret path with no resolvable subject → ends up in `unknown` (caller
    decides how to react).

- Re-export from `domain/mod.rs`.

**Validation.** `cargo test -p journey_dynamics domain::attribute_schema`.

**Done when.** Module compiles, tests green, no other files changed.

---

### A3. Add JSON path helpers (`set_at_path`, `get_at_path`, `flatten`, `rehydrate`)

**Pre-flight.** Read how `json_patch::merge` is used in
`crates/journey_dynamics/src/domain/journey.rs`.

**Do.**

- New module `crates/journey_dynamics/src/domain/json_path.rs`:

  - `pub fn set_at_path(target: &mut Value, path: &AttributePath, value:
    Value)` — walks segments, creating nested `Object`/`Array` shells as
    needed. Numeric segments that look like array indices into an existing
    array are treated as array indices; otherwise as object keys. Document
    the rule in the module doc-comment.
  - `pub fn get_at_path<'a>(source: &'a Value, path: &AttributePath) ->
    Option<&'a Value>`.
  - `pub fn flatten(source: &Value) -> BTreeMap<AttributePath, Value>` —
    leaves only; objects/arrays are recursed.
  - `pub fn rehydrate(changes: &BTreeMap<AttributePath, Value>) -> Value` —
    inverse.

- Property-style tests: `rehydrate(flatten(x)) == x` for representative
  shapes (nested objects, arrays of objects, scalars).

- Edge cases tested: setting a deeper path through an existing scalar
  replaces the scalar; setting `persons/0/name` into `{}` yields
  `{"persons":[{"name":"…"}]}` (or `{"persons":{"0":{"name":"…"}}}` — pick
  one and document; the design doc treats `persons/0/...` as an array index,
  but `passenger_0` as an object key, so do this purely by "does the string
  parse as a u32?").

- Re-export from `domain/mod.rs`.

**Validation.** `cargo test -p journey_dynamics domain::json_path`.

**Done when.** Helpers covered by ≥ 10 unit tests, all green.

---

### A4. Extend `WorkflowDecision` with `phase`

**Pre-flight.** Read `services/decision_engine.rs`, `domain/journey.rs`
(`WorkflowDecisionState`), `queries.rs` (`WorkflowDecisionView`),
`view_repository.rs`.

**Do.**

- Add `pub phase: Option<String>` to:
  - `WorkflowDecision` in `services/decision_engine.rs`.
  - `WorkflowDecisionState` in `domain/journey.rs`.
  - `WorkflowDecisionView` in `queries.rs`.

- `SimpleDecisionEngine`: emit `phase: None`.
- `GoRulesDecisionEngine`: read `phase` (string) from the JDM result if
  present, otherwise `None`. Mirror the existing `suggested_actions`
  extraction pattern.

- `JourneyEvent::WorkflowEvaluated` payload **stays the same** for now; we
  do **not** add `phase` to the event yet (it would force an
  event-up-caster). Instead, the aggregate stores the phase in
  `latest_workflow_decision` but it is lost across replay until step B3.
  Document this temporary limitation with a `// TODO(path-keyed-step-B3)`
  comment.

  > Rationale: keeping events unchanged in Phase A preserves on-disk
  > compatibility and avoids touching the PII codec.

- `JourneyView`: populate `phase` from the latest stored decision row.

- Migration `migrations/2026MMDDHHMMSS_workflow_phase.up.sql`:

  ```sql
  ALTER TABLE journey_workflow_decision ADD COLUMN phase TEXT;
  ```

  and matching `.down.sql` (`ALTER TABLE … DROP COLUMN phase;`).

- Update `view_repository.rs` to read/write `phase` on
  `journey_workflow_decision`.

- Update existing tests to construct `WorkflowDecision { phase: None,
  suggested_actions: … }` where needed. Add one new test asserting that
  `phase` survives load.

**Validation.** `cargo test --workspace`. Bring up Postgres and run
`postgres_view_repository.rs` if available.

**Done when.** Workspace tests pass; integration test for view repository
still passes with the new column; `phase` round-trips through the view.

---

### A5. Add `SetAttributes` command and `AttributesSet` event (parallel to existing)

**Pre-flight.** Read `domain/commands.rs`, `domain/events.rs`,
`domain/journey.rs::handle`, `domain/journey.rs::apply`,
`crates/journey_dynamics/src/pii_codec.rs` (for the macro-derived codec
structure).

**Do.**

#### A5.1 — Wire format

In `domain/commands.rs`:

```rust
#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    Start { id: Uuid },
    Capture { … },                  // unchanged, mark #[deprecated = "use SetAttributes"]
    SetAttributes {
        changes: BTreeMap<AttributePath, Value>,
    },
    CapturePerson { … },
    CapturePersonDetails { … },     // unchanged, mark #[deprecated]
    Complete,
    ForgetSubject { … },
}
```

#### A5.2 — Event

In `domain/events.rs`:

```rust
#[pii(event_type = "AttributesSet", sentinel = "encrypted_data")]
AttributesSet {
    #[pii(plaintext)] plaintext: BTreeMap<AttributePath, Value>,
    #[pii(subject)]   subject_id: Option<Uuid>,
    #[pii(secret)]    secret: BTreeMap<AttributePath, Value>,
},
```

Notes:

- This v1 event supports **at most one subject per command**. Multi-subject
  is deferred (see Phase C / future work). Document this in a comment.
- `subject_id: Option<Uuid>` is `None` when `secret` is empty.
- Update `event_type()` and `event_version()`.
- Confirm `cqrs-es-crypto-derive` supports `BTreeMap<_, _>` in a `#[pii]`
  field; if not, fall back to hand-writing the codec arm for
  `AttributesSet` (see step A6). Prefer hand-writing if the macro requires
  shape changes — the new event is schema-driven and a different beast.

#### A5.3 — Aggregate `handle`

Add a `JourneyCommand::SetAttributes { changes } =>` arm. It must:

1. Reject if journey not started (`NotFound`).
2. Reject if `Complete`.
3. Reject if `changes` is empty (`InvalidData("no changes")`).
4. Compute the path classification using the schema obtained from
   `services.attribute_schema()` (see A5.4). Unknown paths → return
   `JourneyError::UnknownAttributePath(Vec<AttributePath>)`.
5. For every secret path of shape `persons/<ref>/…`, resolve `<ref>` →
   `slot.subject_id`. If the slot does not exist, return
   `PersonNotFound(<ref>)`.
6. Reject if the resolved secret subjects are not all the same single
   subject (`MultiSubjectNotSupported`). This is the v1 restriction.
7. Validate plaintext leaves against the JSON Schema (if provided) by
   building a rehydrated tree from the union of current `shared_data` and
   the new plaintext changes, then calling `services.schema_validator()`.
8. Call `services.decision_engine().evaluate_next_steps(...)` with the new
   path-keyed shape (see A5.5 below).
9. Emit one `AttributesSet { plaintext, secret, subject_id }` event then
   one `WorkflowEvaluated { suggested_actions }`. No `StepProgressed`.

#### A5.4 — `JourneyServices`

Extend with an `attribute_schema: Arc<AttributeSchema>` and an
accessor. Update the constructor and **all** call sites (`state.rs`,
tests, integration tests) to pass an `AttributeSchema`. In tests and in
`state.rs`, use a permissive default (every conceivable path classified as
`Plaintext` — i.e., `AttributeSchema::permissive()`). Add this constructor.

> The flight-booking example will later supply a real schema; for now
> `state.rs` builds a permissive one so the binary keeps booting.

#### A5.5 — Decision engine: new entry point

Add a method on the trait (default-implemented for back-compat):

```rust
async fn evaluate_attributes(
    &self,
    journey: &Journey,
    pending_changes: &BTreeMap<AttributePath, Value>,
) -> Result<WorkflowDecision, …> {
    // default: rehydrate, then route through existing evaluate_next_steps
    // using current_step = "" and merged tree as new_data.
}
```

Both `SimpleDecisionEngine` and `GoRulesDecisionEngine` keep their existing
`evaluate_next_steps` for `Capture`. The new arm uses
`evaluate_attributes`. Phase B will refine the GoRules side to read flat
paths properly.

#### A5.6 — Aggregate `apply`

Add an `AttributesSet` arm:

- For each plaintext path: `json_path::set_at_path(&mut self.shared_data,
  &path, value.clone())`.
- For each secret path under `persons/<ref>/…`: parse out `<ref>`, look up
  the slot. **Temporary dual-write**: also mirror into the existing
  `slot.details` blob using the suffix path. This keeps the legacy view
  rows useful through Phase A/B. Remove in Phase C.

#### A5.7 — Tests

Mirror the existing `domain/journey.rs` test module. Add tests:

- `set_attributes_requires_started`
- `set_attributes_rejects_after_complete`
- `set_attributes_rejects_unknown_path` (configure schema)
- `set_attributes_plaintext_merges_into_shared_data`
- `set_attributes_secret_requires_person_captured`
- `set_attributes_secret_writes_under_slot`
- `set_attributes_emits_workflow_evaluated`
- `set_attributes_rejects_multi_subject`
- `set_attributes_invalid_data_against_json_schema`

**Validation.** `cargo test --workspace`.

**Done when.** All new tests green; all old tests still green; the
`Capture`/`CapturePersonDetails` flows still work end-to-end through the
HTTP route.

---

### A6. Path-keyed PII codec for `AttributesSet`

**Pre-flight.** Read `crates/journey_dynamics/src/pii_codec.rs` and the
generated impl for `PersonDetailsUpdated` (search for `PersonDetailsUpdated`
under `crates/cqrs-es-crypto-derive`).

**Do.**

- Decide between option (1) extending the derive macro with
  `#[pii(by_path)]` or (2) hand-writing the codec arm. **Recommendation:
  hand-write** — see the design doc's rationale (the macro is for
  field-fixed PII, path-keyed is data-driven).

- If hand-writing: replace `#[derive(PiiCodec)]` on `JourneyEvent` with a
  hand-written `impl PiiEventCodec for JourneyEvent { … }` that:

  - Reuses the macro output for the existing variants (copy-and-paste from
    `cargo expand`, then clean up).
  - Adds an `AttributesSet` arm that:
    - `classify` returns `Pii` iff `subject_id.is_some() && !secret.is_empty()`.
    - `extract_secret` returns the JSON-serialised `secret` map and the
      `subject_id`.
    - `reconstruct` reads the decrypted JSON back into `secret` and clears
      the inline `secret` field on the serialized event (replaced by the
      ciphertext sub-payload).
    - `redact` empties `secret` and replaces it with a `{"redacted":
      true}` marker per the project's existing redaction convention.

- New unit tests in `pii_codec.rs`:

  - `test_attributes_set_passes_through_when_no_secret`
  - `test_attributes_set_encrypts_secret_partition_under_subject_dek`
  - `test_attributes_set_decrypts_secret_partition`
  - `test_attributes_set_redacted_when_dek_deleted`
  - `test_attributes_set_aad_binds_to_event_position`

**Validation.** `cargo test -p journey_dynamics pii_codec`.

**Done when.** New codec tests pass; all existing codec tests (for
`PersonCaptured`, `PersonDetailsUpdated`, etc.) still pass.

---

### A7. HTTP extractor and route accept `SetAttributes`

**Pre-flight.** Read `command_extractor.rs`, `route_handler.rs`.

**Do.**

- No code change strictly required (the existing extractor uses serde and
  will pick up the new variant automatically). Verify by adding an
  end-to-end test that POSTs:

  ```json
  { "SetAttributes": { "changes": {
      "search/origin": "LHR",
      "search/destination": "JFK"
  } } }
  ```

  to `/journeys/{id}` and asserts a 204 and that `shared_data` contains
  `{"search":{"origin":"LHR","destination":"JFK"}}`.

- Add the test under `crates/journey_dynamics/tests/` as a unit-style test
  that goes through the route handler with an in-memory state (do not
  introduce a new HTTP integration test framework).

- Document the ergonomic nested-form decoder as a follow-up (Phase B step
  B4); not required to land here.

**Validation.** `cargo test --workspace`. Manually `cargo run -p
journey_dynamics` and `curl` once if a developer is on the task; not
required for the agent.

**Done when.** New test passes; both `Capture` and `SetAttributes` POSTs
are accepted by the same route.

---

## Phase B — Flip the example, narrow the surface

### B1. Add `phase` to `WorkflowEvaluated` and write it on the new path only

**Pre-flight.** Read step A4's `TODO(path-keyed-step-B3)` markers — this is
that step (numbered B1 here so the ordering is correct).

**Do.**

- Extend `JourneyEvent::WorkflowEvaluated` with `phase: Option<String>`.
- Bump `event_version()` for `WorkflowEvaluated` to `"1.1"`. Old `1.0`
  payloads (no `phase`) deserialise to `phase: None` thanks to
  `#[serde(default)]` — add that attribute.
- The aggregate's new `SetAttributes` arm writes `WorkflowEvaluated {
  phase: decision.phase, suggested_actions: decision.suggested_actions }`.
- The legacy `Capture` arm continues to write `phase: None`.
- `View::update` writes the new `phase` column.
- Remove the `TODO` markers from step A4.

**Validation.** `cargo test --workspace`; replay tests using stored
fixtures of the old payload shape (add at least one fixture under
`tests/fixtures/events/workflow_evaluated_v1_0.json`).

**Done when.** Old `1.0` events still load; new events carry `phase`.

---

### B2. Event up-caster for legacy events

**Pre-flight.** Read the design doc's "Migration strategy" section. Skim
`cqrs-es-crypto::repository.rs` for the read path.

**Do.**

- Add a thin layer between the event store and the aggregate that, on
  read, translates:
  - `JourneyEvent::Modified { step, data }` → `JourneyEvent::AttributesSet
    { plaintext: flatten(data) namespaced under "<step>/...", secret:
    empty, subject_id: None }`.
  - `JourneyEvent::PersonDetailsUpdated { person_ref, subject_id, data }`
    → `JourneyEvent::AttributesSet { plaintext: empty, secret:
    flatten(data) namespaced under "persons/<ref>/...", subject_id:
    Some(subject_id) }`.
  - `JourneyEvent::StepProgressed { … }` → **dropped** (no longer needed;
    `current_step` removal happens in Phase C, but the event is replay-only
    harmless until then — see "Done when").

- Implement as a function `upcast_event(SerializedEvent) -> SerializedEvent`
  invoked in a wrapping `PersistedEventRepository` adapter. Place under
  `crates/journey_dynamics/src/event_upcaster.rs`.

- Tests:

  - Fixture round-trip: a recorded `Modified` event from before this PR
    loads as `AttributesSet`.
  - A recorded `PersonDetailsUpdated` event with a known DEK decrypts and
    up-casts to `AttributesSet` with the right `secret` map.
  - A recorded `PersonDetailsUpdated` event whose DEK has been deleted
    redacts to the up-cast `AttributesSet` with the redacted marker on
    `secret`.

- Wire the up-caster into `state.rs` (`new_application_state`).

**Validation.** `cargo test --workspace`.

**Done when.** Replaying a journey created via `Capture` /
`CapturePersonDetails` produces the same aggregate state through the
up-caster as it does today via direct apply.

---

### B3. Port `flight-booking` to `SetAttributes`

**Pre-flight.** Read `examples/flight-booking/src/lib.rs`,
`examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json`,
`examples/flight-booking/schemas/flight-booking-schema.json`.

**Do.**

- Build a `flight_booking::attribute_schema()` factory returning the
  project-wide `AttributeSchema`. Classify:
  - `search/*`, `searchResults/*`, `booking/*` → `Plaintext`.
  - `persons/<ref>/firstName`, `lastName`, `dateOfBirth`, `passportNumber`,
    `nationality` → `Secret { subject: persons/<ref>/subject_id }`.
  - `persons/<ref>/passengerType` → `Plaintext`.

- Update `state.rs` (in the `journey_dynamics` crate, where the binary is
  wired) to load this schema by default; for unit tests, keep using the
  permissive schema.

  > If the binary should remain example-agnostic, expose an env var
  > `JOURNEY_ATTRIBUTE_SCHEMA_PATH` and have `state.rs` load JSON from
  > there.

- Update the JDM orchestrator to:
  - Read from flat paths: top-level keys are now `search`, `searchResults`,
    `booking`, `persons`. Re-confirm ZEN expression syntax for nested
    objects.
  - Emit both `suggestedActions` and `phase`.
  - Drop the `currentStep` input. Replace with derivations off the bag,
    e.g. `phase = "collecting_search" if search.origin == null else
    "collecting_passengers" if len(persons) == 0 else …`.

- Drop `passengers_ready` and `has_unaccompanied_minors` from `BookingData`
  (these were summary fields the application had to compute). Have the JDM
  rule read them directly from `persons/*/passengerType`.

- Update `examples/flight-booking/src/tests.rs` accordingly.

- Add a `TryFrom<&JourneyView>` impl producing `FlightBookingSchema` from
  the path-keyed bag. Use `json_path::get_at_path` for each field.

**Validation.** `cargo check -p flight-booking --all-targets && cargo test
-p flight-booking`.

**Done when.** The example builds, its tests pass, and a hand-driven curl
script POSTing `SetAttributes` reaches `phase: "ready_to_pay"` (document
the script under `examples/flight-booking/SCHEMA_USAGE.md`).

---

### B4. HTTP nested-form sugar (optional)

**Pre-flight.** Read `command_extractor.rs`.

**Do.**

- Accept an alternative wire form on `SetAttributes`:

  ```json
  { "SetAttributes": { "search": { "origin": "LHR" } } }
  ```

  by detecting in the extractor that the inner object is **not** an object
  with a single `changes` key and flattening it via `json_path::flatten`.
  Keep the explicit `{ "changes": { … } }` form as the canonical one.

- Test both forms in `command_extractor.rs` tests.

**Validation.** `cargo test --workspace`.

**Done when.** Both nested-sugar and explicit `changes` forms are accepted
and reach the aggregate as the same command.

---

### B5. Mark old surface deprecated; update docs

**Pre-flight.** Run `cargo doc` and grep for any remaining references to
`Capture`, `CapturePersonDetails`, `current_step` in docs.

**Do.**

- Add `#[deprecated(since = "0.3.0", note = "use SetAttributes")]` to
  `JourneyCommand::Capture` and `JourneyCommand::CapturePersonDetails`
  (already pending from A5).
- Update `README.md` (workspace-level) and `examples/flight-booking/*.md`
  to show the new flow.
- Update `docs/QUICK_START.md`.
- Add a `CHANGELOG.md` entry under "Unreleased" describing the
  command/event additions and the deprecation.

**Validation.** `cargo build --workspace` (lints should warn about the
deprecated usages in any leftover internal callers — clean those up).

**Done when.** No internal code references the deprecated variants except
in the up-caster.

---

## Phase C — Remove the legacy model

> Do not begin Phase C until Phase B has soaked for at least one release
> cycle. Each step below assumes Phase B is complete and no production
> caller is still emitting `Capture` / `CapturePersonDetails`.

### C1. Remove `Capture` and `CapturePersonDetails` commands

**Pre-flight.** Grep for `JourneyCommand::Capture` and
`JourneyCommand::CapturePersonDetails`.

**Do.**

- Delete the variants from `domain/commands.rs`.
- Delete the matching `handle` arms in `domain/journey.rs`.
- Delete the legacy aggregate tests (`modify_journey`,
  `capture_form_data_*`, `test_capture_person_details_*`, etc.). Keep tests
  whose behaviour is now covered by `set_attributes_*` cases.

**Validation.** `cargo test --workspace`.

**Done when.** No reference to the removed variants outside the
event-upcaster.

---

### C2. Remove `Modified`, `PersonDetailsUpdated`, `StepProgressed` event variants

**Pre-flight.** Confirm the up-caster from B2 fully translates these.

**Do.**

- Delete the three variants from `domain/events.rs`.
- Delete the matching `apply` arms.
- Update `View::update` to delete the matching arms.
- The up-caster (`event_upcaster.rs`) keeps generating `AttributesSet`
  *before* deserialisation into the enum, so removing the enum variants is
  safe.
- Drop the codec arms (or, if hand-written in A6, delete them).
- Delete fixture round-trip tests that asserted these variants existed in
  the enum; keep the up-cast tests that work at the
  `SerializedEvent`-level.

**Validation.** `cargo test --workspace`.

**Done when.** The `JourneyEvent` enum has only:
`Started`, `AttributesSet`, `PersonCaptured`, `WorkflowEvaluated`,
`Completed`, `SubjectForgotten`.

---

### C3. Remove `current_step` from aggregate, view, and database

**Pre-flight.** Search for `current_step`.

**Do.**

- Drop the field from `Journey`, `JourneyView`, `Default for Journey`,
  `Journey::current_step()` accessor.
- Drop the `current_step` column from `journey_view` via a new migration
  `migrations/2026MMDDHHMMSS_drop_current_step.up.sql`:

  ```sql
  ALTER TABLE journey_view DROP COLUMN current_step;
  ```

  and matching `.down.sql`.

- Update `view_repository.rs` `SELECT` lists and `INSERT/UPDATE` clauses.
- Remove `current_step` from all serde-deserialised test fixtures (the
  field is now absent; add `#[serde(default)]` on
  `JourneyView::shared_data`-adjacent fields if needed).

**Validation.** `cargo test --workspace` + Postgres integration tests.

**Done when.** Grep for `current_step` returns zero hits in `src/` and
`tests/`.

---

### C4. Remove `PersonSlot.details` and the legacy mirror-write in `apply`

**Pre-flight.** Review the temporary mirror-write added in A5.6.

**Do.**

- Delete `details: Value` from `PersonSlot`.
- Delete the mirror-write branch in the `apply` `AttributesSet` arm — per-
  person attributes now live exclusively under `persons/<ref>/…` in
  `shared_data`.
- Drop the `details` column from `journey_person` via a new migration:

  ```sql
  ALTER TABLE journey_person DROP COLUMN details;
  ```

- Update `PersonView`, `view_repository.rs`, and any projector code.
- Update the up-caster (B2) so that `PersonDetailsUpdated`-derived
  `AttributesSet` events project the secret partition into `shared_data`
  under `persons/<ref>/…`, not into `slot.details`.

**Validation.** `cargo test --workspace` + integration tests.

**Done when.** `PersonSlot` carries only identity fields (`name`, `email`,
`phone`, `subject_id`, `forgotten`).

---

### C5. Drop legacy event indexes; replace with one for `AttributesSet`

**Pre-flight.** Inspect indexes created by
`migrations/20260423132137_init.up.sql`.

**Do.**

- New migration:

  ```sql
  DROP INDEX IF EXISTS idx_events_person_captured_subject;
  DROP INDEX IF EXISTS idx_events_person_details_updated_subject;
  CREATE INDEX idx_events_attributes_set_subject
      ON events ((payload -> 'AttributesSet' ->> 'subject_id'))
      WHERE event_type = 'AttributesSet';
  ```

  Keep `PersonCaptured` indexed (it still exists).

- Update any `find_journeys_by_subject` query in `queries.rs` /
  `view_repository.rs` to include the new index path.

**Validation.** Postgres integration test
`postgres_subject_lookup_hook.rs` still finds journeys via both
`PersonCaptured` and `AttributesSet`.

**Done when.** Index exists and integration test passes.

---

### C6. Remove the up-caster (optional, only if no historical events remain)

**Pre-flight.** Confirm with operators that no legacy events remain in
production. If unsure, keep the up-caster indefinitely — it is cheap.

**Do.**

- Delete `event_upcaster.rs` and its wiring in `state.rs`.
- Delete its tests.

**Validation.** `cargo test --workspace`.

**Done when.** No `event_upcaster` module exists.

---

## Cross-cutting checklists

### After every step

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo build -p journey_dynamics --bin journey_dynamics`
- [ ] `cargo check -p flight-booking --all-targets`

### At phase boundaries

- [ ] Run `postgres_view_repository.rs` and
      `postgres_subject_lookup_hook.rs` against a fresh database produced
      from current migrations.
- [ ] Boot the binary and exercise `/journeys` end-to-end with `curl`.
- [ ] Update `CHANGELOG.md` and bump `version` in `Cargo.toml`.

### Things to defer (explicit non-goals of this plan)

- **Multi-subject events in one `SetAttributes`.** Step A5 rejects them
  with `MultiSubjectNotSupported`. Lifting this requires the
  `cqrs-es-crypto` crate to support a vector of `(subject_id,
  ciphertext)` per event (see the design doc, "Multi-subject events").
  Track as a follow-up RFC.
- **Snapshot encryption** (carried over from `cqrs-es-crypto`'s known
  limitations).
- **JDM ergonomics for deeply-nested flat paths.** B3 confirms feasibility
  for the depth used in flight-booking; richer ergonomics (e.g. `$path`
  selectors) are a separate workstream.

---

## Suggested PR cadence

| PR | Steps | Reviewer focus                                  |
| -- | ----- | ----------------------------------------------- |
| 1  | A1, A2, A3 | Pure additive types and helpers. Skim, ✅. |
| 2  | A4    | Schema migration + serde defaults review.       |
| 3  | A5    | Aggregate semantics, error taxonomy.            |
| 4  | A6    | Cryptography. Senior review required.           |
| 5  | A7    | API surface; light.                             |
| 6  | B1, B2 | Event versioning and up-caster correctness.    |
| 7  | B3    | Example behaviour change; product review too.   |
| 8  | B4, B5 | Docs.                                          |
| 9  | C1, C2 | Removal of legacy command/event variants.      |
| 10 | C3, C4, C5 | Schema migrations.                         |
| 11 | C6 (if scheduled) | Final cleanup.                      |

---

## Glossary

- **Bag.** The journey's accumulated `(path, value)` document.
- **Path.** A `/`-separated key into the bag, e.g.
  `persons/passenger_0/passportNumber`.
- **Classification.** A per-path decision: `Plaintext` (stored verbatim) or
  `Secret { subject }` (encrypted under the DEK of the subject at the
  referenced path).
- **Up-caster.** A read-side adapter that translates legacy event variants
  into the new path-keyed shape before the aggregate sees them.
- **Phase.** A coarse derived label (e.g. `collecting_search`) computed
  from the bag by the decision engine. Not part of the command surface.
