# Implementation Plan: CaptureSubject & Generic Data Subjects

This document outlines the phased implementation of the generic data subject registration system, as designed in `docs/CAPTURE_SUBJECT_DESIGN.md`.

The implementation is divided into three main phases to ensure backward compatibility and allow for incremental verification.

## Phase 1: Infrastructure & Schema Refactoring
This phase focuses on the underlying classification and partitioning logic without changing how commands are actually processed.

### Layer 1: Generalise `NamespacePattern`
*   **Goal:** Move from fixed 3-segment namespaces to prefix-based matching.
*   **Changes:**
    *   Modify `NamespacePattern` struct: replace `namespace: String` with `prefix: AttributePath` and consolidate `secret_fields` / `plaintext_fields` into `plaintext_suffixes`. The new default is **secret for everything not exempted**.
    *   Rewrite `classify` to match at arbitrary depth by stripping the prefix, extracting the role-ref segment, and checking the remaining suffix against `plaintext_suffixes`.
    *   Update `AttributeSchemaConfig` / `NamespacePatternConfig` to the new JSON format. Add serde aliases (`namespace` → `prefix`, `plaintext_fields` → `plaintext_suffixes`) so that existing config files with the old format continue to deserialise correctly. The old `secret_fields` key is silently ignored on read (those fields remain secret under the new default).
    *   Update all `attribute_schema` tests.

### Layer 2: `classify_changes` Re-key
*   **Goal:** Shift the secret-partitioning key from `Uuid` to `AttributePath` (role path), while still threading the UUID through for encryption.
*   **Changes:**
    *   Update `Classification::secret_by_subject` from `BTreeMap<Uuid, BTreeMap<AttributePath, Value>>` to `BTreeMap<AttributePath, (Uuid, BTreeMap<AttributePath, Value>)>` — keyed by role path, value is `(subject_uuid, changes)`.
    *   Update `classify_changes` to group by the `subject` `AttributePath` from `PiiClass::Secret`, storing the resolved UUID as the first element of the tuple.
    *   Remove the manual `strip_prefix("persons/")` reverse-lookup in `journey.rs`; the role path now flows directly out of `classify_changes`.
    *   Update all call sites and tests.

### Layer 3: `SecretPartitionData` Field Rename + Type Change
*   **Goal:** Align the event store with the new role-path terminology.
*   **Changes:**
    *   In `SecretPartitionData`: rename `person_ref: String` → `role_path: AttributePath` (note: this is both a field rename **and** a type change; the JSON shape changes).
    *   Update the `SetAttributes` handler to build partitions directly from `classification.secret_by_subject` — no `subject_to_ref` reverse map required.
    *   Update `pii_codec.rs` with a backward-compat deserialisation shim so that stored events using the old `person_ref` key can still be decoded.
    *   Update all tests.

---

## Phase 2: Domain Logic Migration
This phase introduces the new Subject/Binding concepts and migrates the aggregate state.

### Layer 4: New Subject Commands + `ForgetSubject` Update
*   **Goal:** Implement the primitives for registering and binding subjects; update erasure to use the new subject map.
*   **Changes:**
    *   **New Commands:** `CaptureSubject`, `BindSubject`, and the composite `CaptureAndBindSubject`.
    *   **New Events:** `SubjectCaptured`, `SubjectBound`.
    *   **Aggregate State:** Add `subjects: BTreeMap<Uuid, SubjectRegistration>` and `bindings: BTreeMap<AttributePath, Uuid>` to the `Journey` aggregate alongside `persons` (keep both during transition).
    *   **Validation:** Implement stable-key validation to prevent duplicate bindings.
    *   **Hooks:** Update `SubjectLookupHook` to trigger on `SubjectCaptured`.
    *   **`ForgetSubject` update:** Change the `ForgetSubject` handler to check `self.subjects` (setting `reg.forgotten = true`) instead of iterating `self.persons`. Update the `apply` for `SubjectForgotten` similarly. `self.persons` remains readable for replay but the forgetting logic moves to the new map.

### Layer 5: Update `SetAttributes` Lookup
*   **Goal:** Use the new binding system for attribute placement.
*   **Changes:**
    *   Update the `SetAttributes` handler's `subject_lookup` closure to resolve via `self.bindings` + `self.subjects` (checking `!reg.forgotten`).
    *   Remove the old `strip_prefix("persons/")` reverse-lookup (superseded by Layer 2 re-keying and the `bindings` map).
    *   Retain `self.persons` in the aggregate for `PersonCaptured` replay compatibility but stop writing new entries to it.

---

## Phase 3: Cleanup & Persistence
The final phase removes deprecated paths and updates the read models.

### Layer 6: Deprecate `CapturePerson`
*   **Goal:** Phase out the old "Person-centric" API.
*   **Changes:**
    *   Mark `CapturePerson` and `CapturePersonDetails` as deprecated.
    *   Re-implement these commands as internal aliases that emit `SubjectCaptured` and `SubjectBound` under the `persons/<ref>` path to maintain existing behaviour.
    *   `PersonCaptured` and `PersonDetailsUpdated` event types must remain decodable regardless.

### Layer 7: Read Model Migration
*   **Goal:** Update the database to reflect the new Subject/Binding relationship.
*   **Changes:**
    *   **DDL:** Create `journey_subject` table; update or replace `journey_person`.
    *   **Projector:** Update the view projector to handle `SubjectCaptured` and `SubjectBound` events in addition to legacy `PersonCaptured`.
    *   **Migration:** Write a data migration to backfill existing `person_ref` data into the new subject/binding tables.

## Risk Mitigation
- **Backward Compatibility:** Every layer includes a shim or compatibility step (serde aliases, codec shims, maintaining `self.persons` during transition) to ensure old configs and old events continue to work.
- **Incremental Rollout:** Separating the classification refactor (Phase 1) from the domain changes (Phase 2) lets us verify PII partitioning correctness before touching how subjects are registered.
