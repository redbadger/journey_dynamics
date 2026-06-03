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
| **A** | Refactor `cqrs-es-crypto` to support multi-subject partitioned ciphertext (back-compatible on read). Introduce new types and the `SetAttributes` command **alongside** existing commands. Old code paths remain fully functional. |
| **B** | Switch the flight-booking example and HTTP surface to the new model. Documentation refresh. |
| **C** | Mark the legacy command/event/field surface `#[deprecated]`. **Nothing is removed.** Existing callers continue to compile and run; the compiler emits warnings nudging them to the new surface. Eventual removal is out of scope for this plan and would happen in a later release. |

After Phase A every test continues to pass. After Phase B the example uses
`SetAttributes` exclusively but `Capture`/`CapturePersonDetails` still work for
back-compat. After Phase C the legacy surface is still fully functional but
emits deprecation warnings at compile time.

### Conventions

- Each step starts with **Pre-flight** (what to read) and ends with
  **Validation** (what to run) and **Done when** (acceptance).
- Steps inside a phase are ordered; do not reorder unless explicitly noted.
- Phase C marks the legacy surface `#[deprecated]` but does not remove it.
  Internal usages of deprecated items inside the crate must be wrapped in
  `#[allow(deprecated)]` so the crate builds clean.
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

### A5. Refactor `cqrs-es-crypto` to multi-subject partitioned ciphertext

**Pre-flight.** Read `crates/cqrs-es-crypto/src/lib.rs`,
`crates/cqrs-es-crypto/src/repository.rs`,
`crates/cqrs-es-crypto/src/cipher.rs`, the `PiiEventCodec` trait, and the
derive expansion for `PersonCaptured` (`cargo expand -p journey_dynamics
--lib pii_codec`).

**Goal.** Change the crate's on-disk envelope from "one `(subject_id,
ciphertext)` per event" to "`Vec<{ subject_id, label, ciphertext }>` per
event", in a way that is backward-compatible on the **read** path — legacy
single-blob events still decrypt and redact correctly through the new code.
This step is purely a `cqrs-es-crypto` + `cqrs-es-crypto-derive` change; no
`journey_dynamics` semantics shift.

**Do.**

#### A5.1 — New public types

```rust
pub struct SecretPartition {
    /// Subject whose DEK encrypts this partition.
    pub subject_id: Uuid,
    /// Caller-supplied label routing the cleartext bytes back into the
    /// event on read. Opaque to the crypto layer (e.g. a field name or a
    /// person_ref). Must be unique within an event.
    pub label: String,
    /// Cleartext bytes (typically JSON) to be encrypted.
    pub payload: Vec<u8>,
}

pub struct EncryptedPartition {
    pub subject_id: Uuid,
    pub label: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

pub struct DecryptedPartition {
    pub subject_id: Uuid,
    pub label: String,
    pub payload: Vec<u8>,
}
```

#### A5.2 — `PiiEventCodec` trait

Replace single-subject extract/reconstruct with partition-aware methods:

```rust
pub trait PiiEventCodec: Send + Sync {
    /// Identify partitions to encrypt. Empty vec = pure-plaintext event.
    fn extract_partitions(&self, event: &SerializedEvent)
        -> Result<Vec<SecretPartition>, PiiCodecError>;

    /// Reattach decrypted partitions to a serialized event by `label`.
    fn reconstruct(&self, event: &mut SerializedEvent,
        partitions: Vec<DecryptedPartition>) -> Result<(), PiiCodecError>;

    /// Redact partitions whose DEK has been deleted. The codec decides
    /// the per-label sentinel shape; the repository never invents content.
    fn redact_partitions(&self, event: &mut SerializedEvent,
        labels: &[String]) -> Result<(), PiiCodecError>;
}
```

Provide a `SingleSubjectCodec` adapter trait (default-implemented over the
new trait) so hand-rolled callers that still think in single-subject terms
can be lifted unchanged with `label = "default"`.

#### A5.3 — On-disk envelope

The encrypted event payload becomes:

```json
{
  "EventType": {
    "...plaintext fields...": "...",
    "subjects":              ["<uuid>", "..."],
    "encrypted_partitions":  [
      { "subject_id": "<uuid>",
        "label":      "<string>",
        "nonce":      "<b64>",
        "ciphertext": "<b64>" }
    ]
  }
}
```

- `subjects` is a plaintext peer array maintained by the repository (not
  by the codec) for indexing.
- `encrypted_partitions` is the canonical secret carrier.
- The legacy `encrypted_data` / inline `subject_id` shape is recognised on
  read only (see A5.4) and is never emitted by new writes.

#### A5.4 — Read-path back-compat

The repository detects the legacy shape — a top-level `subject_id` field
plus a single secret field (per the old codec's `extract_secret`) — and
translates it on the fly to a one-element partition vector with `label =
"default"`. The codec sees only the new shape. Add a fixture-based test:
 a legacy `PersonCaptured` or `PersonDetailsUpdated` payload (captured from
current main) round-trips through decrypt and redact without alteration.

#### A5.5 — Per-partition AAD

Each partition's AAD is the concatenation `aggregate_id || sequence ||
subject_id || label`. This makes partitions non-fungible across events,
across subjects in the same event, **and** across labels (preventing
intra-event swap attacks). Update `cipher.rs` to thread the AAD per
partition. Add a test asserting that swapping two partitions' bytes within
one event fails the GCM tag check.

#### A5.6 — Multi-partition decrypt and redact

`CryptoShreddingEventRepository::get_events` (and friends) iterates
partitions on read. For each: try `KeyStore::get_key(subject_id)`. If
`NotFound`, the partition's label is collected into a `redacted_labels`
list passed to `redact_partitions` after the decryptable partitions have
been reattached. Surviving partitions decrypt normally.

Add a test: a two-partition event where one subject has been
crypto-shredded — assert the surviving partition decrypts intact and the
deleted one carries the codec-defined sentinel.

#### A5.7 — Derive macro update

The macro currently generates single-subject extract/reconstruct from
`#[pii(subject)]` + `#[pii(secret)]` fields. Update its code-gen to emit
`extract_partitions` returning a `Vec<SecretPartition>` of length 0 or 1
(0 when `subject_id.is_none()`, 1 with `label = "default"` otherwise), and
an `extract_partitions`-mirrored `reconstruct`. The existing
`PersonCaptured` / `PersonDetailsUpdated` derives continue to compile
unchanged. `redact_partitions` reuses the existing sentinel rules per
field.

#### A5.8 — CHANGELOG, README, version

- Splice the prepared content from
  [`CQRS_ES_CRYPTO_PARTITIONS_ADR.md`](./CQRS_ES_CRYPTO_PARTITIONS_ADR.md)
  into `crates/cqrs-es-crypto/README.md` per the placement notes at the
  bottom of that document. After folding in, delete the staging document.
- Add the prepared CHANGELOG entry (also in the staging document) under
  `## [Unreleased]` in `crates/cqrs-es-crypto/CHANGELOG.md`.
- Bump `crates/cqrs-es-crypto/Cargo.toml` and
  `crates/cqrs-es-crypto-derive/Cargo.toml` minor versions. The workspace
  package version (`Cargo.toml`) stays put.

#### A5.9 — Tests

- All existing `cqrs-es-crypto` and `cqrs-es-crypto-derive` tests pass
  unchanged.
- New tests: legacy-shape read, multi-partition write/read, partial
  shredding (one subject deleted, one intact), AAD swap-detection.
- All existing `journey_dynamics::pii_codec` tests still pass — the
  `PersonCaptured`/`PersonDetailsUpdated` codec arms work via the
  unchanged derive macro contract.

**Validation.** `cargo test -p cqrs-es-crypto -p cqrs-es-crypto-derive -p
journey_dynamics`.

**Done when.** Workspace tests pass; events written before this step still
decrypt; the new partitioned shape round-trips end-to-end; the README and
CHANGELOG document the format change.

---

### A6. Add `SetAttributes` command and `AttributesSet` event (parallel to existing)

**Pre-flight.** Read `domain/commands.rs`, `domain/events.rs`,
`domain/journey.rs::handle`, `domain/journey.rs::apply`,
`crates/journey_dynamics/src/pii_codec.rs`.

**Do.**

#### A6.1 — Wire format

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

Note: a single `SetAttributes` may touch attributes for **multiple
subjects** (e.g. two passengers' passport numbers in one form submission).
The aggregate accepts this; the codec encrypts each subject's slice under
its own DEK as a separate partition (see A5).

#### A6.2 — Event

Hand-written (not derived) — see step A7. Conceptual shape:

```rust
pub enum JourneyEvent {
    // …
    AttributesSet {
        /// Path → value changes that are not classified as Secret.
        plaintext: BTreeMap<AttributePath, Value>,
        /// One entry per subject whose data is touched by this command.
        /// Empty when the command set only plaintext attributes.
        secret_partitions: Vec<SecretPartitionData>,
    },
    // …
}

pub struct SecretPartitionData {
    /// Journey-local slot name; used as the codec `label`.
    pub person_ref: String,
    /// The subject's identity, copied from `PersonSlot.subject_id`.
    pub subject_id: Uuid,
    /// Path → value changes encrypted under `subject_id`'s DEK.
    pub changes: BTreeMap<AttributePath, Value>,
}
```

Update `event_type()` (`"AttributesSet"`) and `event_version()` (`"1.0"`).
Mark with `#[serde(default)]` on `secret_partitions` so future shape
tweaks remain forward-compatible.

#### A6.3 — Aggregate `handle`

Add a `JourneyCommand::SetAttributes { changes } =>` arm. It must:

1. Reject if journey not started (`NotFound`).
2. Reject if `Complete` (`AlreadyCompleted`).
3. Reject if `changes` is empty (`InvalidData("no changes")`).
4. Compute the path classification using `services.attribute_schema()`
   (see A6.4). Unknown paths → return
   `JourneyError::UnknownAttributePath(Vec<AttributePath>)`.
5. For every secret path under `persons/<ref>/…`, resolve `<ref>` →
   `slot.subject_id`. If the slot does not exist, return
   `PersonNotFound(<ref>)`. Group the resolved secret changes by
   `(person_ref, subject_id)` into one `SecretPartitionData` per person.
6. Validate plaintext leaves against the JSON Schema (if provided) by
   building a rehydrated tree from the union of current `shared_data` and
   the new plaintext changes, then calling `services.schema_validator()`.
7. Call `services.decision_engine().evaluate_attributes(...)` with the
   merged bag (see A6.5).
8. Emit one `AttributesSet { plaintext, secret_partitions }` event
   followed by one `WorkflowEvaluated { suggested_actions }`. No
   `StepProgressed`.

#### A6.4 — `JourneyServices`

Extend with an `attribute_schema: Arc<AttributeSchema>` and an
accessor. Update the constructor and **all** call sites (`state.rs`,
tests, integration tests) to pass an `AttributeSchema`. In tests and in
`state.rs`, use a permissive default (every conceivable path classified as
`Plaintext` — i.e., `AttributeSchema::permissive()`). Add this constructor.

> The flight-booking example will later supply a real schema; for now
> `state.rs` builds a permissive one so the binary keeps booting.

#### A6.5 — Decision engine: new entry point

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

#### A6.6 — Aggregate `apply`

Add an `AttributesSet` arm:

- For each plaintext path: `json_path::set_at_path(&mut self.shared_data,
  &path, value.clone())`.
- For each `SecretPartitionData`, iterate its `changes`:
  - Look up the slot by `person_ref`. (It must exist because `handle`
    enforced this, but defensive: skip if missing.)
  - Write each path/value into `shared_data` under `persons/<ref>/…` using
    `set_at_path`.
  - **Permanent mirror-write**: also merge into the existing
    `slot.details` blob using the suffix path. This keeps the legacy
    `journey_person.details` column populated for downstream consumers
    that still read from it. The mirror-write is the bridge between the
    new write surface and the legacy read surface and is retained for as
    long as the legacy view fields exist (i.e. indefinitely under this
    plan).

#### A6.7 — Tests

Mirror the existing `domain/journey.rs` test module. Add tests:

- `set_attributes_requires_started`
- `set_attributes_rejects_after_complete`
- `set_attributes_rejects_empty_changes`
- `set_attributes_rejects_unknown_path` (configure schema)
- `set_attributes_plaintext_merges_into_shared_data`
- `set_attributes_secret_requires_person_captured`
- `set_attributes_secret_writes_under_slot`
- `set_attributes_emits_workflow_evaluated`
- `set_attributes_multi_subject_produces_one_partition_per_subject`
- `set_attributes_invalid_data_against_json_schema`

**Validation.** `cargo test --workspace`.

**Done when.** All new tests green; all old tests still green; the
`Capture`/`CapturePersonDetails` flows still work end-to-end through the
HTTP route.

---

### A7. Path-keyed multi-partition codec for `AttributesSet`

**Pre-flight.** Read `crates/journey_dynamics/src/pii_codec.rs`, the
expanded derive output for the existing variants (via `cargo expand`), and
the new `PiiEventCodec` trait shape introduced in A5.

**Do.**

- Replace `#[derive(PiiCodec)]` on `JourneyEvent` with a hand-written
  `impl PiiEventCodec for JourneyEvent { … }`.

  - Reuse the macro's emitted logic for `PersonCaptured` and
    `PersonDetailsUpdated` (copy from `cargo expand`, then clean up).
    These each contribute zero or one partition with `label = "default"`.
  - Add an `AttributesSet` arm:
    - **`extract_partitions`**: for each `SecretPartitionData` in
      `secret_partitions`, emit a `SecretPartition { subject_id, label:
      person_ref.clone(), payload: serde_json::to_vec(&changes)? }`.
      Returns an empty `Vec` when `secret_partitions` is empty.
    - **`reconstruct`**: bucket the incoming `Vec<DecryptedPartition>` by
      `label`. For each, deserialise the payload bytes back into
      `BTreeMap<AttributePath, Value>` and write it into the matching
      `SecretPartitionData.changes` field (keyed by `person_ref`).
    - **`redact_partitions`**: for each label in the redaction list,
      replace the matching `SecretPartitionData.changes` with a single
      entry `{ AttributePath("redacted") => Value::Bool(true) }`,
      mirroring the existing project sentinel convention.

- The hand-written impl is the single source of truth for `JourneyEvent`
  going forward; the derive macro is no longer used for this enum.

- New unit tests in `pii_codec.rs`:

  - `test_attributes_set_passes_through_when_no_secret_partitions`
  - `test_attributes_set_encrypts_each_partition_under_its_own_dek`
  - `test_attributes_set_decrypts_multi_subject_partitions`
  - `test_attributes_set_partial_shred_keeps_intact_partitions`
    (subject A's DEK deleted, subject B's intact — only A's changes are
    redacted)
  - `test_attributes_set_aad_binds_partition_to_subject_and_label`

**Validation.** `cargo test -p journey_dynamics pii_codec`.

**Done when.** New multi-partition tests pass; all existing
`PersonCaptured` / `PersonDetailsUpdated` codec tests still pass; the
`subjects` plaintext peer array is populated correctly by the repository
for downstream indexing.

---

### A8. Subject indexing for `AttributesSet`; HTTP extractor accepts the new command

**Pre-flight.** Read `command_extractor.rs`, `route_handler.rs`,
`view_repository.rs` (subject-lookup queries), and
`migrations/20260423132137_init.up.sql`.

**Do.**

- **Migration** `migrations/2026MMDDHHMMSS_attributes_set_index.up.sql`:

  ```sql
  CREATE INDEX idx_events_attributes_set_subjects
      ON events USING GIN ((payload -> 'AttributesSet' -> 'subjects'))
      WHERE event_type = 'AttributesSet';
  ```

  and matching `.down.sql`.

- Update `view_repository.rs::find_journeys_by_subject` (and any sibling
  query) so that subject lookup unions across:

  - `event_type = 'PersonCaptured'` with the existing
    `payload -> 'PersonCaptured' ->> 'subject_id'` index (legacy and
    current).
  - `event_type = 'AttributesSet'` with the new GIN index on
    `payload -> 'AttributesSet' -> 'subjects'`.

  Use an array-containment predicate: `payload -> 'AttributesSet' ->
  'subjects' @> jsonb_build_array($1::text)`.

- HTTP extractor: no code change strictly required (serde picks up the new
  variant automatically). Verify by adding a unit-style end-to-end test
  under `crates/journey_dynamics/tests/` that POSTs:

  ```json
  { "SetAttributes": { "changes": {
      "search/origin": "LHR",
      "search/destination": "JFK"
  } } }
  ```

  to `/journeys/{id}` and asserts a 204 and that `shared_data` contains
  `{"search":{"origin":"LHR","destination":"JFK"}}`.

- Defer the nested-form sugar to B4.

**Validation.** `cargo test --workspace`. Run
`postgres_subject_lookup_hook.rs` if Postgres is available, asserting
lookups resolve through both index paths.

**Done when.** Both `Capture` and `SetAttributes` POSTs are accepted at
the same route; subject lookups locate journeys that touched the subject
via either `PersonCaptured` or `AttributesSet`.

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

### B2. Port `flight-booking` to `SetAttributes`

**Pre-flight.** Read `examples/flight-booking/src/lib.rs`,
`examples/flight-booking/jdm-models/flight-booking-orchestrator.jdm.json`,
`examples/flight-booking/schemas/flight-booking-schema.json`.

> **Note on event up-casting.** Earlier drafts of this plan included an
> event up-caster step here (B2). It has been removed: because the
> aggregate keeps its legacy `apply` arms for `Modified`,
> `PersonDetailsUpdated`, and `StepProgressed` (see Phase C, which only
> deprecates these variants), historical events continue to replay
> directly without translation. The up-caster is no longer on the critical
> path. It can be reintroduced as an internal optimisation if and when the
> legacy variants are eventually removed in a future release.

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

### B3. HTTP nested-form sugar (optional)

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

### B4. Documentation refresh

**Pre-flight.** Run `cargo doc` and grep for any remaining references to
`Capture`, `CapturePersonDetails`, `current_step` in docs (not in code —
that's Phase C). Read
[`PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](./PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md);
it is the downstream-consumer-facing artefact and must be accurate by
the end of this step.

**Do.**

- Update `README.md` (workspace-level) and `examples/flight-booking/*.md`
  to show the new flow as the recommended one; demote (but do not delete)
  the legacy examples to a "Legacy API (deprecated)" subsection.
- Update `docs/QUICK_START.md`.
- Review `PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md` against the actual
  shipped types and field names. Update any code snippets that drifted
  during implementation (the guide was written ahead of code; expect
  one or two name tweaks).
- Add a `CHANGELOG.md` entry under "Unreleased" describing the
  command/event additions and linking to the migration guide. Defer
  the deprecation entry to step C0.
- From the workspace `README.md`, link to the migration guide above
  the legacy examples so external readers find it before they copy
  outdated code.

**Validation.** `cargo build --workspace` succeeds (no deprecation
warnings yet — those land in Phase C). Manually skim the migration
guide for stale code snippets.

**Done when.** Docs prominently feature `SetAttributes`/`AttributesSet`,
the migration guide is accurate against shipped code, and the legacy
flow is documented but explicitly marked as deprecated-in-Phase-C.

---

## Phase C — Deprecate the legacy surface

> **No code is removed in this phase.** Every legacy command variant,
> event variant, struct field, accessor, database column, and migration
> stays in place and continues to work. Phase C only adds `#[deprecated]`
> markers so external callers get compile-time nudges toward the new
> surface.
>
> Internal usages of deprecated items inside `journey_dynamics` (apply
> arms, view projections, tests for the legacy paths) must be wrapped in
> `#[allow(deprecated)]` so the crate itself builds clean.
>
> Future removal of the legacy surface is **explicitly out of scope** for
> this plan and belongs in a separate RFC once usage has been measured.

### C0. Choose deprecation metadata and add a `CHANGELOG` entry

**Pre-flight.** Decide the `since` version (the next release that ships
these deprecations) and the standard `note` wording.

**Do.**

- Define a constant for the deprecation metadata to keep wording
  consistent across the crate, e.g. in `domain/mod.rs`:

  ```rust
  // Re-used by every #[deprecated(...)] in the crate.
  // since = "<next-version>"
  // note  = "use `SetAttributes` / `AttributesSet` (path-keyed attributes)"
  ```

- Add a `CHANGELOG.md` entry under "Unreleased":

  ```markdown
  ### Deprecated

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
  ```

**Validation.** `cargo build --workspace` still succeeds.

**Done when.** Wording is agreed; CHANGELOG entry exists; no behavioural
changes yet.

---

### C1. Mark `Capture` and `CapturePersonDetails` deprecated

**Pre-flight.** Grep for internal usages of
`JourneyCommand::Capture` / `JourneyCommand::CapturePersonDetails` in the
workspace.

**Do.**

- Annotate the variants in `domain/commands.rs`:

  ```rust
  #[deprecated(since = "<next-version>",
               note  = "use SetAttributes (path-keyed attributes)")]
  Capture { step: String, data: Value },

  #[deprecated(since = "<next-version>",
               note  = "use SetAttributes (path-keyed attributes)")]
  CapturePersonDetails { person_ref: String, data: Value },
  ```

- The aggregate's `handle` arms for these variants stay. Wrap the match
  arms with `#[allow(deprecated)]` so the crate compiles without
  warnings:

  ```rust
  #[allow(deprecated)]
  JourneyCommand::Capture { step, data } => { /* unchanged */ }
  ```

- Keep the legacy aggregate tests (`modify_journey`, `capture_form_data_*`,
  `test_capture_person_details_*`, etc.). Add `#![allow(deprecated)]` to
  the test module.

- Update the `command_extractor` and `route_handler` to keep accepting
  the deprecated variants without warning by wrapping the relevant
  match arms or `serde`-derived constructions in
  `#[allow(deprecated)]`.

**Validation.** `cargo test --workspace`; `cargo build --workspace`
produces zero deprecation warnings from inside the crate. Any external
consumer that constructs the deprecated variants directly will now see a
warning.

**Done when.** Both legacy variants are marked deprecated; the crate
builds clean; their behaviour is unchanged.

---

### C2. Mark `Modified`, `PersonDetailsUpdated`, `StepProgressed` deprecated

**Pre-flight.** Note every site that pattern-matches on these variants:
`Journey::apply`, `View::update`, the `pii_codec` arms, the
`view_repository.rs` projector, and tests.

**Do.**

- Annotate the three variants in `domain/events.rs` with `#[deprecated]`
  using the same `since` / `note`.
- Wrap every internal pattern-match on them with `#[allow(deprecated)]`,
  arm-by-arm. Concretely:

  - `Journey::apply` keeps its `Modified` / `PersonDetailsUpdated` /
    `StepProgressed` arms (they still need to replay historical events).
  - `View::update` keeps its projections for these variants.
  - The hand-written `PiiEventCodec` keeps its `PersonDetailsUpdated`
    arm (this is what makes historical encrypted events still readable).
  - The macro-derived codec branches for these variants stay.

- Keep all fixture round-trip tests that exercise these variants. Add
  `#![allow(deprecated)]` to the relevant test modules.

- Aggregate behaviour stays exactly as today: a deprecated
  `JourneyCommand::Capture` still produces a deprecated
  `JourneyEvent::Modified`. Old downstream consumers that pattern-match
  on `Modified` continue to receive them.

**Validation.** `cargo test --workspace` and the Postgres integration
tests still pass; the crate builds with zero deprecation warnings
internally; external code that pattern-matches on the variants now sees
warnings.

**Done when.** The three variants are marked deprecated; replay,
projection, encrypt/decrypt of historical events all still work.

---

### C3. Mark `current_step` accessors / fields deprecated

**Pre-flight.** Search for `current_step`.

**Do.**

- `Journey::current_step()` accessor (`domain/journey.rs`): mark
  `#[deprecated]`. Field stays.
- `JourneyView::current_step` (`queries.rs`): mark the field
  `#[deprecated]`. Column stays.
- `Journey` struct field stays (no annotation — field deprecation on
  private fields is moot).
- Wrap internal reads/writes of `current_step` in `#[allow(deprecated)]`
  inside the crate. External readers of the field on `JourneyView` see
  the warning at compile time.
- The `current_step` column in `journey_view` stays; the migrations that
  created it are unchanged. No new migration in this step.

**Validation.** `cargo test --workspace` + Postgres integration tests;
crate builds with zero deprecation warnings internally.

**Done when.** `current_step` is accessible everywhere it is today but
any external code reading the public field/accessor sees a deprecation
warning.

---

### C4. Mark `PersonSlot.details` deprecated

**Pre-flight.** Review the permanent mirror-write established in A6.6.
Note that this step explicitly **does not** remove the mirror-write — the
mirror-write is what keeps the deprecated field's value coherent with
new commands.

**Do.**

- Mark `PersonSlot::details` `#[deprecated]` in `domain/journey.rs`.
- Mark `PersonView::details` `#[deprecated]` in `queries.rs`.
- Wrap every internal read/write of `details` (in `apply`, in the
  mirror-write inside `AttributesSet` apply, in the view projector, in
  the legacy `PersonDetailsUpdated` arm) with `#[allow(deprecated)]`.
- The mirror-write stays. The `journey_person.details` column stays. No
  new migration.
- Document in code comments that the canonical location for per-person
  attributes is `shared_data` under `persons/<ref>/…`, and that
  `slot.details` is a deprecated mirror retained for back-compat.

**Validation.** `cargo test --workspace` + integration tests; the
mirror-write test from A6.7 still passes.

**Done when.** External access to `details` produces a deprecation
warning; the field continues to be populated by both legacy commands and
(via mirror-write) new commands.

---

## Cross-cutting checklists

### After every step

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo build -p journey_dynamics --bin journey_dynamics`
- [ ] `cargo check -p flight-booking --all-targets`

> **Clippy + deprecation.** Phase C steps add `#[deprecated]` markers.
> Internal usages must be wrapped in `#[allow(deprecated)]` so
> `clippy -- -D warnings` keeps passing. Resist the urge to use a
> crate-wide `#![allow(deprecated)]` — the goal is for *external* callers
> to see the warnings, so scoping the allows narrowly preserves the
> nudge.

### At phase boundaries

- [ ] Run `postgres_view_repository.rs` and
      `postgres_subject_lookup_hook.rs` against a fresh database produced
      from current migrations.
- [ ] Boot the binary and exercise `/journeys` end-to-end with `curl`.
- [ ] Update `CHANGELOG.md` and bump `version` in `Cargo.toml`.

### Things to defer (explicit non-goals of this plan)

- **Removal of the legacy surface.** Phase C deprecates but never
  removes. A future RFC may schedule removal of the legacy commands,
  events, fields, columns, and migrations once usage telemetry confirms
  it is safe. That work would include: removing the legacy variants,
  dropping the `current_step` and `details` columns, deleting the
  mirror-write, and — at that point only — introducing an event
  up-caster to translate historical legacy events into `AttributesSet`
  at read time.
- **Snapshot encryption** (carried over from `cqrs-es-crypto`'s known
  limitations). Snapshots remain plaintext; do not store PII in aggregate
  state that gets snapshotted.
- **JDM ergonomics for deeply-nested flat paths.** B2 confirms feasibility
  for the depth used in flight-booking; richer ergonomics (e.g. `$path`
  selectors) are a separate workstream.
- **Cross-event partition deduplication.** If the same subject appears in
  many `AttributesSet` events, each event still carries its own
  partition. A future optimisation could batch-encrypt, but the current
  per-event partition model is simpler and matches the event-sourcing
  grain.

### Things now in scope that were previously deferred

- **Multi-subject events in one `SetAttributes`.** Handled natively by the
  partitioned ciphertext envelope landed in A5. No `MultiSubjectNotSupported`
  error exists.
- **Per-path redaction within an event.** A consequence of partitioning:
  when subject A is shredded but subject B is not, A's paths are redacted
  while B's are intact in the same `AttributesSet` event.

---

## Suggested PR cadence

| PR | Steps | Reviewer focus                                              |
| -- | ----- | ----------------------------------------------------------- |
| 1  | A1, A2, A3 | Pure additive types and helpers. Skim, ✅.             |
| 2  | A4    | Schema migration + serde defaults review.                   |
| 3  | A5    | **Cryptography refactor. Senior review required.** Envelope shape, AAD per partition, back-compat read path. |
| 4  | A6    | Aggregate semantics, error taxonomy, multi-subject handling. |
| 5  | A7    | Hand-written multi-partition codec for `AttributesSet`.     |
| 6  | A8    | API surface + subject-lookup migration.                     |
| 7  | B1    | Event versioning (`phase` on `WorkflowEvaluated`).         |
| 8  | B2    | Example behaviour change; product review too.               |
| 9  | B3, B4 | Optional sugar + docs.                                     |
| 10 | C0, C1, C2 | Deprecation markers on commands/events.                |
| 11 | C3, C4 | Deprecation markers on fields/accessors.                   |

---

## Downstream-consumer artefacts

For maintainers of code that depends on `journey_dynamics`, see
[`PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md`](./PATH_KEYED_ATTRIBUTES_MIGRATION_GUIDE.md).
It is the user-facing companion to this implementation plan: same
change, opposite audience.

Keep the guide in sync with the shipped types and field names as part
of step B4. If a step in this plan changes a public name (a command
variant, an event field, an accessor), update the migration guide in
the same PR.

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
- **Partition.** A subject-scoped slice of an event's secret data,
  encrypted under one DEK. An `AttributesSet` event carries zero or more
  partitions, one per subject whose data is touched in the submission.
- **Label.** A within-event identifier for a partition, used by the codec
  to route decrypted bytes back into the right field. For
  `AttributesSet`, the label is the `person_ref`; for legacy single-subject
  events, it is the string `"default"`.
