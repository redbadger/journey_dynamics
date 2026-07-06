# Changelog

All notable changes to `es-capture` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> `es-capture` is an internal workspace crate (`publish = false`). It was
> extracted from the Journey domain in `journey_dynamics`; see that crate's
> history for the pre-extraction lineage of these types.

## [Unreleased]

### Added

- **Initial release** — the reusable, domain-agnostic **progressive-capture
  spine** for event-sourced systems that need per-subject crypto-shredding
  (GDPR right-to-erasure) under dynamic, externalised rules. A new domain is
  mostly *configuration + types + (optional) rules* rather than new aggregate
  code.

- **`CaptureAggregate<C: CaptureConfig>`** (`aggregate` module) — a ready-made
  `cqrs_es::Aggregate` implementing the capture spine. A domain specialises it
  with a zero-sized `CaptureConfig` marker that supplies the aggregate `TYPE`;
  it carries no behaviour of its own. Exposes `shared_data()`, `state()`,
  `subjects()`, `bindings()`, and `latest_workflow_decision()`.

- **`CaptureCommand` / `CaptureEvent` / `CaptureError`** — the command, event,
  and error enums shared across domains. Commands: `Start`, `SetAttributes`,
  `RegisterSubject`, `BindSubject`, `RegisterAndBindSubject`, `ForgetSubject`,
  `Complete`. Events: `Started`, `AttributesSet`, `SubjectRegistered`,
  `SubjectBound`, `SubjectForgotten`, `WorkflowEvaluated`, `Completed`.

- **`CaptureServices`** — the collaborators the aggregate needs: an
  `AttributeSchema`, a `SchemaValidator`, and an optional `DecisionEngine`.
  Construct with `new` (with rules) or `without_decision_engine` (capture only).

- **`AttributeSchema` / `AttributeEntry` / `PiiClass` / `NamespacePattern` /
  `AttributeSchemaConfig`** (`attribute_schema` module) — per-path PII
  classification keyed by RFC6901 JSON Pointers. A path resolves as
  `Plaintext` or `Secret { subject }` via exact entry → namespace pattern →
  plaintext prefix → permissive fallback → unknown (rejected). Includes the
  pure `classify_changes` routing function and a serializable config form.

- **`SubjectRegistry` / `SubjectRegistration`** (`subject_registry` module) —
  tracks registered subjects and role-path → subject bindings, with the
  resolution and idempotency helpers the aggregate uses (`resolve_active`,
  `needs_registration`, `check_binding`, …).

- **`AttributesSetCodec`** (`attributes_set_codec` module) — a domain-agnostic
  `cqrs_es_crypto::PiiEventCodec` for the `AttributesSet` event. Encrypts each
  subject's `changes` into its own partition (labelled by role path), decrypts
  on read when the DEK is present, and writes a `{"/redacted": true}` sentinel
  once the DEK has been deleted.

- **`DecisionEngine` seam** (`decision_engine` module) — the optional
  `DecisionEngine` trait plus `WorkflowDecision`, an in-process
  `SimpleDecisionEngine`, and a `GoRulesDecisionEngine` that evaluates a
  compiled GoRules JDM (`zen-engine`) model on a thread-pinned worker pool.
  When configured, `SetAttributes` also emits `WorkflowEvaluated`.

- **`SchemaValidator`** (`schema_validator` module) — the `SchemaValidator`
  trait with `NoOpValidator` and a `jsonschema`-backed `JsonSchemaValidator`.

- **`capture()` pipeline** (`capture` module) — the pure
  classify → validate → evaluate step that produces a `CaptureOutcome`,
  separated from the aggregate's state mutation.

- **`json_path` helpers** — `flatten` (a `serde_json::Value` tree → a flat
  `PointerBuf → Value` map) and `assign_all` (a flat map → a tree).
