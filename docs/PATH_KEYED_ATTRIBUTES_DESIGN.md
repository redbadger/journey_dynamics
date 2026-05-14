# Path-Keyed Attributes — Design Proposal

**Status:** Draft / RFC
**Author:** _(see git blame)_
**Last updated:** 2026-04-30

---

## TL;DR

Replace the step-scoped `Capture { step, data }` command and its companion
`CapturePersonDetails { person_ref, data }` with a single, flat command that
carries a bag of `(path, value)` pairs. Drop `current_step` and
`StepProgressed` from the domain. Make "which step is the user on" a derived
label produced by the decision engine (`phase`), and make "which suggested
action did the user pick" a UI-only concern that never reaches the server.
Keep `CapturePerson` as a structured command because it binds crypto key
material. Tag attributes in the schema with PII classification so the crypto
layer can route per-attribute instead of per-event-variant.

---

## Background

### What we have today

```rust
pub enum JourneyCommand {
    Start { id: Uuid },
    Capture { step: String, data: Value },
    CapturePerson { person_ref, subject_id, name, email, phone },
    CapturePersonDetails { person_ref: String, data: Value },
    Complete,
    ForgetSubject { subject_id: Uuid },
}
```

The aggregate already does most of the right things:

- `Capture` accepts a free-form `Value` payload and merges it into
  `shared_data` via JSON merge-patch.
- `CapturePersonDetails` does the same into a per-person `details` blob.
- After every modifying command, the decision engine re-evaluates the entire
  state and emits `WorkflowEvaluated { suggested_actions }`.
- A rules engine (GoRules JDM) computes suggestions from data, not from a
  state machine encoded in the aggregate.

The vestiges of a step-driven model that remain:

- `Capture` carries a `step: String` field.
- `Journey` holds a `current_step: Option<String>`.
- `JourneyEvent::StepProgressed { from_step, to_step }` is emitted on
  transitions.
- The decision engine namespaces the incoming data under the step name when
  evaluating: `{ <step>: <data> }`.

### Why this hurts

Three concrete pains in the current code:

1. **Step is a wire-level concept the aggregate never branches on.** The only
   thing `current_step` is used for inside the aggregate is to detect
   transitions and pass the step name through to the decision engine. The
   engine, in turn, only uses it as a JSON namespace key. Removing it
   doesn't change any business decision.

2. **The PII / non-PII structural split forces summary fields.**
   Per-passenger details live in `persons[].details` (decrypted on read
   while the DEK exists, redacted after shredding); shared workflow data
   lives in `shared_data`. The decision engine evaluates over
   `shared_data` only, so `BookingData` carries `passengers_ready:
   Option<u32>` and `has_unaccompanied_minors: Option<bool>` — facts the
   application has to compute from the per-person blobs and re-submit
   into shared data just so the rules can see them. The information is
   technically available pre-shredding, but it lives in the wrong shape.
   Even non-PII fields like `passengerType` are stuck inside the
   per-person blob today.

3. **Variation by country/cohort gets encoded as new commands or steps.**
   This hasn't bitten us yet, but it's the failure mode that prior
   engagements with similar onboarding flows have flagged repeatedly.
   As soon as one country needs nationality during search (because it
   affects pricing), the step-namespaced shape mismatches the data.

### Prior art / lineage

Drawing on lessons from previous engagements with similar onboarding
flows, the case for partial submits over journeys-and-steps runs as
follows: the domain is fundamentally "a bag of attributes accumulated
over time, with validation rules that say what's still missing." Steps
are a UI affordance, not a domain concept. The server should publish
facts about the data, the UI should decide what to render.

The counter-argument: types-at-the-leaves still has value. JSON Schema
drives form generation, parse-don't-validate gives clean ingestion
boundaries, and the finalised record is a struct.

This proposal is the synthesis: **paths at the top, types at the leaves.**

---

## Proposal

### 1. The wire format

A single new command:

```rust
pub enum JourneyCommand {
    Start { id: Uuid },

    /// Set one or more attributes by path. The unit of submission.
    SetAttributes { changes: BTreeMap<AttributePath, Value> },

    /// Bind a subject_id to a person slot. Required before any PII
    /// attribute under `persons/<ref>/...` can be set.
    CapturePerson { person_ref, subject_id, name, email, phone },

    Complete,
    ForgetSubject { subject_id: Uuid },
}

pub struct AttributePath(String); // e.g. "search/origin", "passengers/0/passportNumber"
```

`AttributePath` is a `/`-separated path into a single, journey-wide JSON
document. Array indices are part of the path:
`passengers/0/firstName`. The path is the schema key.

### 2. The single event

```rust
JourneyEvent::AttributesSet { changes: BTreeMap<AttributePath, Value> }
```

One event per command, regardless of how many attributes it carries. The
event records the *set of facts* the client asserted in this submission;
audit and replay stay coarse-grained. The aggregate's `apply` walks
`changes` and writes each value into the appropriate location in
`shared_data`.

`Modified` and `PersonDetailsUpdated` both go away. `StepProgressed` goes
away.

### 3. Attribute schema with classification

A side-channel schema (separate from JSON Schema) classifies each path:

```rust
pub enum PiiClass {
    Plaintext,                   // stored as-is in shared_data
    Secret { subject: PathRef }, // encrypted under the subject_id at PathRef
}

pub struct AttributeSchema {
    pub paths: BTreeMap<AttributePath, PiiClass>,
    pub json_schema: serde_json::Value, // for validation / form generation
}
```

`PathRef` references another path in the document that holds the relevant
`subject_id` (typically `persons/<ref>/subject_id`, populated by
`CapturePerson`). On write, the crypto layer:

1. Splits `changes` into plaintext and secret partitions by consulting the
   schema.
2. For each secret partition, looks up the `subject_id` from the named
   path, encrypts under that subject's DEK, and emits the secret partition
   as an encrypted sub-payload of the event.
3. Plaintext changes go into the event verbatim.

On read, the inverse: decrypted secret partitions are merged back into the
event payload before the aggregate sees it. If a DEK has been shredded,
those paths are redacted to a sentinel.

### 4. Phase: a derived label

Extend the decision engine output:

```rust
pub struct WorkflowDecision {
    pub phase: Option<String>,           // <- new
    pub suggested_actions: Vec<String>,
}
```

The JDM rules emit both. `phase` is a coarse label ("collecting_search",
"collecting_passenger_pii", "ready_to_pay") computed entirely from the bag.
It is **not** part of the command surface. It lives in
`latest_workflow_decision` alongside `suggested_actions`, written by the
existing `WorkflowEvaluated` event.

### 5. Suggested actions and user choice

`suggested_actions` continues to work exactly as today: a set of strings,
re-evaluated on every change. The user picks one in the UI; that pick never
reaches the server. The next `SetAttributes` submission carries whatever
attributes the chosen form produced. The engine re-evaluates.

If telemetry on user choices is wanted, emit a separate
`UserSelectedAction` event to the analytics pipeline. It is **not** a domain
event.

### 6. `CapturePerson` stays

`CapturePerson` is structurally different from `SetAttributes`: it
allocates crypto key material (binds a `subject_id` to a `person_ref`) as a
prerequisite for any subsequent `persons/<ref>/...` attribute writes. If we
collapsed it into `SetAttributes`, we'd need a special case that detects
"setting `persons/<ref>/subject_id` triggers DEK creation" and atomically
reject any other `persons/<ref>/*` attributes that arrive before it. That
special case is more work than the structured command and obscures a real
domain invariant. Keep it.

### 7. Optional structured ingestion

For callers that prefer types at the boundary, expose `TryFrom<&Bag>` for
the finalised record (e.g. `FlightBookingSchema`). The bag is the wire
format and storage format; structs are an optional view at the egress
boundary (downstream API, reporting, etc.). Paths at the top, types at
the leaves.

---

## Rationale

### Why drop `step` from the command?

Because the user has to be allowed to ping-pong between forms. The moment
the wire carries a step, two failure modes appear:

- The user submits attributes "for step X" but the engine, given the new
  data, decides phase is now Y. Now we have a contradiction to reconcile.
- Two clients (agent + customer) submit concurrently. Whose step wins?

If the command carries no step, there is nothing to reconcile. Submissions
become commutative-ish (modulo last-writer-wins per path), which matches
the partial-submit philosophy.

### Why one event per submission, not one per attribute?

Audit and replay. A user submitting a form is a single intentional act;
splitting it into N events makes the event log harder to read and breaks
atomicity (what if events 3 and 4 are written but 5 fails?). The
`AttributesSet { changes }` event is the unit of intent. Internally,
`apply` walks the map.

### Why is `phase` a projection, not an event?

If `phase` were an event, the aggregate would need to decide its ordering
relative to `AttributesSet`, and replays would have to reconstruct phase
transitions. Both are unnecessary because phase is a pure function of the
bag. Keep events as facts about attributes; derive labels on demand.

### Why path-keyed instead of nested JSON?

The wire format is flat `(path, value)` pairs because:

- It makes the partial-submit semantics obvious: each pair is a fact.
- It makes per-attribute classification (PII, validation, etc.) trivial.
- It avoids the merge-patch ambiguity of "is `null` a deletion or an
  unset?" — explicit `Delete(path)` or `Set(path, null)` becomes possible.
- The decision engine still gets a tree: rehydrate flat → tree before
  evaluation. Cheap.

### Why keep JSON Schema?

It drives form generation and per-leaf validation. The proposal preserves
it; only the *shape of the wire envelope* changes. Per-attribute schemas
are derived from the JSON Schema by walking the path.

---

## Migration strategy

Run both interfaces in parallel for a release. New code uses
`SetAttributes`; old `Capture` / `CapturePersonDetails` are kept as
deprecated facades that translate to the new command. Cut over example
code first (flight-booking), then deprecate, then remove.

The event log is the harder migration. Two options:

1. **Up-cast on read.** Translate `Modified` and `PersonDetailsUpdated`
   events to `AttributesSet` in a read-time event up-caster. No on-disk
   rewrite. Simplest and recommended.
2. **Snapshot + replay.** Take a snapshot of every aggregate, drop the old
   event log, replay as `AttributesSet` events. More invasive; only if
   we have a reason to.

Crypto-shredding events in flight: existing `PersonDetailsUpdated`
encrypted events keep working through the up-caster. New events use the
per-attribute classification path.

---

## Crate-by-crate changes

### `cqrs-es-crypto` (transport-agnostic crypto layer)

**Status:** mostly unchanged.

The crate's contract is `PiiEventCodec`: the caller declares which event
types carry PII and how their payloads are structured. Today it's geared
toward "this whole event variant contains a `Value` field that should be
encrypted under this `subject_id` field." The new model needs:

- **A path-aware codec mode.** Add a new `PiiEventCodec` variant (or a
  parallel trait) that classifies *paths within a payload* rather than
  *fields of a struct*. Concretely: given `AttributesSet { changes }`,
  partition `changes` by path classification, encrypt the secret partition
  under the right subject's DEK, store as a sub-payload alongside the
  plaintext partition.

  Sketch:

  ```rust
  pub trait PiiEventCodecByPath {
      fn classify(&self, event: &SerializedEvent) -> Classification;
      fn encrypt_partitioned(...) -> Result<SerializedEvent, _>;
      fn decrypt_partitioned(...) -> Result<SerializedEvent, _>;
      fn redact_partitioned(...) -> Result<SerializedEvent, _>;
  }

  pub struct Classification {
      pub plaintext_paths: Vec<AttributePath>,
      pub secret_partitions: Vec<SecretPartition>, // grouped by subject_id
  }
  ```

- **Multi-subject encryption in a single event.** Today an encrypted event
  has one `subject_id`. With path-keyed attributes, one submission could
  touch attributes belonging to two passengers (rare, but legal). The
  encrypted sub-payload becomes a `Vec<(subject_id, ciphertext)>` rather
  than a single `(subject_id, ciphertext)`. Each entry encrypts the
  partition for one subject.

- **Sentinel/redaction stays.** The shape of redacted output for path-keyed
  events is "delete the redacted paths from `changes`" or "replace with
  a marker `{ "redacted": true }`". Keep the existing sentinel mechanism
  available for legacy events.

The cipher (`cipher.rs`), key store (`key_store.rs`), and repository
plumbing (`repository.rs`) need no algorithmic changes. The work is at the
codec boundary.

**Estimated change:** ~300–500 LoC, mostly new. No breaking changes to
existing consumers if the new codec is added alongside the old one.

### `cqrs-es-crypto-derive` (derive macro)

**Status:** new attribute, new code-gen branch.

Today the macro generates `PiiEventCodec` from `#[pii(...)]` attributes on
enum variants:

```rust
#[pii(event_type = "PersonCaptured")]
PersonCaptured {
    #[pii(plaintext)] person_ref: String,
    #[pii(subject)]   subject_id: Uuid,
    #[pii(secret)]    name: String,
    ...
}
```

For path-keyed events we need a different shape. Two options:

1. **New attribute, same macro.** Add `#[pii(by_path)]` on a variant with a
   single `changes: BTreeMap<AttributePath, Value>` field. The macro emits
   code that defers classification to a runtime `AttributeSchema` provided
   by the caller. The macro doesn't know the paths — only that they exist.

2. **Don't use the derive macro for this variant.** Hand-write the codec
   for `AttributesSet`, since its classification is data-driven (schema)
   rather than type-driven (struct fields).

Option 2 is simpler and probably right for now. The derive macro's
sweet spot is "this variant has these fixed PII fields"; a schema-driven
event is a different beast. Document this in the macro README.

If we go with option 1 later, the work is:

- Parser: recognise `#[pii(by_path)]` on a variant.
- Code-gen: emit `classify`/`extract`/`reconstruct`/`redact` arms that call
  through to a runtime schema provided as `Self::Services` or similar.

**Estimated change:** zero if we hand-write the codec for `AttributesSet`.
~200 LoC if we extend the macro.

### `journey_dynamics` (the application)

This is where the bulk of the work is.

#### `domain/commands.rs`

- Replace `Capture` and `CapturePersonDetails` with `SetAttributes`.
- Define `AttributePath`.
- Keep `CapturePerson`, `Start`, `Complete`, `ForgetSubject` as-is.

#### `domain/events.rs`

- Replace `Modified` and `PersonDetailsUpdated` with `AttributesSet`.
- Remove `StepProgressed`.
- Update the `PiiCodec` derive (or hand-write a codec) for `AttributesSet`
  to do path-based partitioning.

#### `domain/journey.rs`

- Drop `current_step` from `Journey` and `PersonSlot.details` (the latter
  becomes individual paths under `persons/<ref>/...` in `shared_data`).
- Rewrite `handle` for `SetAttributes`:
  1. Validate journey state (`InProgress`, exists).
  2. For each `(path, value)` in `changes`, validate the leaf against the
     per-path JSON Schema.
  3. For any path under `persons/<ref>/...`, ensure the slot exists
     (i.e., `CapturePerson` has been called).
  4. Re-evaluate the decision engine over the merged-in-flight bag.
  5. Emit `AttributesSet { changes }` and `WorkflowEvaluated { phase,
     suggested_actions }`.
- Rewrite `apply` for `AttributesSet`:
  - For each `(path, value)`, walk into `shared_data` and merge.
  - Use `json_patch::merge` semantics at the leaf, or implement an
    explicit `set_at_path`.
- Remove the `is_step_transition` logic entirely.

#### `domain/journey.rs` — `PersonSlot`

The slot becomes thinner. Identity fields (`name`, `email`, `phone`) can
either stay as struct fields (set by `CapturePerson`, never by
`SetAttributes`) or be flattened into `persons/<ref>/name`,
`persons/<ref>/email`, etc. Recommend keeping them as struct fields for
now — they're set atomically by `CapturePerson` and never partially.

`PersonSlot.details: Value` goes away. Free-form per-passenger details are
just attributes under `persons/<ref>/...` and live in `shared_data`.

#### `services/decision_engine.rs`

- Stop namespacing data under the step:
  ```rust
  // BEFORE
  let keyed_data = serde_json::json!({ current_step: new_data });
  json_patch::merge(&mut accumulated_data, &keyed_data);

  // AFTER
  // The bag is already merged in the aggregate; pass shared_data verbatim.
  ```
- `WorkflowDecision` gains `phase: Option<String>`.
- For the GoRules engine: update the JDM models to read from the flat bag
  (or from a rehydrated tree of the bag) and to emit a `phase` field
  alongside `suggestedActions`.

#### `queries.rs`

- Drop `current_step` from `JourneyView`.
- Add `phase: Option<String>` to `WorkflowDecisionView`.
- Drop the `StepProgressed` arm in `View::update`.
- The `persons` field of `JourneyView` either stays (sourced from the
  `journey_person` table) or is replaced by querying `shared_data` for
  `persons/*`. Keeping the table is simpler and preserves the
  shred-by-row erasure path.

#### `command_extractor.rs` and `route_handler.rs`

- The HTTP API surface changes: a new `SetAttributes` command takes a JSON
  body of `{ "changes": { "search/origin": "LHR", ... } }`, or the more
  ergonomic nested form `{ "search": { "origin": "LHR" } }` flattened
  server-side at parse time.
- Recommend the nested form on the wire and flatten in the extractor —
  it's friendlier for clients and trivially convertible.
- Deprecate `/journeys/{id}` POST bodies that use the old `Capture` /
  `CapturePersonDetails` envelopes; keep them as up-cast translations
  for one release.

#### Migrations

- Schema migration: drop `current_step` column from the journey view
  table (if it's persisted).
- Event up-caster: translate historical `Modified` and
  `PersonDetailsUpdated` events to `AttributesSet` at read time. Implement
  in a thin layer between the event store and the aggregate.

#### Tests

- Most aggregate tests in `domain/journey.rs` need rewriting.
  `start_a_journey`, `modify_journey`, etc. become `set_attributes_*`
  tests. The good news: the test count probably halves because the
  `step`/`is_step_transition` cases collapse.
- Decision-engine tests need the new `phase` output asserted.
- View tests need `current_step` assertions removed and `phase`
  assertions added.

#### Examples

`flight-booking/src/lib.rs`:

- Drop `passengers_ready` and `has_unaccompanied_minors` summary fields —
  these are now visible to the decision engine directly at
  `persons/*/passengerType` etc., without the application having to
  re-shape per-person data into shared data.
- The `FlightBookingSchema` struct stays as the *parsed-final* shape used
  for downstream submission. Add a `TryFrom<&JourneyView>` (or
  `TryFrom<&Bag>`) impl that pulls the right paths.
- The JDM models need updating to read from flat paths and emit `phase`.

---

## Risks and open questions

- **JDM rules on flat paths.** The path-keyed bag has to be rehydrated to
  a tree before JDM evaluates against it (JDM expects nested JSON). Need
  to confirm this is cheap and that path notation in JDM expressions is
  ergonomic. Spike before committing.
- **Schema drift.** With paths on the wire, typos like `searh/origin` are
  silent unless we validate against the path set. Validation must reject
  unknown paths in `SetAttributes`.
- **Multi-subject events.** A submission that touches two passengers'
  PII produces an event with two encrypted partitions. The repository
  layer must handle multi-subject encryption (today it's single-subject
  per event). Confirm this fits the existing `subject_encryption_keys`
  table model.
- **Atomicity of `CapturePerson` + first attributes.** A common UI flow is
  "capture passenger identity and details in one form." That's two
  commands today. Consider whether `CapturePerson` should optionally
  carry an initial `attributes: BTreeMap<AttributePath, Value>` for a
  combined operation, or whether the UI should just send two requests
  and rely on the decision engine to handle the intermediate state
  gracefully. Recommend the latter for simplicity.
- **Backwards compatibility window.** How long do we keep `Capture` /
  `CapturePersonDetails` as deprecated facades? Recommend one release
  cycle.

---

## Appendix: example flow

### Today

```
POST /journeys
POST /journeys/{id}  { "Capture": { "step": "search", "data": { "search": { ... } } } }
POST /journeys/{id}  { "CapturePerson": { "person_ref": "passenger_0", ... } }
POST /journeys/{id}  { "CapturePersonDetails": { "person_ref": "passenger_0", "data": { "passportNumber": "..." } } }
POST /journeys/{id}  { "Complete" }
```

### Proposed

```
POST /journeys
POST /journeys/{id}  { "SetAttributes": { "changes": {
    "search/origin": "LHR",
    "search/destination": "JFK",
    "search/departureDate": "2025-08-15"
} } }
POST /journeys/{id}  { "CapturePerson": { "person_ref": "passenger_0", ... } }
POST /journeys/{id}  { "SetAttributes": { "changes": {
    "persons/passenger_0/passportNumber": "GB123456789",
    "persons/passenger_0/dateOfBirth": "1990-05-15",
    "persons/passenger_0/passengerType": "adult"
} } }
POST /journeys/{id}  { "Complete" }
```

`passportNumber` and `dateOfBirth` are classified as `Secret { subject:
persons/passenger_0/subject_id }` and encrypted at rest; `passengerType`
is `Plaintext`. The decision engine sees all three at their natural
paths (subject to redaction after shredding), without the application
having to re-shape per-person data into shared data.

---

## Decision

_(to be filled in once reviewed)_
