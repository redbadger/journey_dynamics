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

A schema cannot declare a subject at `"car-hire/main-driver"` or `"flights/pax/alice-ref"` —
only under `"persons/"`.

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

### 3. `NamespacePattern` only handles exactly three segments

`NamespacePattern` matches paths of the form `<namespace>/<ref>/<field>` — exactly three
segments. A path like `flights/outbound/pax/alice-ref/passportNumber` (five segments) falls
through to the permissive fallback, silently treating secret PII as plaintext.

### Root cause

All three problems stem from the same root cause: **subjects, their role assignments, and their
schema classification are unnecessarily coupled**. The aggregate identifies subjects by a simple
string suffix rather than by the full schema path they occupy; the schema assumes a fixed
depth; and there is no separation between "this person exists" and "this person occupies this
role".

---

## Goals

- **Decouple subject identity from role assignment.** A subject is registered once per journey
  (with a cross-journey `subject_id`); role paths are bound separately.
- Allow subjects to be bound at **any** `AttributePath` in the schema — not just under a fixed
  root namespace.
- Allow the **same `subject_id`** to be bound at multiple role paths within a single journey
  (different roles for the same person).
- When a subject is shredded (`ForgetSubject`), **all** bindings for that `subject_id` become
  inert in a single operation, regardless of which paths they occupy.
- `classify_changes` and `SecretPartitionData` preserve per-path identity so that the crypto
  label is always correct.
- Generalise `NamespacePattern` to support arbitrary-depth prefixes with secret-by-default
  classification.
- **Enforce stable role refs** — reject bare-integer path segments (positional indices) in role
  bindings to prevent crypto and data-shape fragility when lists are reordered.
- Existing persisted events (`PersonCaptured`, `PersonDetailsUpdated`) continue to apply
  correctly via the read path.

## Non-Goals

- Changing the crypto layer's `SecretPartition.label` semantics (it remains an opaque routing
  string; only its value changes from `person_ref` to the role path string).
- Removing support for existing `PersonCaptured` / `PersonDetailsUpdated` event types from the
  event store (they must remain decodable indefinitely).
- Changing the `ForgetSubject` HTTP API shape.

---

## Design

The design separates three concerns that are currently conflated:

1. **Subject identity** — "this data subject is part of this journey" (`CaptureSubject`)
2. **Role binding** — "this subject occupies this schema path" (`BindSubject`)
3. **Schema classification** — "attributes under this prefix belong to a subject-scoped
   namespace" (`NamespacePattern`)

### New commands

#### `CaptureSubject`

```rust
CaptureSubject {
    /// Cross-journey identity key — used to look up the DEK.
    subject_id: Uuid,

    /// An email address for this subject, stored in plaintext and indexed
    /// in `subject_lookup` to support shredding by email.
    ///
    /// Optional. When `None` on a new subject, the subject can only be
    /// shredded by `subject_id`. When `None` on an existing subject
    /// (upsert), the stored email is left unchanged. When `Some`, the
    /// stored email is replaced.
    email: Option<String>,
}
```

`CaptureSubject` is intentionally minimal. It registers the subject's existence in the
journey — nothing more. No path, no role, no PII beyond the optional email.

**Idempotent.** Calling again with the same `subject_id` upserts the email (where `None`
means "leave unchanged", not "clear"). `Some("")` clears the email explicitly.

##### Why keep email on `CaptureSubject`?

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

#### `BindSubject`

```rust
BindSubject {
    /// The full schema path at which this subject is being bound.
    /// Must correspond to the subject path derived by the attribute schema's
    /// namespace pattern (or an explicit PiiClass::Secret entry).
    ///
    /// Examples: `"lead-booker"`, `"car-hire/main-driver"`,
    ///           `"pax/alice-ref"`
    role_path: AttributePath,

    /// Cross-journey identity key — must already be registered via
    /// `CaptureSubject`.
    subject_id: Uuid,
}
```

`BindSubject` creates the mapping `role_path → subject_id` that `SetAttributes` uses to
route secret attributes to the correct DEK.

**Conflict rules:**

- `subject_id` must already be registered via `CaptureSubject` → error
  `SubjectNotRegistered` if not.
- A `role_path` already bound to the **same** `subject_id` → idempotent no-op.
- A `role_path` already bound to a **different** `subject_id` → error
  `RolePathConflict`. Rebinding is not allowed (see [Why disallow
  rebinding?](#why-disallow-rebinding)).
- A **new** `role_path` with a `subject_id` that already occupies another path → allowed
  (multi-role case).

**Stable-key enforcement.** The role ref — the final segment of `role_path` — must not be
a bare integer (a string consisting entirely of ASCII digits). Positional indices like
`pax/0` break when lists are reordered and cause `set_at_path` to create JSON arrays
instead of objects, which is incompatible with how the JDM rules engine consumes
passenger data (see [JDM compatibility](#jdm-compatibility)). A bare-integer ref is
rejected with a `PositionalRoleRef` error that suggests using a stable identifier.

```rust
fn validate_role_ref(role_path: &AttributePath) -> Result<(), JourneyError> {
    if let Some(ref_segment) = role_path.segments().last() {
        if !ref_segment.is_empty() && ref_segment.chars().all(|c| c.is_ascii_digit()) {
            return Err(JourneyError::PositionalRoleRef {
                role_path: role_path.clone(),
                ref_segment: ref_segment.to_string(),
            });
        }
    }
    Ok(())
}
```

#### `CaptureAndBindSubject`

```rust
CaptureAndBindSubject {
    subject_id: Uuid,
    email: Option<String>,
    role_path: AttributePath,
}
```

Convenience command that registers the subject and binds it to a role in one step. The
handler validates both operations and emits two events in sequence: `SubjectCaptured`
then `SubjectBound`. This keeps the event model clean (two separate facts) while
reducing round-trips for the common case.

All validation rules from `CaptureSubject` and `BindSubject` apply.

### New events

```rust
SubjectCaptured {
    subject_id: Uuid,
    email: Option<String>,
}

SubjectBound {
    role_path: AttributePath,
    subject_id: Uuid,
}
```

No PII is encrypted in `SubjectCaptured` beyond email, which is stored in plaintext (it
is not sensitive in the same way as name or passport number, and is needed for the lookup
index).

### Aggregate state

`Journey.persons` is replaced by two maps:

```rust
struct Journey {
    // ...

    /// Registered data subjects, keyed by cross-journey identity.
    subjects: BTreeMap<Uuid, SubjectRegistration>,

    /// Role bindings: role_path → subject_id.
    /// Every subject_id here must exist in `subjects`.
    bindings: BTreeMap<AttributePath, Uuid>,
}

struct SubjectRegistration {
    email: Option<String>,
    forgotten: bool,
}
```

The `SetAttributes` subject lookup becomes two cheap map lookups with no prefix stripping
and no reverse maps:

```rust
classify_changes(schema, &changes, |role_path| {
    self.bindings
        .get(role_path)
        .copied()
        .filter(|sid| {
            self.subjects
                .get(sid)
                .is_some_and(|reg| !reg.forgotten)
        })
})
```

### `classify_changes` — key by role path, not UUID

`Classification::secret_by_subject` currently uses `Uuid` as the key, which
collapses multiple paths for the same subject into one bucket. It must be
changed to key by `AttributePath`:

```rust
pub struct Classification {
    pub plaintext: BTreeMap<AttributePath, Value>,

    /// Changes grouped by role path (e.g. `"pax/alice-ref"`).
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

### `SecretPartitionData` — `person_ref` → `role_path`

```rust
pub struct SecretPartitionData {
    /// Full schema path at which the subject was bound.
    /// Used as the crypto label (routing key on the read path).
    pub role_path: AttributePath,

    /// Resolved UUID — used to look up the DEK.
    pub subject_id: Uuid,

    pub changes: BTreeMap<AttributePath, Value>,
}
```

Renaming from `person_ref: String` to `role_path: AttributePath` is a
breaking change to the event schema's JSON shape. See [Migration](#migration).

### `SetAttributes` partition building

The `subject_to_ref` reverse map is removed. Partitions are built directly from
`classification.secret_by_subject`:

```rust
let mut secret_partitions: Vec<SecretPartitionData> = classification
    .secret_by_subject
    .into_iter()
    .map(|(role_path, (subject_id, changes))| SecretPartitionData {
        role_path,
        subject_id,
        changes,
    })
    .collect();
secret_partitions.sort_by(|a, b| a.role_path.cmp(&b.role_path));
```

No prefix stripping. No UUID→ref reverse map. The role path is the partition
identity end-to-end.

**Note:** Two `SecretPartitionData` entries in the same `AttributesSet` event may share
the same `subject_id` (same DEK) when a subject occupies multiple roles. The crypto
layer must handle encrypting multiple partitions under the same key with **different
labels**. This must be verified with a test case.

### `SetAttributes` command ordering

`SetAttributes` can only encrypt secret attributes if the relevant subject has been
captured **and** bound to a role path beforehand. If an attribute path resolves to a
role path that has no binding, it lands in `Classification::unknown` and the command
is rejected.

For list-style collections (passengers, drivers, etc.), this means the caller must:

1. `CaptureSubject` (or `CaptureAndBindSubject`) for each subject
2. `BindSubject` for each role path (if not already done via `CaptureAndBindSubject`)
3. `SetAttributes` with the attribute paths

If a `SetAttributes` arrives before the binding exists, the error should clearly
indicate which role path was unresolvable so the caller can issue the missing
`BindSubject`.

### `ForgetSubject` and `SubjectForgotten`

`ForgetSubject` becomes trivial — it marks the subject registration as forgotten in a
single write. No iteration over bindings is needed; the lookup naturally filters out
forgotten subjects.

```rust
// Command handler
ForgetSubject { subject_id } => {
    if !self.subjects.contains_key(&subject_id) {
        return Err(JourneyError::SubjectNotRegistered);
    }
    // ... emit SubjectForgotten
}

// Apply
SubjectForgotten { subject_id } => {
    if let Some(reg) = self.subjects.get_mut(&subject_id) {
        reg.forgotten = true;
    }
}
```

All bindings that point at the forgotten `subject_id` become inert automatically because
the lookup filters `!reg.forgotten`. Every role Alice occupied becomes unreadable in a
single operation.

### Crypto label

In `pii_codec.rs`, the `AttributesSet` branch currently uses `person_ref` as the
`SecretPartition.label`. Under the new design it uses `role_path.to_string()`.

`reconstruct` and `redact_partitions` match on `role_path` (from the stored
JSON field) instead of `person_ref`. This is purely a string rename in the codec.

---

## Generalised `NamespacePattern`

The current `NamespacePattern` only handles exactly three-segment paths
(`<namespace>/<ref>/<field>`). The generalised version supports arbitrary-depth prefixes
and classifies the entire subtree as secret by default.

### Structure

```rust
pub struct NamespacePattern {
    /// Path prefix — one or more segments. The segment immediately after
    /// this prefix in any matching path is the role ref.
    ///
    /// e.g. `"pax"` matches `pax/{ref}/…`
    ///      `"flights/outbound/pax"` matches `flights/outbound/pax/{ref}/…`
    pub prefix: AttributePath,

    /// Suffixes (relative to `prefix/{ref}`) that are exempt from encryption.
    /// Everything else under the namespace is Secret by default.
    ///
    /// e.g. `"passengerType"` exempts `prefix/{ref}/passengerType`
    ///      `"meta/source"`   exempts `prefix/{ref}/meta/source`
    pub plaintext_suffixes: BTreeSet<String>,
}
```

**Secret-by-default** is the safe posture — you opt attributes *out* of encryption
rather than in. A new field added under a subject namespace is automatically encrypted;
you cannot accidentally ship plaintext PII by forgetting to classify it.

### Updated `classify`

```rust
// 2. Namespace patterns — match prefix/{ref}/{suffix…} at any depth.
for pattern in &self.namespace_patterns {
    let prefix_segs: Vec<&str> = pattern.prefix.segments().collect();
    let n = prefix_segs.len();

    // Need at least prefix + ref + one attribute segment.
    if segs.len() < n + 2 {
        continue;
    }
    if segs[..n] != prefix_segs[..] {
        continue;
    }

    // Suffix is everything after prefix/ref.
    let suffix = segs[n + 1..].join("/");

    if pattern.plaintext_suffixes.contains(&suffix) {
        return Some(Cow::Borrowed(&PLAINTEXT));
    }

    // Default: Secret. Role path = prefix/ref.
    let role_str = segs[..=n].join("/");
    if let Ok(role_path) = role_str.parse::<AttributePath>() {
        return Some(Cow::Owned(PiiClass::Secret { subject: role_path }));
    }
}
```

### Classification precedence

Most-specific wins, with Secret overriding Plaintext at equal specificity:

1. **Exact path entry** — most specific, always wins.
2. **Namespace pattern match** — secret-by-default; `plaintext_suffixes` can exempt
   specific fields. Cuts through plaintext prefixes.
3. **Plaintext prefix** — broad subtree classification.
4. **Permissive fallback** — if the schema is permissive.
5. **`None`** — path is unknown.

The critical rule: **namespace patterns are evaluated before plaintext prefixes**. If
`"booking"` is a plaintext prefix but there is a namespace pattern with prefix
`"booking/passengers"`, then `booking/passengers/alice-ref/passportNumber` is Secret
(step 2 matches) even though `booking/origin` is Plaintext (step 3 matches).

An explicit path entry (step 1) always overrides a namespace pattern's default — useful
for one-off exceptions.

### Example matches

With a namespace pattern `{ prefix: "pax", plaintext_suffixes: {"passengerType"} }`:

| Path | Match? | Classification |
|------|--------|----------------|
| `pax/alice-ref/passportNumber` | ✓ ref=`alice-ref`, suffix=`passportNumber` | Secret → `pax/alice-ref` |
| `pax/alice-ref/address/line1` | ✓ ref=`alice-ref`, suffix=`address/line1` | Secret → `pax/alice-ref` |
| `pax/alice-ref/passengerType` | ✓ suffix in `plaintext_suffixes` | Plaintext |
| `pax/alice-ref` | ✗ only prefix + ref, no attribute | Falls through |
| `pax` | ✗ too short | Falls through |

### Schema configuration

The JSON configuration format changes from:

```json
{
  "namespace_patterns": [
    {
      "namespace": "persons",
      "secret_fields": ["firstName", "passportNumber"],
      "plaintext_fields": ["passengerType"]
    }
  ]
}
```

To:

```json
{
  "namespace_patterns": [
    {
      "prefix": "persons",
      "plaintext_suffixes": ["passengerType"]
    }
  ]
}
```

The old `secret_fields` / `plaintext_fields` split is replaced by a single
`plaintext_suffixes` set. Everything not listed is Secret by default.

---

## Why disallow rebinding?

Allowing a `role_path` to be rebound from one `subject_id` to another is technically
possible — the stored events carry `subject_id` so historical decryption still works —
but it creates subtle problems:

- The **read model** must track the binding at each point in time. If `pax/a` was Alice
  until event 5 and Bob from event 6, the projection must merge both timelines under the
  same path.
- The **audit trail** becomes ambiguous — "pax/a's passport changed" doesn't tell you
  which person changed.
- **`ForgetSubject`** gets messy — if Alice is forgotten and her old data at `pax/a` is
  shredded, but Bob's new data at `pax/a` isn't, the path contains a mix of shredded and
  readable data.

Disallowing rebinding and using stable keys avoids all of this. If a caller genuinely
needs to replace a person in a role, they use a new role path and leave the old one
inert.

---

## JDM compatibility

The flight-booking orchestrator JDM receives `shared_data` as its input context. The
`persons` key is a **keyed object** (not a JSON array), and the orchestrator already
handles this:

```zen
persons_list = persons != null ? values(persons) : []
passenger_counts.found = len(persons_list)
computed.passengersComplete = ... and all(persons_list, #.passengerType != null)
```

`values()` extracts the map values into an array for counting and iteration. The keys
(`alice-ref`, `bob-ref`, …) are opaque to the rules — only the values matter.

The stable-key enforcement reinforces this: `set_at_path` in `json_path.rs` treats
bare-integer segments as **array indices** and non-integer segments as **object keys**.
By rejecting bare integers in role refs, we guarantee that subject namespaces always
produce JSON objects in `shared_data`, which is the shape the JDM expects.

---

## What Happens to `CapturePerson`

`CapturePerson` is **deprecated** but not immediately removed. Internally it emits
both `SubjectCaptured` and `SubjectBound { role_path: "persons/<person_ref>" }`.

`CapturePersonDetails` is deprecated. Its functionality is covered by
`SetAttributes` with secret attribute paths.

`PersonCaptured` and `PersonDetailsUpdated` event types remain in the store and
must continue to apply correctly. The aggregate `apply` match arm for
`PersonCaptured` populates both `subjects` and `bindings`:

```rust
PersonCaptured { person_ref, subject_id, email, .. } => {
    let role_path: AttributePath = format!("persons/{person_ref}").parse().unwrap();
    self.subjects.entry(subject_id).or_insert(SubjectRegistration {
        email: None,
        forgotten: false,
    });
    // Update email if provided.
    if let Some(ref e) = email {
        self.subjects.get_mut(&subject_id).unwrap().email = Some(e.clone());
    }
    self.bindings.insert(role_path, subject_id);
}
```

---

## Migration

### Event schema — `AttributesSet`

The stored JSON for `AttributesSet` events changes the field name inside each
`secret_partitions` entry from `person_ref` to `role_path`:

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
      { "role_path": "lead-booker", "subject_id": "…", "changes": {} }
    ]
  }
}
```

Events written before this change carry `person_ref`. The codec (and `serde`
deserialisation) must handle both field names during a transition period.

**Codec shim** (least invasive, keeps the event store untouched): detect the old shape
in `reconstruct` / `redact_partitions` and map `person_ref → "persons/" + person_ref`
on read. This only works if old `person_ref` values, when prefixed with `"persons/"`,
are valid `AttributePath` values and correspond to registered bindings in the new model.
This invariant must be asserted by a migration test.

Alternatively, `#[serde(alias = "person_ref")]` on the `role_path` field handles the
deserialisation transparently, but requires that old values are already valid role paths
(they are not — they lack the `"persons/"` prefix). The codec shim is therefore the
correct approach.

### Read model — `journey_person` table

`journey_person` is keyed by `(journey_id, person_ref)`. Under the new model the
natural key is `(journey_id, role_path)`. Options:

- Add a `role_path` column alongside `person_ref` and populate it from
  `SubjectBound` events; phase out `person_ref` as the primary key.
- Replace `journey_person` with `journey_subject` keyed by `(journey_id, subject_id)`
  and a separate `role_binding` table keyed by `(journey_id, role_path)`.

Exact migration DDL is TBD.

### `subject_lookup` table

The `SubjectLookupHook` currently fires on `PersonCaptured` events. It must also
fire on `SubjectCaptured` events when `email` is `Some`. The upsert SQL is
unchanged; only the event type checked changes.

### `NamespacePattern` schema configuration

Existing `attribute-schema.json` files must be migrated from the old format to the new:

| Old field | New field | Notes |
|-----------|-----------|-------|
| `namespace` | `prefix` | Semantically identical for single-segment namespaces |
| `secret_fields` | *(removed)* | Everything is secret by default |
| `plaintext_fields` | `plaintext_suffixes` | Same set of field names |

This is a non-breaking change if the schema loader accepts both formats during a
transition period.

---

## Example: Flight booking with dynamic passengers

```
// ── Register subjects ────────────────────────────────
CaptureSubject { subject_id: ALICE, email: Some("alice@example.com") }
CaptureSubject { subject_id: BOB,   email: Some("bob@example.com")   }

// ── Bind to roles ────────────────────────────────────
BindSubject { role_path: "lead-booker",   subject_id: ALICE }
BindSubject { role_path: "pax/alice-ref", subject_id: ALICE }
BindSubject { role_path: "pax/bob-ref",   subject_id: BOB   }

// Or equivalently, using the convenience command:
// CaptureAndBindSubject { subject_id: ALICE, email: Some("alice@example.com"), role_path: "lead-booker" }
// BindSubject { role_path: "pax/alice-ref", subject_id: ALICE }   // second role
// CaptureAndBindSubject { subject_id: BOB, email: Some("bob@example.com"), role_path: "pax/bob-ref" }

// ── Schema ───────────────────────────────────────────
// namespace pattern: prefix="pax", plaintext_suffixes={"passengerType"}
// explicit entry:    "lead-booker/name" → Secret { subject: "lead-booker" }
// explicit entry:    "lead-booker/phone" → Secret { subject: "lead-booker" }
// plaintext prefix:  "search"

// ── Set attributes ───────────────────────────────────
SetAttributes {
    changes: {
        "search/origin":                "LHR",           // plaintext (prefix)
        "lead-booker/name":             "Alice Smith",   // secret → ALICE
        "pax/alice-ref/passportNumber": "AB123",         // secret → ALICE
        "pax/alice-ref/passengerType":  "adult",         // plaintext (suffix exemption)
        "pax/bob-ref/passportNumber":   "CD456",         // secret → BOB
        "pax/bob-ref/passengerType":    "adult",         // plaintext (suffix exemption)
    }
}

// classify_changes produces:
//   plaintext: {
//     "search/origin": "LHR",
//     "pax/alice-ref/passengerType": "adult",
//     "pax/bob-ref/passengerType": "adult",
//   }
//   secret_by_subject: {
//     "lead-booker"   → (ALICE, { "lead-booker/name": "Alice Smith" })
//     "pax/alice-ref" → (ALICE, { "pax/alice-ref/passportNumber": "AB123" })
//     "pax/bob-ref"   → (BOB,   { "pax/bob-ref/passportNumber": "CD456" })
//   }
//
// Three secret partitions. Two share Alice's DEK (distinct labels). One uses
// Bob's DEK.

// ── Later: add a third passenger (no schema change needed) ───
CaptureAndBindSubject {
    subject_id: CAROL,
    email: Some("carol@example.com"),
    role_path: "pax/carol-ref",
}
SetAttributes {
    changes: {
        "pax/carol-ref/passportNumber": "EF789",
        "pax/carol-ref/passengerType":  "child",
    }
}

// ── GDPR erasure ─────────────────────────────────────
ForgetSubject { subject_id: ALICE }
// Marks ALICE.forgotten = true.
// "lead-booker" and "pax/alice-ref" bindings both become inert.
// Both sets of secret attributes become permanently unreadable.
// Bob's and Carol's data are untouched.
```

### What `shared_data` looks like after these commands

```json
{
  "search": { "origin": "LHR" },
  "lead-booker": { "name": "Alice Smith" },
  "pax": {
    "alice-ref": { "passportNumber": "AB123", "passengerType": "adult" },
    "bob-ref":   { "passportNumber": "CD456", "passengerType": "adult" },
    "carol-ref": { "passportNumber": "EF789", "passengerType": "child" }
  }
}
```

`pax` is a JSON **object** keyed by the stable role ref — not an array. The JDM
orchestrator consumes it via `values(pax)` to get an array for `len()` and `all()`.

---

## Open Questions

1. **Should `CaptureSubject` accept `Some("")` to clear an email?**
   The current proposal treats `None` as "leave unchanged" and `Some(value)` as "set".
   An explicit "clear" case might warrant a dedicated `ClearSubjectEmail` command or a
   tri-state (`Set(String)` / `Clear` / `Unchanged`), but that may be over-engineering.
   For now, `Some("")` clears.

2. **Error variant naming.**
   `PersonRefConflict` and `PersonNotFound` need equivalents:
   - `SubjectNotRegistered` — `subject_id` not in `subjects` map.
   - `RolePathConflict` — `role_path` already bound to a different `subject_id`.
   - `PositionalRoleRef` — bare-integer segment in role path.

3. **`shared_data` shape — `slot.details` mirror-write.**
   `SetAttributes` currently mirror-writes secret changes into `slot.details` for
   backward compatibility. Once `PersonSlot` is removed, the mirror-write is removed
   too. Does any downstream consumer still depend on `slot.details`?

4. **Multiple partitions sharing a DEK.**
   The crypto layer must support encrypting two separate partitions under the same DEK
   but with different labels (the multi-role case). This needs a test to confirm.

5. **`NamespacePattern` prefix conflicts.**
   If two namespace patterns have overlapping prefixes (e.g. `"pax"` and
   `"pax/outbound"`), the first match wins. Should the schema loader reject overlapping
   patterns, or is first-match-wins sufficient?

---

## Implementation Sketch

The changes are layered; each layer can be reviewed and tested independently.

### Layer 1 — Generalise `NamespacePattern`

Change `NamespacePattern` from a single `namespace` string to an `AttributePath` prefix.
Replace `secret_fields` / `plaintext_fields` with `plaintext_suffixes`. Update
`classify` to match at arbitrary depth. Update `AttributeSchemaConfig` JSON format (with
backward-compatible deserialisation for the old format). Update all tests.

This is a pure refactor — existing schemas with single-segment namespaces and explicit
secret fields produce the same classifications as before, since the new default is
secret-for-everything-not-exempted.

### Layer 2 — `classify_changes` re-key

Change `Classification::secret_by_subject` key from `Uuid` to `AttributePath`.
Update `classify_changes` and all call sites. Update `attribute_schema` tests.

This is a pure refactor with no behaviour change for existing code paths
(existing schemas have one-to-one path→UUID mappings).

### Layer 3 — `SecretPartitionData` field rename

Rename `person_ref: String` to `role_path: AttributePath` in
`SecretPartitionData`. Update `SetAttributes` handler, `pii_codec.rs`, and all
tests. Add codec shim for backward-compatible deserialisation of old events.

### Layer 4 — `CaptureSubject`, `BindSubject`, `CaptureAndBindSubject` commands

Add the new commands and events. Add `subjects: BTreeMap<Uuid, SubjectRegistration>`
and `bindings: BTreeMap<AttributePath, Uuid>` to the aggregate alongside `persons`
(keep both during transition). Add stable-key validation. Wire up `SubjectLookupHook`
to fire on `SubjectCaptured` in addition to `PersonCaptured`.

### Layer 5 — `SetAttributes` subject lookup

Switch `SetAttributes` to use `self.bindings` + `self.subjects` for the subject lookup.
Retain `self.persons` for `PersonCaptured` replay compatibility but stop writing new
entries into it.

### Layer 6 — Deprecate `CapturePerson` / `CapturePersonDetails`

Mark the commands deprecated. Internally they emit `SubjectCaptured` +
`SubjectBound` under the `"persons/<ref>"` path. The event types must remain
decodable regardless.

### Layer 7 — Read model migration

Update `journey_person` / `journey_subject` DDL. Update the view projector to
handle `PersonCaptured`, `SubjectCaptured`, and `SubjectBound` events.
