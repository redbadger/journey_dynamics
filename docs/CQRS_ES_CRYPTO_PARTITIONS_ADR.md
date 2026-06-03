# `cqrs-es-crypto` — Multi-subject partitioned ciphertext

**Status:** Draft — to be folded into `crates/cqrs-es-crypto/README.md` as
part of step **A5.8** of
[`PATH_KEYED_ATTRIBUTES_PLAN.md`](./PATH_KEYED_ATTRIBUTES_PLAN.md).
**Companion to:**
[`PATH_KEYED_ATTRIBUTES_DESIGN.md`](./PATH_KEYED_ATTRIBUTES_DESIGN.md).

This document is written to drop into the existing README in two places:

1. A new section **"Multi-subject events"** added immediately after
   **"How it works"**.
2. A small edit to the **"How it works → Write path / Read path"** diagrams
   so they show the partitioned model.

The text below is in the README's existing voice and can be pasted verbatim.

---

## Section to add after **"How it works"**

### Multi-subject events

A single event may carry PII belonging to more than one data subject. The
canonical example is a "set passport details for passengers A and B" form
submission. To make this expressible without losing the per-subject
crypto-shredding property, every encrypted event is stored as a **list of
subject-scoped partitions** rather than a single ciphertext.

```
event payload
 ├── plaintext fields …
 ├── subjects:             ["<uuid-A>", "<uuid-B>"]   (plaintext peer array)
 └── encrypted_partitions: [
       { subject_id: "<uuid-A>", label: "passenger_0", nonce, ciphertext },
       { subject_id: "<uuid-B>", label: "passenger_1", nonce, ciphertext },
     ]
```

Each partition is encrypted under its own subject's DEK with its own AAD
(see below). Subjects are independent: shredding subject A's DEK redacts
A's partition only and leaves B's partition decryptable in the same event.

Most events still carry a single partition (or none), which is exactly the
shape the derive macro emits for variants annotated with one
`#[pii(subject)]` field. The list-of-partitions shape is what hand-written
codecs reach for when an event variant intentionally fans out across
subjects.

#### `SecretPartition`, `EncryptedPartition`, `DecryptedPartition`

| Type | Direction | Carries |
|------|-----------|---------|
| `SecretPartition` | write-path input | `subject_id`, `label`, cleartext `payload: Vec<u8>` |
| `EncryptedPartition` | on-disk | `subject_id`, `label`, `nonce`, `ciphertext` |
| `DecryptedPartition` | read-path output | `subject_id`, `label`, cleartext `payload: Vec<u8>` |

`label` is an opaque, within-event identifier chosen by the codec. It
tells `reconstruct` which decrypted bytes belong to which field/partition
on the way back. Conventional values:

- `"default"` — a single-subject event written by the derive macro.
- A field name (e.g. `"data"`) — multiple PII fields on one variant that
  share a subject but are stored as separate partitions.
- A journey-local slot name (e.g. `"passenger_0"`) — a hand-written codec
  routing per-person changes into separate partitions.

#### Per-partition AAD

Each partition's additional authenticated data is the concatenation:

```
aggregate_id || sequence || subject_id || label
```

This makes partitions non-fungible across:

- **Events** (aggregate + sequence) — a partition cannot be replayed into
  a different event.
- **Subjects within one event** (subject_id) — a partition cannot be
  re-tagged to point at a different subject.
- **Labels within one event** (label) — two partitions belonging to the
  same subject cannot be swapped to put data under the wrong field.

The old AAD format `"<aggregate_id>:<sequence>"` is recognised on read for
back-compat (see below); new writes always use the per-partition scheme.

#### Read path: independent decryption per partition

For each partition the repository:

1. Looks up the DEK via `KeyStore::get_key(partition.subject_id)`.
2. If found, decrypts the ciphertext with the partition's AAD and adds a
   `DecryptedPartition` to the output list.
3. If `NotFound`, adds the partition's `label` to a `redacted_labels` list
   passed to `redact_partitions` after reconstruction.

The codec's `reconstruct(event, decrypted_partitions)` reattaches surviving
partitions, and `redact_partitions(event, labels)` stamps the
codec-defined sentinel onto the rest. A single event can therefore end up
with some partitions decrypted, some redacted, and some plaintext fields
fully intact — all at once.

#### Backward compatibility

Events written before this version of the crate use a single inline
ciphertext field (e.g. `encrypted_pii` or a custom sentinel name) plus a
top-level `subject_id`. The read path detects this shape and translates it
to a one-partition vector with `label = "default"` before handing it to
the codec. The codec sees only the new shape.

The legacy AAD format `"<aggregate_id>:<sequence>"` is accepted on read
for partitions that originate from up-cast legacy events. New writes
always emit the per-partition AAD.

There is **no on-disk migration** — old events keep their inline shape,
new events are written as partition lists. The codec is the only piece
that needs to be aware of both forms, and only on the read path.

> **Why no on-disk rewrite?** Re-encrypting the historical event log would
> require touching every event under every DEK, which is exactly the work
> crypto-shredding is designed to avoid. The translation layer is cheap
> and runs only on read.

---

## Section to add to **"Manual implementation"** (replaces today's "The trait has four methods" block)

### The trait

```rust
pub trait PiiEventCodec: Send + Sync {
    /// Identify partitions to encrypt. Empty vec = pure-plaintext event.
    fn extract_partitions(&self, event: &SerializedEvent)
        -> Result<Vec<SecretPartition>, PiiCodecError>;

    /// Reattach decrypted partitions to a serialized event by `label`.
    fn reconstruct(&self, event: &mut SerializedEvent,
        partitions: Vec<DecryptedPartition>) -> Result<(), PiiCodecError>;

    /// Redact partitions whose DEK has been deleted.
    fn redact_partitions(&self, event: &mut SerializedEvent,
        labels: &[String]) -> Result<(), PiiCodecError>;
}
```

**Write path:**
- `extract_partitions(event)` — inspect an unencrypted event and return
  zero or more `SecretPartition`s. The repository encrypts each under its
  named subject's DEK and writes them to `encrypted_partitions` on the
  serialised event. An empty `Vec` means "pass through unchanged".

**Read path:**
- `reconstruct(event, partitions)` — given the partitions the repository
  was able to decrypt, write each one's cleartext back into the event
  under whatever shape the codec wants. Partitions are bucketed by
  `label`; the codec routes them.
- `redact_partitions(event, labels)` — for each label whose DEK has been
  deleted, stamp the codec-defined sentinel onto the event in place.

#### `SingleSubjectCodec` adapter

For codecs that conceptually carry a single subject per event, implement
the simpler `SingleSubjectCodec` trait and use the `as_pii_event_codec()`
adapter:

```rust
pub trait SingleSubjectCodec: Send + Sync {
    fn classify(&self, event: &SerializedEvent)
        -> Option<(Uuid, Vec<u8>)>;          // (subject_id, payload)
    fn reconstruct_single(&self, event: &mut SerializedEvent,
        subject_id: Uuid, payload: Vec<u8>) -> Result<(), PiiCodecError>;
    fn redact_single(&self, event: &mut SerializedEvent)
        -> Result<(), PiiCodecError>;
}
```

The adapter wraps a `SingleSubjectCodec` as a `PiiEventCodec` that always
emits a one-element partition with `label = "default"`. This is the path
the derive macro uses internally.

---

## Section to add to **"Required database schema"**

### Indexing events by subject

To find every aggregate that touched a given subject (typically for
GDPR right-to-erasure), index the plaintext `subjects` array per event
type:

```sql
-- Legacy single-subject event types (compatibility):
CREATE INDEX idx_events_person_captured_subject
    ON events ((payload -> 'PersonCaptured' ->> 'subject_id'))
    WHERE event_type = 'PersonCaptured';

-- Multi-subject event types (new shape):
CREATE INDEX idx_events_attributes_set_subjects
    ON events USING GIN ((payload -> 'AttributesSet' -> 'subjects'))
    WHERE event_type = 'AttributesSet';
```

The plaintext `subjects` array is emitted automatically by the repository
for every event with at least one partition — codecs do not need to
manage it.

Subject lookup queries union across both index forms while legacy events
remain in the log:

```sql
SELECT DISTINCT aggregate_id FROM events
 WHERE (event_type = 'PersonCaptured'
        AND payload -> 'PersonCaptured' ->> 'subject_id' = $1)
    OR (event_type = 'AttributesSet'
        AND payload -> 'AttributesSet' -> 'subjects'
            @> jsonb_build_array($1::text));
```

---

## Section to add to **"Security notes"**

- Per-partition AAD (`aggregate_id || sequence || subject_id || label`)
  prevents a valid partition from being transplanted across events,
  across subjects within an event, or across labels within an event.
  Legacy single-partition AAD (`"<aggregate_id>:<sequence>"`) is accepted
  on read for back-compat but never produced on write.
- Partitions are encrypted independently; one subject's compromised DEK
  does not leak any other partition in the same event.
- Crypto-shredding remains per-subject: deleting a DEK redacts every
  partition encrypted under it, across all events.
- The plaintext `subjects` array exposes which subjects an event touches.
  It is metadata-not-content (subject IDs are non-PII opaque identifiers),
  but if your application uses subject IDs that are themselves sensitive
  (e.g. hashed emails), encrypt or hash them at the application layer
  before they reach the crypto crate.

---

## Diagram updates

Replace the existing **Write path** diagram in **"How it works"** with:

```
persist(events)
  └── for each event
        ├── PiiEventCodec::extract_partitions → []          → store verbatim
        └── PiiEventCodec::extract_partitions → [p₀, p₁ …]
              └── for each partition pᵢ
                    ├── KeyStore::get_or_create_key(pᵢ.subject_id)
                    ├── AES-256-GCM encrypt(pᵢ.payload,
                    │                       aad = agg||seq||sub||label)
                    └── append EncryptedPartition to event.encrypted_partitions
              └── repository fills event.subjects = [p₀.subject_id, p₁.subject_id …]
```

Replace the existing **Read path** diagram with:

```
get_events / get_last_events / stream_events
  └── for each stored event
        ├── no encrypted_partitions  → return verbatim (or up-cast legacy shape)
        └── encrypted_partitions: [e₀, e₁ …]
              ├── decrypted := []
              ├── redacted  := []
              ├── for each eᵢ
              │     ├── KeyStore::get_key(eᵢ.subject_id)
              │     │     ├── Some(dek) → AES-256-GCM decrypt(eᵢ, aad)
              │     │     │                → push DecryptedPartition to decrypted
              │     │     └── None      → push eᵢ.label to redacted
              ├── PiiEventCodec::reconstruct(event, decrypted)
              ├── PiiEventCodec::redact_partitions(event, redacted)
              └── return updated event
```

---

## CHANGELOG entry to add to `crates/cqrs-es-crypto/CHANGELOG.md`

```markdown
## [Unreleased]

### Changed (breaking — write path)

- The on-disk encrypted-event envelope is now a list of subject-scoped
  partitions (`encrypted_partitions: [{subject_id, label, nonce,
  ciphertext}]`) with a plaintext peer array (`subjects: [uuid, …]`),
  replacing the previous single-ciphertext-per-event shape.
- The `PiiEventCodec` trait surface changes from
  `classify` / `extract_encrypted` / `reconstruct` / `redact` to
  `extract_partitions` / `reconstruct` / `redact_partitions`. See the
  README for the new shape.
- The derive macro emits the new trait surface automatically. Existing
  `#[derive(PiiCodec)]` callers should compile unchanged, but the
  expanded code is now partition-aware.
- AAD is now `aggregate_id || sequence || subject_id || label`
  per partition. Legacy AAD (`<aggregate_id>:<sequence>`) is accepted on
  read only.

### Added

- `SecretPartition`, `EncryptedPartition`, `DecryptedPartition` public types.
- `SingleSubjectCodec` adapter trait for codecs that intentionally carry
  one subject per event.
- Per-partition decryption: a single event may now have some partitions
  decrypted and some redacted simultaneously (when one subject has been
  shredded and another has not).

### Backward compatibility

- Events written before this release decrypt and redact unchanged. The
  read path detects the legacy single-ciphertext shape and treats it as a
  one-partition vector with `label = "default"`.
- No on-disk migration is required. New writes use the partitioned shape;
  old writes stay in their original shape.
```

---

## Notes for the implementing agent

1. The "section to add after How it works" is the main contribution. The
   other sections (Manual implementation, Required database schema,
   Security notes, diagrams) are edits to existing material — preserve
   what is there and add/replace as described.
2. Don't fold this whole document in as one block. Splice each section
   into the matching place in the README so the flow remains
   logically ordered (quick start → manual → how it works →
   multi-subject → database schema → testing → features → structure →
   security).
3. After folding in the new content, delete this file
   (`docs/CQRS_ES_CRYPTO_PARTITIONS_ADR.md`) and reference the README
   from the design doc. The ADR lived here only as a staging document.
