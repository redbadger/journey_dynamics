# Migrating to Path-Keyed Attributes

**Audience:** engineers maintaining applications, services, or projections
that depend on `journey_dynamics`.

**TL;DR.** A new command/event pair (`SetAttributes` / `AttributesSet`)
replaces the step-scoped `Capture` / `CapturePersonDetails` and their
events. The old surface is still fully functional but emits compile-time
deprecation warnings. This guide shows how to clear those warnings.

If you only want the cheat sheet, jump to
[Quick reference](#quick-reference).

> Companion documents:
>
> - [`PATH_KEYED_ATTRIBUTES_DESIGN.md`](./PATH_KEYED_ATTRIBUTES_DESIGN.md)
>   — the proposal that motivated the change.
> - [`PATH_KEYED_ATTRIBUTES_PLAN.md`](./PATH_KEYED_ATTRIBUTES_PLAN.md) —
>   the implementation plan inside `journey_dynamics`.

---

## What changed at the conceptual level

A journey used to be a sequence of steps. Each `Capture` submission named
a step (`"search"`, `"passenger_0"`, …) and carried a free-form JSON blob
that got merged into either `shared_data` or a per-person `details` blob
depending on which command you used.

A journey is now a bag of `(path, value)` pairs. The path is the schema
key. Steps no longer exist on the wire. "Which form should the UI show
next" is a derived label (`phase`) computed by the decision engine, not
a command parameter.

A submission carries one or more attributes by path:

```json
{ "SetAttributes": { "changes": {
    "search/origin":                          "LHR",
    "search/destination":                     "JFK",
    "persons/passenger_0/passportNumber":     "GB123456789",
    "persons/passenger_0/passengerType":      "adult"
} } }
```

The application classifies each path as plaintext or secret (PII)
through an `AttributeSchema`. Secrets are encrypted under the right
subject's DEK by the crypto layer; plaintext attributes are stored
verbatim in `shared_data`.

## Backward-compatibility guarantee

Until a future, separately-scheduled removal RFC:

- `JourneyCommand::Capture` and `CapturePersonDetails` are still accepted
  and behave identically to today.
- They still emit `JourneyEvent::Modified` and
  `JourneyEvent::PersonDetailsUpdated` respectively. Pattern-matchers on
  those variants continue to fire for new submissions made via the
  deprecated commands.
- `Journey::current_step()`, `JourneyView::current_step`, and
  `PersonSlot.details` / `PersonView.details` keep being populated.
  `details` is kept in sync by a permanent mirror-write from
  `AttributesSet`, so even consumers that still read `details` will see
  fresh data from clients that have already migrated.
- The HTTP route accepts both the legacy and the new command shapes.

You can migrate writers and readers independently, on your own schedule.

---

## Quick reference

| Deprecated | Replacement |
| --- | --- |
| `JourneyCommand::Capture { step, data }` | `JourneyCommand::SetAttributes { changes }` with paths under `<step>/…` |
| `JourneyCommand::CapturePersonDetails { person_ref, data }` | `JourneyCommand::SetAttributes { changes }` with paths under `persons/<ref>/…` |
| `JourneyEvent::Modified { step, data }` | `JourneyEvent::AttributesSet { plaintext, secret_partitions }` |
| `JourneyEvent::PersonDetailsUpdated { … }` | One entry in `AttributesSet.secret_partitions` |
| `JourneyEvent::StepProgressed { … }` | _No replacement._ Read `WorkflowDecisionView.phase` instead. |
| `Journey::current_step()` / `JourneyView::current_step` | `WorkflowDecisionView.phase` |
| `PersonSlot.details` / `PersonView.details` | Read `JourneyView::shared_data` under `persons/<ref>/…` |

| New capabilities | What it gives you |
| --- | --- |
| Multi-subject submissions in one command | Update two passengers in one request; each subject's PII goes into its own ciphertext partition. |
| Per-path PII classification | The crypto layer routes per attribute, not per event variant. No more "shape the data wrongly to make it visible to the decision engine". |
| Path-level redaction within an event | When one subject is crypto-shredded but another is not, only the shredded paths are redacted in the same event. |

---

## Migration recipes

### 1. Replace a non-PII `Capture` with `SetAttributes`

**Before:**

```rust
client.execute(&id, JourneyCommand::Capture {
    step: "search".into(),
    data: json!({
        "origin": "LHR",
        "destination": "JFK",
        "departureDate": "2025-08-15",
    }),
}).await?;
```

**After:**

```rust
client.execute(&id, JourneyCommand::SetAttributes {
    changes: BTreeMap::from([
        (AttributePath::new("search/origin")?,        json!("LHR")),
        (AttributePath::new("search/destination")?,   json!("JFK")),
        (AttributePath::new("search/departureDate")?, json!("2025-08-15")),
    ]),
}).await?;
```

Notes:

- Each leaf becomes its own `(path, value)` pair. Nested objects on the
  old wire become slash-separated paths on the new wire.
- The old "step" (`"search"`) becomes the first path segment.
- The decision engine receives a tree rehydrated from these paths, so
  rule logic that previously read `data.search.origin` continues to
  work without change.

If you would rather not flatten by hand, use the nested-sugar form on
the HTTP wire (B3 in the implementation plan):

```json
{ "SetAttributes": {
    "search": {
        "origin":        "LHR",
        "destination":   "JFK",
        "departureDate": "2025-08-15"
    }
} }
```

The extractor flattens this server-side. The canonical form remains
`{ "changes": { … } }`.

### 2. Replace a `CapturePersonDetails` submission

**Before:**

```rust
client.execute(&id, JourneyCommand::CapturePersonDetails {
    person_ref: "passenger_0".into(),
    data: json!({
        "passportNumber": "GB123456789",
        "dateOfBirth":    "1990-05-15",
        "passengerType":  "adult",
    }),
}).await?;
```

**After:**

```rust
client.execute(&id, JourneyCommand::SetAttributes {
    changes: BTreeMap::from([
        (AttributePath::new("persons/passenger_0/passportNumber")?, json!("GB123456789")),
        (AttributePath::new("persons/passenger_0/dateOfBirth")?,    json!("1990-05-15")),
        (AttributePath::new("persons/passenger_0/passengerType")?,  json!("adult")),
    ]),
}).await?;
```

Notes:

- `passportNumber` and `dateOfBirth` are classified `Secret { subject:
  persons/passenger_0/subject_id }` in your `AttributeSchema`, so they
  get encrypted just like before — but now under a per-attribute rule
  rather than per-command rule.
- `passengerType` is `Plaintext`, so it lands directly in `shared_data`
  where the decision engine can read it without you having to re-shape
  it. Previously you had to copy it into a summary field on `BookingData`.
- `CapturePerson` (which binds a `subject_id` to a `person_ref`) is
  **not** deprecated. Call it once per passenger before sending any
  `persons/<ref>/…` attributes.

### 3. Submit attributes for multiple subjects in one request

This is a new capability. There is no "before".

```rust
client.execute(&id, JourneyCommand::SetAttributes {
    changes: BTreeMap::from([
        (AttributePath::new("persons/passenger_0/passportNumber")?, json!("GB123456789")),
        (AttributePath::new("persons/passenger_1/passportNumber")?, json!("US987654321")),
    ]),
}).await?;
```

The aggregate groups the changes by subject, the codec encrypts each
group under its own DEK, and the resulting `AttributesSet` event carries
two partitions. If one subject is later crypto-shredded, only that
subject's partition is redacted; the other survives in the same event.

### 4. Stop reading `current_step`

`current_step` was a UI-driven label that happened to live on the
aggregate. The decision engine now publishes a coarser, schema-driven
label called `phase`.

**Before:**

```rust
let view = repo.load(&id).await?;
match view.current_step.as_deref() {
    Some("payment")     => render_payment_form(&view),
    Some("confirmation") => render_confirmation(&view),
    _ => render_default(&view),
}
```

**After:**

```rust
let view = repo.load(&id).await?;
let phase = view.latest_workflow_decision
    .as_ref()
    .and_then(|d| d.phase.as_deref());
match phase {
    Some("ready_to_pay")   => render_payment_form(&view),
    Some("completing")     => render_confirmation(&view),
    _ => render_default(&view),
}
```

`phase` values are defined by your JDM models, not the application. The
example flight-booking phases are `collecting_search`,
`collecting_passengers`, `ready_to_pay`, `completing`.

`view.latest_workflow_decision.suggested_actions` is unchanged.

### 5. Stop reading `PersonSlot.details` / `PersonView.details`

Per-passenger attributes now live in `shared_data` under
`persons/<ref>/…`. The deprecated `details` blob is still populated by a
mirror-write so existing readers don't break, but new readers should
read from the canonical location.

**Before:**

```rust
let passport = view.persons[0].details
    .get("passportNumber")
    .and_then(|v| v.as_str());
```

**After:**

```rust
let passport = view.shared_data
    .pointer("/persons/passenger_0/passportNumber")
    .and_then(|v| v.as_str());
```

Or, using the path helper:

```rust
use journey_dynamics::domain::{AttributePath, json_path};

let path = AttributePath::new("persons/passenger_0/passportNumber")?;
let passport = json_path::get_at_path(&view.shared_data, &path)
    .and_then(|v| v.as_str());
```

Note that redaction is now per-path. If the subject has been shredded,
the value at `persons/<ref>/passportNumber` is the codec's sentinel; the
non-PII path `persons/<ref>/passengerType` remains intact.

### 6. Stop pattern-matching `Modified` / `PersonDetailsUpdated` / `StepProgressed` in projections

If you've written a custom projector or analytics consumer that
pattern-matches on event variants, you have two options.

**Option A — keep matching the legacy variants.** They are still emitted
for `Capture` and `CapturePersonDetails` commands. You'll just see
deprecation warnings on the variants themselves. Wrap the arms in
`#[allow(deprecated)]` if you want a clean build until you migrate.

```rust
#[allow(deprecated)]
match event.payload {
    JourneyEvent::Modified { step, data } => { /* still works */ }
    JourneyEvent::PersonDetailsUpdated { person_ref, data, .. } => { /* still works */ }
    // … plus a new arm for AttributesSet
    _ => {}
}
```

**Option B — handle `AttributesSet` and treat the legacy variants as
equivalent.** Recommended once your writers have migrated.

```rust
match event.payload {
    JourneyEvent::AttributesSet { plaintext, secret_partitions } => {
        for (path, value) in plaintext {
            // path-based projection
        }
        for partition in secret_partitions {
            for (path, value) in partition.changes {
                // partition.subject_id, partition.person_ref, path
            }
        }
    }
    #[allow(deprecated)]
    JourneyEvent::Modified { step, data } => {
        // optional: project legacy events too, or assume they no longer
        // occur once writers have migrated
    }
    // …
}
```

There is no replacement for `StepProgressed`. If you used it to drive
UI state, switch to `phase` (recipe 4). If you used it for analytics,
emit a client-side event when the UI advances — the server no longer
knows about UI steps.

### 7. Subject lookup queries

The Postgres GIN index that backs `find_journeys_by_subject` now unions
across `PersonCaptured` (unchanged) and `AttributesSet` (new). No SQL
change is required for callers using the application's
`find_journeys_by_subject` API.

If you query the event store directly, update lookups to include
`AttributesSet`:

```sql
SELECT DISTINCT aggregate_id FROM events
 WHERE (event_type = 'PersonCaptured'
        AND payload -> 'PersonCaptured' ->> 'subject_id' = $1)
    OR (event_type = 'AttributesSet'
        AND payload -> 'AttributesSet' -> 'subjects'
            @> jsonb_build_array($1::text));
```

The `subjects` array on each `AttributesSet` event is plaintext, emitted
automatically by the crypto repository.

---

## Configuring your `AttributeSchema`

The aggregate refuses `SetAttributes` calls whose paths are not in the
schema (this is the protection against silent typos like
`searh/origin`). At application startup, build an `AttributeSchema` and
pass it to `JourneyServices::new(...)`.

```rust
use journey_dynamics::domain::{
    AttributePath, AttributeSchema, PiiClass,
};

let mut paths = BTreeMap::new();

// Plaintext attributes.
for p in [
    "search/origin",
    "search/destination",
    "search/departureDate",
    "booking/totalPrice",
    "persons/passenger_0/passengerType",
    "persons/passenger_1/passengerType",
] {
    paths.insert(AttributePath::new(p)?, PiiClass::Plaintext);
}

// PII attributes — encrypted under the named subject's DEK.
let secret_for = |person_ref: &str| PiiClass::Secret {
    subject: AttributePath::new(
        format!("persons/{person_ref}/subject_id")
    ).unwrap(),
};

for (person_ref, field) in [
    ("passenger_0", "passportNumber"),
    ("passenger_0", "dateOfBirth"),
    ("passenger_1", "passportNumber"),
    ("passenger_1", "dateOfBirth"),
] {
    paths.insert(
        AttributePath::new(format!("persons/{person_ref}/{field}"))?,
        secret_for(person_ref),
    );
}

let schema = Arc::new(AttributeSchema::new(paths, Some(json_schema_value)));
// Or pass None if you are not using JSON Schema structural validation:
// let schema = Arc::new(AttributeSchema::new(paths, None));
```

For experimentation, use `AttributeSchema::permissive()` — every path is
accepted and classified as plaintext. Do not ship this in production:
it disables typo protection and disables PII encryption.

---

## How `phase` and `suggestedActions` differ

| | `phase` | `suggested_actions` |
| --- | --- | --- |
| Cardinality | One label | A list |
| Source of truth | The JDM rule's `phase` output (if present) | The JDM rule's `suggestedActions` output |
| Used for | "Which conceptual section is the user in?" — drives big UI state | "Which forms are valid next?" — drives a list of buttons |
| Required? | Optional (`None` if the rule didn't compute one) | Always present (possibly empty) |
| Visible across replay | Yes (stored on the latest `WorkflowEvaluated` event) | Yes (same) |

Both are populated on the same `WorkflowDecisionView`. They are not
mutually exclusive; use whichever (or both) fits your UI.

---

## Crypto-shredding semantics

Unchanged in spirit, but worth restating because the on-disk shape has
shifted:

- `DELETE /subjects/{id}` and `DELETE /subjects/by-email` work
  identically.
- Crypto-shredding deletes the subject's DEK and the
  `subject_lookup` row, then emits a `SubjectForgotten` audit event on
  every affected journey.
- After shredding, every `AttributesSet` partition belonging to the
  shredded subject becomes irrecoverable. Other subjects' partitions in
  the same events remain decryptable.
- `PersonSlot.forgotten` and `PersonView.forgotten` still flip to
  `true`. Identity fields on the slot still null out.

---

## Common gotchas

### "I'm getting `UnknownAttributePath`"

You sent a path that is not in your `AttributeSchema`. Either add it or
fix the typo. In tests, you may want `AttributeSchema::permissive()` to
get unblocked.

### "My JDM rule no longer fires"

If your rule looked up `capturedData.<step>.field`, it now needs to
look up `capturedData.<step>.field` against the rehydrated tree. The
shape is the same (you wrote the paths to mimic the old nested shape),
but make sure the JDM is reading from `capturedData` directly and not
from the now-removed `currentStep` input. The flight-booking
orchestrator (`examples/flight-booking/jdm-models/`) demonstrates the
pattern.

### "I get a deprecation warning on `JourneyEvent::Modified` in my projector"

Wrap the arm in `#[allow(deprecated)]`. The variant continues to be
emitted by `Capture` commands and continues to replay from the historical
event log. There is no rush to delete the arm.

### "Two passengers' details in one request used to work via two commands; will the new single-command form be atomic?"

Yes. `SetAttributes` is one command, one event, applied atomically.
Failure (e.g. an invalid path) rejects the entire submission with no
partial writes.

### "Can I mix `Capture` and `SetAttributes` against the same journey?"

Yes, in any order. The legacy `apply` arms and the new `AttributesSet`
arm both write into the same `shared_data` document, so reading from
`shared_data` after the dust settles gives a coherent view regardless of
which commands were used. Just be aware that `Capture` always writes
under the namespace of its `step` field — if your `SetAttributes`
schema disagrees about which path that step corresponds to, you can
end up with two parallel sub-trees in `shared_data`.

### "What happens to journeys created before this release?"

They replay unchanged. The aggregate still recognises and applies
`Modified` / `PersonDetailsUpdated` / `StepProgressed` events.
Encrypted historical events use the older single-blob ciphertext shape;
the `cqrs-es-crypto` read path detects this and decrypts them as a
single-partition event with `label = "default"`.

---

## When the legacy surface will be removed

Not in this release, and not in the next one. A future RFC will measure
external usage and propose a deprecation deadline. Plan for the new
surface to be available indefinitely; plan for the legacy surface to be
removed eventually but not on a short clock. The deprecation warnings
themselves are the only ticking part of the migration timer right now.

---

## Where to find more

| Topic | Document |
| --- | --- |
| Proposal and rationale | [`PATH_KEYED_ATTRIBUTES_DESIGN.md`](./PATH_KEYED_ATTRIBUTES_DESIGN.md) |
| Implementation plan (for contributors) | [`PATH_KEYED_ATTRIBUTES_PLAN.md`](./PATH_KEYED_ATTRIBUTES_PLAN.md) |
| `cqrs-es-crypto` envelope changes | [`crates/cqrs-es-crypto/README.md`](../crates/cqrs-es-crypto/README.md) (after step A5.8 lands) |
| Flight-booking example | [`examples/flight-booking/`](../examples/flight-booking/) |
| Quick start | [`docs/QUICK_START.md`](./QUICK_START.md) |
