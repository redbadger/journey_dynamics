# es-capture

A reusable, domain-agnostic **progressive-capture spine** for event-sourced
systems that need per-subject crypto-shredding (GDPR right-to-erasure) under
dynamic, externalised rules.

`es-capture` holds the machinery that is generic across domains — JSON-pointer
attribute handling, privacy classification, the subject registry, validation,
the optional decision-engine seam, and the CQRS assembly — so a new
event-sourced domain is mostly **configuration + types + (optional) rules**
rather than new aggregate code.

It builds on [`cqrs-es`](https://crates.io/crates/cqrs-es) for event sourcing and
[`cqrs-es-crypto`](../cqrs-es-crypto) for the encryption / crypto-shredding layer.

---

## The idea

Most "capture a form, encrypt the PII, allow erasure" domains differ only in:

- **which attributes exist** and **how each is classified** (plaintext vs.
  encrypted-per-subject),
- an optional **JSON Schema** for structural validation,
- optional **workflow rules** (what to suggest next), and
- the **read models** they project.

Everything else — the aggregate, its commands and events, the subject lifecycle,
the encrypt/decrypt/redact codec — is identical. `es-capture` factors that
"everything else" into one generic aggregate, [`CaptureAggregate<C>`](#the-aggregate),
so a domain only supplies the parts above.

```
your domain  =  CaptureConfig (a TYPE string)
             +  AttributeSchema (PII classification)
             +  optional JSON Schema  (validation)
             +  optional DecisionEngine (rules)
             +  your own read models / projections
```

See [`crates/journey_dynamics`](../journey_dynamics) (the Journey domain) for a
full real-world consumer: `Journey = CaptureAggregate<JourneyConfig>`.

---

## The aggregate

[`CaptureAggregate<C: CaptureConfig>`](src/aggregate.rs) is a ready-made
`cqrs_es::Aggregate`. The only per-domain input is a zero-sized marker that
supplies the aggregate `TYPE`:

```rust
use es_capture::aggregate::{CaptureAggregate, CaptureConfig};

pub struct PersonConfig;
impl CaptureConfig for PersonConfig {
    const TYPE: &'static str = "Person";
}

/// The aggregate, specialised for your domain — no behaviour of its own.
pub type Person = CaptureAggregate<PersonConfig>;
```

The command, event, and error enums are shared across all domains:

| `CaptureCommand` | Emits | Purpose |
|---|---|---|
| `Start { id }` | `Started` | Create an aggregate instance. |
| `SetAttributes { changes }` | `AttributesSet` (+ `WorkflowEvaluated` if an engine is configured) | Apply a flat map of JSON-Pointer → value; each path is routed to plaintext or per-subject-encrypted storage by the schema. |
| `RegisterSubject { subject_id, email }` | `SubjectRegistered` | Register a data subject (idempotent; a new email updates the record). |
| `BindSubject { role_path, subject_id }` | `SubjectBound` | Bind a registered subject to a role path (e.g. `/persons/passenger_0`). |
| `RegisterAndBindSubject { role_path, subject_id, email }` | `SubjectRegistered` + `SubjectBound` | The two above in one command. |
| `ForgetSubject { subject_id }` | `SubjectForgotten` | Audit event recorded **after** the DEK is deleted. |
| `Complete` | `Completed` | Close the aggregate to further attribute changes. |

State accumulates into `shared_data` — a single JSON tree holding the plaintext
attributes plus the decrypted secret values (it is never encrypted at rest, and
remains intact for non-shredded subjects after any erasure). Accessors:
`shared_data()`, `state()`, `subjects()`, `bindings()`, `latest_workflow_decision()`.

---

## Attribute classification

Every attribute is addressed by an RFC6901 **JSON Pointer** (e.g.
`/search/origin`, `/persons/passenger_0/passport`). An
[`AttributeSchema`](src/attribute_schema.rs) classifies each path as one of:

```rust
pub enum PiiClass {
    Plaintext,                       // stored verbatim in shared_data
    Secret { subject: PointerBuf },  // encrypted under the DEK of the subject
                                     // bound to `subject` (the role path)
}
```

A schema resolves a path in this order: **exact entry → namespace pattern →
plaintext prefix → permissive fallback → unknown (rejected)**. Build one to suit
your domain:

```rust
use std::collections::BTreeMap;
use es_capture::attribute_schema::{AttributeEntry, AttributeSchema, NamespacePattern, PiiClass};

// Explicit mode: only declared paths are known; anything else is rejected.
let explicit = AttributeSchema::new(
    BTreeMap::from([
        ("/self/firstName".parse().unwrap(),
         AttributeEntry::new(PiiClass::Secret { subject: "/self".parse().unwrap() })),
        ("/self/country".parse().unwrap(),
         AttributeEntry::new(PiiClass::Plaintext)),
    ]),
    None, // optional JSON Schema value for structural validation
);

// Dynamic three-segment namespaces: `/persons/{ref}/{field}` is Secret under
// `/persons/{ref}`, except the listed plaintext suffixes.
let namespaced = AttributeSchema::new(BTreeMap::new(), None)
    .with_plaintext_prefixes(vec!["/search".parse().unwrap(), "/booking".parse().unwrap()])
    .with_namespace_patterns(vec![NamespacePattern {
        prefix: "/persons".parse().unwrap(),
        plaintext_suffixes: ["/passengerType".parse().unwrap()].into_iter().collect(),
    }]);

// Permissive: every unmatched path is plaintext (handy for tests / bootstrap).
let permissive = AttributeSchema::permissive();
```

A serializable form (`AttributeSchemaConfig`) lets you load a schema from JSON at
runtime — see how `journey_dynamics` reads `JOURNEY_ATTRIBUTE_SCHEMA_PATH`.

---

## Subjects and crypto-shredding

PII is encrypted per **data subject**. A subject is registered (`subject_id` +
`email`) and bound to a **role path**; secret attributes under that role are
encrypted with the subject's Data Encryption Key (DEK). The
[`SubjectRegistry`](src/subject_registry.rs) tracks registrations and bindings
and answers the resolution and idempotency questions the aggregate needs
(`resolve_active`, `needs_registration`, `check_binding`, …).

The encrypt/decrypt/redact itself happens in the `cqrs-es-crypto` layer via the
[`AttributesSetCodec`](src/attributes_set_codec.rs) — a domain-agnostic
`PiiEventCodec` for the `AttributesSet` event. It encrypts each subject's
`changes` into its own partition (labelled by role path), decrypts them on read
when the DEK is present, and writes a `{"/redacted": true}` sentinel once the DEK
is gone. **Erasure is a single `KeyStore::delete_key(subject_id)`** — and because
DEKs are subject-scoped, that one deletion shreds the subject's PII across every
aggregate and event in which they appear.

```rust
use std::sync::Arc;
use cqrs_es::{CqrsFramework, Query, persist::PersistedEventStore};
use cqrs_es_crypto::{CryptoShreddingEventRepository, FieldCipher, KeyStore};
use es_capture::aggregate::CaptureServices;
use es_capture::attributes_set_codec::AttributesSetCodec;

// 1. Services: schema + validator (+ optional decision engine).
let services = CaptureServices::without_decision_engine(validator, attribute_schema);
// or, with rules:  CaptureServices::new(decision_engine, validator, attribute_schema);

// 2. Wrap any PersistedEventRepository with the crypto layer, using the
//    generic AttributesSetCodec.
let repo = CryptoShreddingEventRepository::new(
    inner_repo, key_store, FieldCipher::new(), Arc::new(AttributesSetCodec),
);

// 3. Standard cqrs-es assembly.
let store   = PersistedEventStore::new_event_store(repo);
let queries: Vec<Box<dyn Query<Person>>> = vec![/* your projections */];
let cqrs    = CqrsFramework::new(store, queries, services);
```

For a complete production assembly — including the KEK provider, transactional
DEK writes, and persist hooks — see
[`crates/journey_dynamics/src/config.rs`](../journey_dynamics/src/config.rs).

---

## Optional decision engine

If you supply a [`DecisionEngine`](src/decision_engine.rs), every `SetAttributes`
also emits a `WorkflowEvaluated { suggested_actions, phase }`. Without one,
capture still runs and emits only `AttributesSet`. Two implementations ship:

- **`SimpleDecisionEngine`** — a trivial in-process engine for tests.
- **`GoRulesDecisionEngine`** — evaluates a compiled GoRules JDM
  (`zen-engine`) model, pre-compiling expressions and running them on a
  thread-pinned worker pool.

```rust
let services = CaptureServices::new(decision_engine, validator, attribute_schema);
```

---

## Crate structure

| Module | Contents |
|---|---|
| `aggregate` | `CaptureAggregate`, `CaptureConfig`, `CaptureCommand`, `CaptureEvent`, `CaptureError`, `CaptureServices`, `CaptureState`, `SecretPartitionData`, `WorkflowDecisionState` |
| `attribute_schema` | `AttributeSchema`, `AttributeEntry`, `PiiClass`, `NamespacePattern`, `AttributeSchemaConfig`, `classify_changes` |
| `subject_registry` | `SubjectRegistry`, `SubjectRegistration` |
| `attributes_set_codec` | `AttributesSetCodec` — the `cqrs_es_crypto::PiiEventCodec` for `AttributesSet` |
| `capture` | The pure `capture()` pipeline: classify → validate → evaluate, producing a `CaptureOutcome` |
| `decision_engine` | `DecisionEngine` trait, `WorkflowDecision`, `SimpleDecisionEngine`, `GoRulesDecisionEngine` |
| `schema_validator` | `SchemaValidator` trait, `NoOpValidator`, `JsonSchemaValidator` |
| `json_path` | `flatten` (tree → path map) and `assign_all` (path map → tree) |

---

## Testing

The aggregate works with `cqrs_es::test::TestFramework` for given/when/then
behaviour tests, and with `cqrs-es-crypto`'s `testing` feature
(`InMemoryEventRepository` + `InMemoryKeyStore`) for full in-memory
encrypt/shred round-trips — no database required.

```rust
use cqrs_es::test::TestFramework;
use es_capture::aggregate::{CaptureCommand, CaptureEvent};

type Tester = TestFramework<Person>;

#[test]
fn start_emits_started() {
    let id = uuid::Uuid::new_v4();
    Tester::with(person_services())
        .given_no_previous_events()
        .when(CaptureCommand::Start { id })
        .then_expect_events(vec![CaptureEvent::Started { id }]);
}
```

See [`crates/journey_dynamics/src/domain/journey.rs`](../journey_dynamics/src/domain/journey.rs)
for an extensive behaviour-test suite over this aggregate.

---

## Design notes

For the full design and the extraction plan that produced this crate, see
[`docs/REUSABLE_ES_FOUNDATION.md`](../../docs/REUSABLE_ES_FOUNDATION.md).
