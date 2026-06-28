# HR example

A standalone example built **directly** on the [`es-capture`](../../crates/es-capture)
spine. It models two aggregates — `Person` and `Employment` — and demonstrates
**cross-aggregate crypto-shredding**: the same human is the same data subject in
both aggregates, so a single right-to-erasure makes their PII unreadable
everywhere it lives.

The whole stack runs in memory (`InMemoryEventRepository` + `InMemoryKeyStore`),
so there is no Postgres and no HTTP server to set up.

## What it shows

- **Two aggregates, no aggregate code.** `Person` and `Employment` are just
  `CaptureAggregate<C>` specialisations selected by a one-line `CaptureConfig`
  marker. They differ only in their *attribute schema*.
- **A shared data subject.** One person (`subject_id`) is bound under `/self` in
  `Person` and under `/employee` in `Employment`.
- **Cross-aggregate erasure.** Deleting the subject's Data Encryption Key once
  (`KeyStore::delete_key`) redacts the encrypted fields in **both** aggregates;
  plaintext fields are untouched.
- **No decision engine.** Services are built with
  `CaptureServices::without_decision_engine`, so capture runs but emits no
  `WorkflowEvaluated` events.

## PII classification

| Aggregate    | Role path    | Secret (encrypted per-subject)                                     | Plaintext                                              |
| ------------ | ------------ | ----------------------------------------------------------------- | ----------------------------------------------------- |
| `Person`     | `/self`      | `/self/firstName`, `/self/lastName`, `/self/dateOfBirth`, `/self/nationalInsuranceNumber` | `/self/country`                                        |
| `Employment` | `/employee`  | `/employee/salary`, `/employee/bankAccountNumber`, `/employee/bankSortCode` | everything under `/employment` (e.g. `personId`, `jobTitle`, `department`, `startDate`) |

`Person` uses an **explicit** schema (any path not listed is rejected);
`Employment` mixes explicit secret entries with a `/employment` plaintext-prefix
rule. See [`src/lib.rs`](src/lib.rs).

## Running

```sh
# from examples/hr/
cargo run      # the hire → read → erase → read demo
cargo test     # behaviour tests + the cross-aggregate shred test
```

### Expected demo output (abridged)

```
── After hiring ──
Person:     { "self": { "firstName": "Ada", … "country": "UK" } }
Employment: { "employee": { "salary": 145000, … }, "employment": { "jobTitle": "Principal Engineer", … } }

>> Right-to-erasure … Deleting the data encryption key once …

── After erasure (PII unreadable in BOTH; plaintext intact) ──
Person:     { "redacted": true, "self": { "country": "UK" } }
Employment: { "redacted": true, "employment": { "jobTitle": "Principal Engineer", … } }
```

After erasure the secret fields are gone (the decrypted view shows a `redacted`
marker in their place) while plaintext survives — from a single `delete_key`.

## Usage

### Wire up the system

`HrApp::build()` assembles one shared event log and one shared key store backing
both aggregate frameworks plus a read-only reader:

```rust
use hr::HrApp;

let app = HrApp::build();
// app.person      : HrCqrs<PersonConfig>
// app.employment  : HrCqrs<EmploymentConfig>
// app.key_store   : Arc<dyn KeyStore>   (shared — the cross-aggregate seam)
// app.reader      : HrRepo              (read-only, same log + key store)
```

### Hire a person (dispatch commands)

```rust
use hr::{PERSON_ROLE, attrs, ptr};
use es_capture::aggregate::CaptureCommand;
use serde_json::json;
use uuid::Uuid;

let subject = Uuid::new_v4();        // the data subject (shared across aggregates)
let person_id = Uuid::new_v4();
let pid = person_id.to_string();

app.person.execute(&pid, CaptureCommand::Start { id: person_id }).await?;

app.person.execute(&pid, CaptureCommand::RegisterAndBindSubject {
    role_path: ptr(PERSON_ROLE),     // "/self"
    subject_id: subject,
    email: "ada@example.com".into(),
}).await?;

app.person.execute(&pid, CaptureCommand::SetAttributes {
    changes: attrs(vec![
        ("/self/firstName", json!("Ada")),            // secret
        ("/self/nationalInsuranceNumber", json!("QQ123456C")), // secret
        ("/self/country", json!("UK")),               // plaintext
    ]),
}).await?;
```

Register the **same `subject`** under `/employee` in the `Employment` aggregate
to link the two; secret fields there (e.g. `/employee/salary`) are then encrypted
under that subject's key too.

### Read an aggregate's current view

`read_state` folds the persisted events back through the crypto layer, so secrets
are decrypted while the key exists:

```rust
use hr::{PersonConfig, read_state};

let view = read_state::<PersonConfig>(&app.reader, person_id).await;
assert_eq!(view["self"]["firstName"], serde_json::json!("Ada"));
```

### Right-to-erasure (the headline)

One key deletion erases the subject's PII across **every** aggregate it appears
in:

```rust
app.key_store.delete_key(&subject).await?;   // cryptographic erasure

// optional: record the GDPR audit event in each aggregate's history
app.person.execute(&pid, CaptureCommand::ForgetSubject { subject_id: subject }).await?;

// now secrets are unrecoverable in both Person and Employment
let view = read_state::<PersonConfig>(&app.reader, person_id).await;
assert!(view["self"].get("firstName").is_none());
assert_eq!(view["self"]["country"], serde_json::json!("UK")); // plaintext survives
```

The cryptographic erasure is `delete_key`; `ForgetSubject` only records an audit
event in the aggregate's stream.

## Layout

| File                       | Purpose                                                            |
| -------------------------- | ----------------------------------------------------------------- |
| [`src/lib.rs`](src/lib.rs)   | Aggregate configs, attribute schemas, services, `SharedRepo`, `HrApp` wiring, `read_state` |
| [`src/main.rs`](src/main.rs) | The runnable hire → read → erase → read demo                       |
| [`src/tests.rs`](src/tests.rs) | `TestFramework` behaviour tests + the cross-aggregate shred test |
