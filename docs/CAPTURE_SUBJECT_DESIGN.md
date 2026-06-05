# `CaptureSubject` — Generic Data Subject Registration

**Status:** Draft  
**Replaces:** `CapturePerson` / `CapturePersonDetails` (to be deprecated)

---

## Problem Statement

The current design has two interlocking assumptions that limit flexibility:

### 1. Subjects are always under `persons/`

`CapturePerson` registers a slot keyed by a `person_ref` string (e.g. `"lead_booker"`). The
attribute schema then references that subject as `"persons/lead_booker"` — a path formed by
prepending `"persons/"` to the ref. This prefix is hardcoded in the `SetAttributes` handler:

```rust
subject_path
    .as_str()
    .strip_prefix("persons/")         // hardcoded namespace
    .and_then(|person_ref| persons.get(person_ref))
    .map(|slot| slot.subject_id)
```

A schema cannot declare a subject at `"car-hire/main-driver"` or `"flights/pax/0"` — only under
`"persons/"`.

### 2. The same subject cannot occupy multiple roles

The `SetAttributes` handler builds a reverse map `subject_id → person_ref` to construct
`SecretPartitionData` entries:

```rust
let subject_to_ref: BTreeMap<Uuid, String> = self.persons.iter()
    .map(|(person_ref, slot)| (slot.subject_id, person_ref.clone()))
    .collect();
```

`BTreeMap` requires unique keys, so if Alice is both `"lead_booker"` and `"passenger_0"` (same
`subject_id`, different slots), only the alphabetically-last `person_ref` survives. Any
`SetAttributes` command then labels her partition with the wrong slot name, breaking:

- `slot.details` mirror-writes in `apply()` (prefix stripping uses the wrong ref)
- crypto routing on the read path (the label is used to match decrypted bytes back to their
  partition)

Both problems stem from the same root cause: **the aggregate identifies subjects by a simple
string suffix rather than by the full schema path they occupy**.

---

## Goals

- Allow subjects to be registered at **any** `AttributePath` in the schema — not just under a
  fixed root namespace.
- Allow the **same `subject_id`** to be registered at multiple paths within a single journey
  (different roles for the same person).
- When a subject is shredded (`ForgetSubject`), **all** registrations for that `subject_id` are
  forgotten, regardless of which paths they occupy.
- `classify_changes` and `SecretPartitionData` preserve per-path identity so that the crypto
  label is always correct.
- Existing persisted events (`PersonCaptured`, `PersonDetailsUpdated`) continue to apply
  correctly via the read path.

## Non-Goals

- Changing the crypto layer's `SecretPartition.label` semantics (it remains an opaque routing
  string; only its value changes from `person_ref` to the subject path string).
- Removing support for existing `PersonCaptured` / `PersonDetailsUpdated` event types from the
  event store (they must remain decodable indefinitely).
- Changing the `ForgetSubject` HTTP API shape.

---

## Design

### New command: `CaptureSubject`

```rust
CaptureSubject {
    /// The full schema path at which this subject is being registered.
    /// Must match the `subject` field in the corresponding `PiiClass::Secret`
    /// entries in the attribute schema.
    ///
    /// Examples: `"people/lead-booker"`, `"car-hire/main-driver"`,
    ///           `"flights/outbound/pax/0"`
    subject_path: AttributePath,

    /// Cross-journey identity key — used to look up the DEK.
    /// The same UUID may be registered at multiple subject_paths within
    /// one journey (multiple roles).
    subject_id: Uuid,

    /// An email address for this subject, stored in plaintext and indexed
    /// in `subject_lookup` to support shredding by email.
    ///
    /// Optional. When `None` the subject can only be shredded by `subject_id`.
    /// When `Some`, any subsequent `CaptureSubject` for the same `subject_id`
    /// replaces the stored email (upsert semantics).
    email: Option<String>,
}
```

`CaptureSubject` is intentionally minimal. It registers the mapping
`subject_path → subject_id` so that `SetAttributes` can encrypt secret attributes
correctly. It does **not** capture PII identity fields beyond email; those are
captured via `SetAttributes` using secret attribute paths (e.g.
`people/lead-booker/name`, `people/lead-booker/phone`).

#### Why keep email on `CaptureSubject`?

Email is a common lookup key for GDPR erasure requests. The `SubjectLookupHook`
currently indexes `PersonCaptured.email → subject_id` so that callers can issue
`DELETE /subjects?email=alice@example.com` without knowing the UUID. If email
were moved to `SetAttributes` it would be encrypted under the subject's DEK and
could not be indexed.

Keeping email as an optional plaintext field on `CaptureSubject` (and on the
resulting `SubjectCaptured` event) preserves this capability without coupling it
to identity data that belongs in `SetAttributes`.

Other identity fields (`name`, `phone`, passport numbers, dates of birth, etc.)
have no lookup use case and belong in `SetAttributes`.

#### Conflict rules

- Calling `CaptureSubject` for a `subject_path` that already exists with the
  **same** `subject_id` is an upsert (updates the stored email). Idempotent.
- Calling `CaptureSubject` for a `subject_path` that already exists with a
  **different** `subject_id` is rejected (`SubjectPathConflict`). A path cannot
  be reassigned to a different subject.
- Calling `CaptureSubject` for a **new** `subject_path` with a `subject_id` that
  already occupies another path is allowed — this is the multi-role case.

### New event: `SubjectCaptured`

```rust
SubjectCaptured {
    subject_path: AttributePath,
    subject_id: Uuid,
    email: Option<String>,
}
```

No PII is encrypted in this event beyond email, which is stored in plaintext (it
is not sensitive in the same way as name or passport number, and is needed for
the lookup index).

### Aggregate state

`Journey.persons` is replaced by `Journey.subjects`:

```rust
/// Per-subject registrations, keyed by the full schema path at which each
/// subject was registered (e.g. `"people/lead-booker"`).
subjects: BTreeMap<AttributePath, SubjectSlot>,
```

```rust
pub struct SubjectSlot {
    /// Cross-journey identity key — used to find the DEK.
    pub subject_id: Uuid,
    /// Set to `true` when `SubjectForgotten` is applied for this subject.
    pub forgotten: bool,
}
```

The `subject_lookup` in `SetAttributes` becomes a direct map lookup with no
prefix stripping:

```rust
classify_changes(schema, &changes, |subject_path| {
    self.subjects
        .get(subject_path)
        .filter(|s| !s.forgotten)
        .map(|s| s.subject_id)
})
```

### `classify_changes` — key by subject path, not UUID

`Classification::secret_by_subject` currently uses `Uuid` as the key, which
collapses multiple paths for the same subject into one bucket. It must be
changed to key by `AttributePath`:

```rust
pub struct Classification {
    pub plaintext: BTreeMap<AttributePath, Value>,

    /// Changes grouped by subject path (e.g. `"people/lead-booker"`).
    /// Each entry carries the resolved UUID alongside its attribute-change map.
    /// Keying by path (not UUID) preserves the distinction between two paths
    /// occupied by the same physical subject.
    pub secret_by_subject: BTreeMap<AttributePath, (Uuid, BTreeMap<AttributePath, Value>)>,

    pub unknown: Vec<AttributePath>,
}
```

`classify_changes` groups by the `subject` `AttributePath` from `PiiClass::Secret`
rather than the resolved UUID. The UUID is still threaded through as the second
element of the tuple for the encryption layer.

### `SecretPartitionData` — `person_ref` → `subject_path`

```rust
pub struct SecretPartitionData {
    /// Full schema path at which the subject was registered.
    /// Used as the crypto label (routing key on the read path).
    pub subject_path: AttributePath,

    /// Resolved UUID — used to look up the DEK.
    pub subject_id: Uuid,

    pub changes: BTreeMap<AttributePath, Value>,
}
```

Renaming from `person_ref: String` to `subject_path: AttributePath` is a
breaking change to the event schema's JSON shape. See [Migration](#migration).

### `SetAttributes` partition building

The `subject_to_ref` reverse map is removed. Partitions are built directly from
`classification.secret_by_subject`:

```rust
let mut secret_partitions: Vec<SecretPartitionData> = classification
    .secret_by_subject
    .into_iter()
    .map(|(subject_path, (subject_id, changes))| SecretPartitionData {
        subject_path,
        subject_id,
        changes,
    })
    .collect();
secret_partitions.sort_by(|a, b| a.subject_path.cmp(&b.subject_path));
```

No prefix stripping. No UUID→ref reverse map. The subject path is the partition
identity end-to-end.

### `ForgetSubject` and `SubjectForgotten`

The `ForgetSubject` command handler and `SubjectForgotten` apply logic change
from iterating `self.persons` to iterating `self.subjects`. The semantics are
identical — mark every slot whose `subject_id` matches.

### Crypto label

In `pii_codec.rs`, the `AttributesSet` branch currently uses `person_ref` as the
`SecretPartition.label`. Under the new design it uses `subject_path.to_string()`.

`reconstruct` and `redact_partitions` match on `subject_path` (from the stored
JSON field) instead of `person_ref`. This is purely a string rename in the codec.

---

## What Happens to `CapturePerson`

`CapturePerson` is **deprecated** but not immediately removed. It continues to
work by internally registering the subject at `"persons/<person_ref>"` — the same
path it always produced in the schema. Callers migrating to `CaptureSubject`
choose their own path.

`CapturePersonDetails` is deprecated. Its functionality is covered by
`SetAttributes` with secret attribute paths.

`PersonCaptured` and `PersonDetailsUpdated` event types remain in the store and
must continue to apply correctly. The aggregate `apply` match arm for
`PersonCaptured` can be translated to a `SubjectCaptured` application internally,
or kept as a separate arm forever — the event store is immutable.

---

## Migration

### Event schema — `AttributesSet`

The stored JSON for `AttributesSet` events changes the field name inside each
`secret_partitions` entry from `person_ref` to `subject_path`:

**Before:**
```json
{
  "AttributesSet": {
    "plaintext": {},
    "secret_partitions": [
      { "person_ref": "lead_booker", "subject_id": "…", "changes": {} }
    ]
  }
}
```

**After:**
```json
{
  "AttributesSet": {
    "plaintext": {},
    "secret_partitions": [
      { "subject_path": "people/lead-booker", "subject_id": "…", "changes": {} }
    ]
  }
}
```

Events written before this change carry `person_ref`. The codec (and `serde`
deserialisation) must handle both field names during a transition period.
Options:

1. **Dual-field read**: deserialise via `#[serde(alias = "person_ref")]` on the
   new `subject_path` field — requires that old events had `persons/<person_ref>`
   values that are still valid schema paths under the new model.
2. **Migration script**: rewrite stored event payloads at deploy time (risky;
   event stores are typically append-only).
3. **Codec shim**: detect the old shape in `reconstruct` / `redact_partitions`
   and map `person_ref → "persons/" + person_ref` on read.

Option 3 (codec shim) is the least invasive and keeps the event store untouched.

### Read model — `journey_person` table

`journey_person` is keyed by `(journey_id, person_ref)`. Under the new model the
natural key is `(journey_id, subject_path)`. Options:

- Add a `subject_path` column alongside `person_ref` and populate it from
  `SubjectCaptured` events; phase out `person_ref` as the primary key.
- Replace `journey_person` with `journey_subject` keyed by `(journey_id,
  subject_path)`.

Exact migration DDL is TBD.

### `subject_lookup` table

The `SubjectLookupHook` currently fires on `PersonCaptured` events. It must also
fire on `SubjectCaptured` events when `email` is `Some`. The upsert SQL is
unchanged; only the event type checked changes.

---

## Open Questions

1. **`apply` for `PersonCaptured` → where does it land?**  
   Under the new model the aggregate has `subjects: BTreeMap<AttributePath, SubjectSlot>`.
   A replayed `PersonCaptured { person_ref: "lead_booker", subject_id: X, … }` should insert
   into `subjects` at key `"persons/lead_booker"` so that historical journeys replay
   correctly. The identity fields (`name`, `email`, `phone`) no longer have a home in
   `SubjectSlot` — they would need to be written into `shared_data` via the same path, or
   simply ignored (they are in the encrypted event payload and will be correctly
   decrypted/redacted by the codec anyway).

2. **`PersonSlot` identity fields in the read model.**  
   The `journey_person` table has `name`, `email`, `phone` columns populated from
   decrypted `PersonCaptured` events. After the migration the equivalent data comes
   from `AttributesSet` events. The read-model projector must handle both.  
   For journeys that only ever used `CapturePerson`, the `name`/`email`/`phone`
   columns will continue to be populated from the legacy events. This is fine for
   existing journeys.

3. **`shared_data` shape.**  
   `SetAttributes` writes secret changes at their full path into `shared_data`
   (e.g. `shared_data["people"]["lead-booker"]["name"]`). The `slot.details`
   mirror-write is deprecated and will be removed once `PersonSlot` is gone.
   Does the read model / downstream still depend on `slot.details`?

4. **Error variant naming.**  
   `PersonRefConflict` and `PersonNotFound` will need equivalents for the new
   model: `SubjectPathConflict` and `SubjectNotFound` (or `SubjectNotRegistered`).
   What are the preferred names?

5. **Should `CaptureSubject` also be the route for updating the email?**  
   Today `CapturePerson` is idempotent — calling it again with the same
   `person_ref`/`subject_id` updates `name`, `email`, `phone`. Under `CaptureSubject`,
   re-calling with the same `subject_path`/`subject_id` would update the stored email.
   Is this the right model, or should email updates go through a different command?

---

## Example: Alice in Two Roles

```
// Alice is both the lead booker and the named driver on a car hire.
// Same subject_id, two schema paths.

CaptureSubject {
    subject_path: "people/lead-booker",
    subject_id: ALICE,
    email: Some("alice@example.com"),
}

CaptureSubject {
    subject_path: "car-hire/main-driver",
    subject_id: ALICE,
    email: None,  // email already registered above
}

// Schema declares both paths as secret subjects for their respective fields.
// PiiClass::Secret { subject: "people/lead-booker" }   → for people/lead-booker/name etc.
// PiiClass::Secret { subject: "car-hire/main-driver" } → for car-hire/main-driver/licence-number etc.

SetAttributes {
    changes: {
        "people/lead-booker/name":             "Alice Smith",
        "car-hire/main-driver/licence-number": "SMITH901152AB9IJ",
    }
}

// classify_changes produces two entries in secret_by_subject:
//   "people/lead-booker"   → (ALICE, { "people/lead-booker/name": "Alice Smith" })
//   "car-hire/main-driver" → (ALICE, { "car-hire/main-driver/licence-number": "SMITH901152AB9IJ" })
//
// Two SecretPartitionData entries, two crypto partitions, both encrypted under
// Alice's DEK.  Labels are the subject paths — distinct even though the UUID is shared.

ForgetSubject { subject_id: ALICE }

// Emits SubjectForgotten { subject_id: ALICE }.
// apply() marks BOTH subject slots (people/lead-booker and car-hire/main-driver)
// as forgotten.  Both sets of secret attributes become permanently unreadable.
```

---

## Implementation Sketch

The changes are layered; each layer can be reviewed and tested independently.

### Layer 1 — `classify_changes` (no domain changes yet)

Change `Classification::secret_by_subject` key from `Uuid` to `AttributePath`.
Update `classify_changes` and all call sites. Update `attribute_schema` tests.

This is a pure refactor with no behaviour change for existing code paths
(existing schemas have one-to-one path→UUID mappings).

### Layer 2 — `SecretPartitionData` field rename

Rename `person_ref: String` to `subject_path: AttributePath` in
`SecretPartitionData`. Update `SetAttributes` handler, `pii_codec.rs`, and all
tests. Add `#[serde(alias = "person_ref")]` for backward-compatible deserialisation.

### Layer 3 — `CaptureSubject` command and `SubjectCaptured` event

Add the new command and event. Add `subjects: BTreeMap<AttributePath, SubjectSlot>`
to the aggregate alongside `persons` (keep both during transition). Wire up
`SubjectLookupHook` to fire on `SubjectCaptured` in addition to `PersonCaptured`.

### Layer 4 — `SetAttributes` subject lookup

Switch `SetAttributes` to use `self.subjects` for the subject lookup. Retain
`self.persons` for `PersonCaptured` replay compatibility but stop writing new
entries into it.

### Layer 5 — Deprecate `CapturePerson` / `CapturePersonDetails`

Mark the commands deprecated. Internally they can delegate to `CaptureSubject`
under the `"persons/<ref>"` path. Or simply leave them working as-is forever —
the event types must remain decodable regardless.

### Layer 6 — Read model migration

Update `journey_person` / `journey_subject` DDL. Update the view projector to
handle both `PersonCaptured` and `SubjectCaptured` events.
