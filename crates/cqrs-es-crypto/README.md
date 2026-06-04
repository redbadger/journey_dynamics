# cqrs-es-crypto

Transparent PII encryption and GDPR crypto-shredding for [`cqrs-es`](https://crates.io/crates/cqrs-es).

Wraps any [`PersistedEventRepository`](https://docs.rs/cqrs-es/0.5/cqrs_es/persist/trait.PersistedEventRepository.html) with a crypto layer that:

- **Encrypts** designated PII fields on the write path using AES-256-GCM, keyed by a per-subject Data Encryption Key (DEK).
- **Decrypts** them transparently on the read path when the DEK is present.
- **Redacts** them permanently when the DEK has been deleted — this is GDPR crypto-shredding.

Each event is stored as zero or more **subject-scoped encrypted partitions**, so a
single event can carry PII belonging to several data subjects and still be
shredded one subject at a time. You describe how to split an event into
partitions by implementing the [`PiiEventCodec`](https://docs.rs/cqrs-es-crypto/latest/cqrs_es_crypto/repository/trait.PiiEventCodec.html) trait. There are two ways to do
that:

- **Hand-write the codec** (the general case). Full control, and the only way to
  support **multiple subjects per event** or classification that is decided at
  runtime (e.g. path-keyed attributes partitioned by a schema). See
  [Manual implementation](#manual-implementation).
- **`#[derive(PiiCodec)]`** (a convenience). Zero boilerplate for the common
  case of an event whose PII all belongs to a **single** subject. See
  [Quick start](#quick-start).

The derive macro is a generator for the single-subject shape; everything it emits
can be written by hand when you need more.

---

## Quick start

> This section uses `#[derive(PiiCodec)]`, which fits events whose PII all
> belongs to a **single** data subject (one partition per event). If one event
> must carry PII for **multiple** subjects, or its PII classification is decided
> at runtime, write the codec by hand instead — see
> [Manual implementation](#manual-implementation).

### 1. Add the dependency

```toml
[dependencies]
# With the derive macro (recommended):
cqrs-es-crypto = { version = "...", features = ["derive"] }

# Without:
cqrs-es-crypto = { version = "..." }
```

### 2. Annotate your event enum

Enable the `derive` feature and annotate your event enum. Unannotated variants
pass through unchanged.

```rust
use cqrs_es_crypto::PiiCodec;
use serde_json::Value;
use uuid::Uuid;

#[derive(PiiCodec)]
enum MyEvent {
    // Non-PII variants — no annotation needed.
    Started { id: Uuid },

    // Annotate PII-bearing variants.  `event_type` must match the string
    // returned by `DomainEvent::event_type()` for this variant.
    // `sentinel` names the encrypted-blob field in the stored JSON payload
    // (defaults to `"encrypted_pii"`).
    #[pii(event_type = "PersonCaptured")]
    PersonCaptured {
        #[pii(plaintext)]  person_ref: String,      // kept in the clear; preserved after shredding
        #[pii(subject)]    subject_id: Uuid,        // data-subject UUID; used to look up the DEK
        #[pii(secret)]     name: String,            // encrypted; redacted as "[redacted]"
        #[pii(secret)]     email: String,           // encrypted; redacted as "[redacted]"
        #[pii(secret)]     phone: Option<String>,   // encrypted; redacted as null
    },

    #[pii(event_type = "PersonDetailsUpdated", sentinel = "encrypted_data")]
    PersonDetailsUpdated {
        #[pii(plaintext)]  person_ref: String,
        #[pii(subject)]    subject_id: Uuid,
        #[pii(secret)]     data: Value,             // encrypted; redacted as {}
    },
}
// Generates: pub struct MyEventPiiCodec;
//            impl PiiEventCodec for MyEventPiiCodec { ... }
```

> **One subject per variant.** Each PII variant names exactly one
> `#[pii(subject)]` field, so the macro emits a single encrypted partition
> (`label = "default"`) per event. An event that mixes PII from several subjects
> cannot be expressed this way — hand-write the codec instead (see
> [Manual implementation](#manual-implementation)).

#### Field roles

| Attribute | Role |
|-----------|------|
| `#[pii(subject)]` | The data-subject UUID. Exactly one per PII variant. Kept in the clear so the read path can locate the DEK without decrypting anything first. The field name is up to you (e.g. `subject_id`, `user_id`, `customer_ref`); whatever identifier you write becomes the JSON key in the stored payload. |
| `#[pii(plaintext)]` | A non-PII field. Kept in the clear on both write and read, including after shredding. |
| `#[pii(secret)]` | A PII field. Encrypted on write, decrypted or redacted on read. Every field on a PII variant must be annotated; unannotated fields are a compile error. |

#### Redaction defaults

The macro infers a redaction value from each `#[pii(secret)]` field's type:

| Type | Redacted as | Overridable? |
|------|-------------|--------------|
| `String` | `"[redacted]"` | no |
| `Option<T>` | `null` | no |
| `serde_json::Value` | `{}` | no |
| `Vec<T>` | `[]` | no |
| `chrono::NaiveDate` | `"0000-01-01"` (requires the `chrono` feature) | yes |
| Anything else | compile error — use `#[pii(secret, redact = "...")]` | n/a |

The override syntax accepts a string literal:

```rust
#[pii(secret, redact = "1900-01-01")] dob: chrono::NaiveDate,
```

It is only allowed on types whose default is not part of the crate's
contract — so `String`, `Option<_>`, `serde_json::Value`, and `Vec<_>` cannot
be overridden.

### 3. Wire it into your repository

```rust
use std::sync::Arc;
use cqrs_es_crypto::{
    CryptoShreddingEventRepository, FieldCipher, PostgresKeyStore, StaticKekProvider,
};

// Load the Key Encryption Key from the environment.
// Generate one with: openssl rand -base64 32
let kek = base64::engine::general_purpose::STANDARD
    .decode(std::env::var("APP_KEK")?.trim())?;

// Build a versioned KEK provider.  The "v1" label becomes the kek_id stored
// in the database — use it again when you rotate to identify legacy rows.
let provider = Arc::new(StaticKekProvider::single("v1", kek)?);

// Key store — wraps/unwraps DEKs via the provider.
let key_store = Arc::new(PostgresKeyStore::new(
    pool.clone(),
    Arc::clone(&provider),
));

// Repository — encrypts/decrypts event payloads with a stateless FieldCipher
// (no KEK needed here; only the per-subject DEK is used for field encryption).
let codec = Arc::new(MyEventPiiCodec);    // generated by #[derive(PiiCodec)]

let inner = postgres_es::PostgresEventRepository::new(pool);
let repo  = CryptoShreddingEventRepository::new(inner, key_store, FieldCipher::new(), codec);
```

### 4. Crypto-shred a subject

```rust
// Permanently destroys the DEK.  All encrypted events for this subject
// will be redacted on the next read.
key_store.delete_key(&subject_id).await?;
```

---

## Manual implementation

Hand-writing `PiiEventCodec` is the general case — reach for it whenever an event
is more than a single subject's fixed fields. The common reasons:

- **Multi-subject events.** One event carries PII for more than one data subject
  (e.g. several passengers' passport numbers captured in one submission). The
  codec returns **one `SecretPartition` per subject**, each encrypted under its
  own DEK, so crypto-shredding stays per-subject: deleting one subject's DEK
  redacts only their partition and leaves the others intact.
- **Schema- or data-driven classification.** Which fields are PII — and which
  subject they belong to — is decided at runtime rather than by fixed struct
  fields. For example a path-keyed `AttributesSet` event whose `changes` map is
  partitioned by a runtime `AttributeSchema`.
- **Non-standard payload shapes or custom redaction logic.**

The single-subject case the derive macro covers is just the degenerate version of
this: return one partition with `label = "default"`.

### The trait

```rust
pub trait PiiEventCodec: Send + Sync {
    /// Identify partitions to encrypt. Empty vec = pure-plaintext event.
    /// The codec also removes the PII fields from `event.payload` in the
    /// same pass.
    fn extract_partitions(&self, event: &mut SerializedEvent)
        -> Result<Vec<SecretPartition>, PiiCodecError>;

    /// Reattach decrypted partitions to the event payload by `label`.
    fn reconstruct(&self, event: &mut SerializedEvent,
        partitions: Vec<DecryptedPartition>) -> Result<(), PiiCodecError>;

    /// Write codec-defined sentinel values for partitions whose DEK was deleted.
    fn redact_partitions(&self, event: &mut SerializedEvent,
        labels: &[String]) -> Result<(), PiiCodecError>;

    /// Detect the pre-partition on-disk shape (default: None).
    /// Override this to read events written before this version of the crate.
    fn extract_encrypted_legacy(&self, _event: &SerializedEvent)
        -> Option<EncryptedPiiExtract> { None }
}
```

**Write path:**
- `extract_partitions(event)` — inspect an unencrypted event, extract cleartext
  bytes for each subject's PII slice AND remove those fields from the payload,
  then return one `SecretPartition` per subject. An empty `Vec` means the event
  is stored verbatim.

**Read path (new shape):**
- `reconstruct(event, partitions)` — given the decrypted partitions, write each
  one's cleartext back into the event under whatever fields the codec manages.
- `redact_partitions(event, labels)` — for each label whose DEK was deleted,
  write the codec-defined sentinel values (e.g. `"[redacted]"`, `null`, `{}`).

**Read path (legacy shape):**
- `extract_encrypted_legacy(event)` — default returns `None`; override to
  detect the pre-partition single-ciphertext shape that was written by an older
  version of this crate. The repository decrypts and passes the result to
  `reconstruct` (or `redact_partitions`) as a single partition with
  `label = "default"`.

The example below is **multi-subject** and **path-keyed**. A single
`AttributesSet` event carries one group of secret `changes` per person, already
grouped by subject. The codec encrypts each group into its own partition,
labelled by the person's slot (`person_ref`), so shredding one subject's DEK
redacts only their changes while everyone else's still decrypt.

```rust
use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::{DecryptedPartition, PiiCodecError, PiiEventCodec, SecretPartition};
use serde_json::{json, Value};
use uuid::Uuid;

// Payload shape this codec manages:
//
//   { "AttributesSet": {
//       "plaintext": { "search/origin": "LHR" },
//       "secret_partitions": [
//         { "person_ref": "passenger_0", "subject_id": "…",
//           "changes": { "persons/passenger_0/passport": "GB123…" } },
//         { "person_ref": "passenger_1", "subject_id": "…",
//           "changes": { "persons/passenger_1/passport": "FR456…" } }
//       ]
//   } }
//
// One partition per `secret_partitions` entry; the label equals `person_ref`.
pub struct AttributesSetCodec;

impl PiiEventCodec for AttributesSetCodec {
    fn extract_partitions(
        &self,
        event: &mut SerializedEvent,
    ) -> Result<Vec<SecretPartition>, PiiCodecError> {
        if event.event_type != "AttributesSet" {
            return Ok(vec![]);
        }
        let parts = &event.payload["AttributesSet"]["secret_partitions"];
        let n = parts.as_array().map_or(0, Vec::len);

        let mut partitions = Vec::with_capacity(n);
        for i in 0..n {
            let entry = &event.payload["AttributesSet"]["secret_partitions"][i];
            let Some(subject_id) = entry["subject_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let Some(person_ref) = entry["person_ref"].as_str().map(str::to_string) else {
                continue;
            };
            let changes = entry["changes"].clone();
            if changes == json!({}) { // nothing secret in this group
                continue;
            }

            // Replace the cleartext changes with `{}`; the bytes are about to be
            // encrypted into a partition labelled by `person_ref`.
            event.payload["AttributesSet"]["secret_partitions"][i]["changes"] = json!({});

            partitions.push(SecretPartition {
                subject_id,
                label: person_ref,
                payload: serde_json::to_vec(&changes)?,
            });
        }
        Ok(partitions)
    }

    fn reconstruct(
        &self,
        event: &mut SerializedEvent,
        partitions: Vec<DecryptedPartition>,
    ) -> Result<(), PiiCodecError> {
        if event.event_type != "AttributesSet" {
            return Ok(());
        }
        let n = event.payload["AttributesSet"]["secret_partitions"]
            .as_array()
            .map_or(0, Vec::len);
        for part in partitions {
            // The label routes the decrypted bytes back to the matching entry.
            for i in 0..n {
                let matches = event.payload["AttributesSet"]["secret_partitions"][i]["person_ref"]
                    .as_str()
                    == Some(part.label.as_str());
                if matches {
                    let changes: Value = serde_json::from_slice(&part.payload)?;
                    event.payload["AttributesSet"]["secret_partitions"][i]["changes"] = changes;
                    break;
                }
            }
        }
        Ok(())
    }

    fn redact_partitions(
        &self,
        event: &mut SerializedEvent,
        labels: &[String],
    ) -> Result<(), PiiCodecError> {
        if event.event_type != "AttributesSet" {
            return Ok(());
        }
        let n = event.payload["AttributesSet"]["secret_partitions"]
            .as_array()
            .map_or(0, Vec::len);
        for i in 0..n {
            let redacted = event.payload["AttributesSet"]["secret_partitions"][i]["person_ref"]
                .as_str()
                .is_some_and(|pr| labels.iter().any(|l| l == pr));
            if redacted {
                // Sentinel marking a shredded partition.
                event.payload["AttributesSet"]["secret_partitions"][i]["changes"] =
                    json!({ "redacted": true });
            }
        }
        Ok(())
    }
}
```

A single-subject event is just the degenerate case: return one `SecretPartition`
with `label = "default"` and match on that label in `reconstruct` /
`redact_partitions` — exactly what `#[derive(PiiCodec)]` generates for you.

> **Real-world reference.** A complete, tested implementation of this exact
> pattern — including the single-subject `PersonCaptured` /
> `PersonDetailsUpdated` variants and legacy back-compat — lives in this
> workspace as `JourneyPiiCodec`
> (`crates/journey_dynamics/src/pii_codec.rs`).

---

## How it works

### Key hierarchy

```
KEK (Key Encryption Key)       — one per deployment, loaded from the environment
 └── DEK (Data Encryption Key) — one per data subject, stored wrapped in Postgres
      └── event PII fields     — encrypted with AES-256-GCM per event
```

- **KEK** — A 256-bit key held only in application memory. Never stored. Used to
  wrap and unwrap DEKs via AES-256-KWP (RFC 5649).
- **DEK** — A fresh 256-bit key generated for each data subject. Stored
  wrapped in the `subject_encryption_keys` table. Deleting this row
  permanently destroys the ability to recover any PII for that subject —
  **this is crypto-shredding**.
- **AAD** — Every encrypted partition binds its ciphertext to its event position
  AND its subject and label using
  `"<aggregate_id>:<sequence>:<subject_id>:<label>"` as additional authenticated
  data. This prevents a partition from being transplanted across events, subjects,
  or labels within an event.

### Write path

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
              └── repository adds event.subjects = [p₀.subject_id, …]
```

### Read path

```
get_events / get_last_events / stream_events
  └── for each stored event
        ├── no encrypted_partitions → check extract_encrypted_legacy
        │     ├── None  → return verbatim (plain event)
        │     └── Some  → decrypt with legacy AAD → reconstruct / redact_partitions
        └── encrypted_partitions: [e₀, e₁ …]
              ├── for each eᵢ
              │     ├── KeyStore::get_key(eᵢ.subject_id)
              │     │     ├── Some(dek) → AES-256-GCM decrypt → DecryptedPartition
              │     │     └── None      → add eᵢ.label to redacted list
              ├── PiiEventCodec::reconstruct(event, decrypted_partitions)
              ├── PiiEventCodec::redact_partitions(event, redacted_labels)
              └── return updated event
```

---

## Multi-subject events

A single event may carry PII belonging to more than one data subject — for
example, a "set passport details for passengers A and B" form submission. To
support this without losing per-subject crypto-shredding, every encrypted event
is stored as a **list of subject-scoped partitions**.

```
event payload
 ├── plaintext fields …
 ├── subjects:             ["<uuid-A>", "<uuid-B>"]   (plaintext)
 └── encrypted_partitions: [
       { subject_id: "<uuid-A>", label: "passenger_0", nonce, ciphertext },
       { subject_id: "<uuid-B>", label: "passenger_1", nonce, ciphertext },
     ]
```

Each partition is encrypted independently. Shredding subject A's DEK redacts
A's partition only; B's partition decrypts normally in the same event.

`#[derive(PiiCodec)]` only emits the single-partition shape (`label = "default"`,
or none). Multi-subject events are written by hand-written codecs that return one
`SecretPartition` per subject from `extract_partitions` — see the worked example
under [Manual implementation](#manual-implementation).

### Per-partition AAD

Each partition's AAD is `aggregate_id || sequence || subject_id || label`,
making partitions non-fungible across events, subjects within an event, and
labels within an event.

### Backward compatibility

Events written before this version carry a single inline ciphertext field
(e.g. `encrypted_pii`) with legacy AAD `"aggregate_id:sequence"`. The read path
detects this shape via `extract_encrypted_legacy` and translates it to a
one-partition vector with `label = "default"` before calling `reconstruct`.
No on-disk migration is required.

---

## Required database schema

The `PostgresKeyStore` requires a `subject_encryption_keys` table:

```sql
CREATE TABLE subject_encryption_keys
(
    key_id       UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id   UUID      NOT NULL UNIQUE,
    wrapped_key  BYTEA     NOT NULL,
    kek_id       TEXT      NOT NULL,
    rewrapped_at TIMESTAMP,
    created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_subject_keys_subject_id ON subject_encryption_keys (subject_id);
CREATE INDEX idx_subject_keys_kek_id     ON subject_encryption_keys (kek_id);
```

The `kek_id` column records which KEK version wrapped each row — this is what
enables zero-downtime KEK rotation. See `docs/KEK_ROTATION_RUNBOOK.md`.

---

## Testing

Enable the `testing` feature to use `InMemoryKeyStore` and
`InMemoryEventRepository` without a database:

```toml
[dev-dependencies]
cqrs-es-crypto = { version = "...", features = ["testing"] }

# Also enable "derive" if you want to use #[derive(PiiCodec)] in tests:
cqrs-es-crypto = { version = "...", features = ["testing", "derive"] }
```

```rust
use std::sync::Arc;
use cqrs_es_crypto::{
    CryptoShreddingEventRepository, FieldCipher, InMemoryEventRepository,
    InMemoryKeyStore, KeyStore,
};

fn make_test_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
    let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
    let codec = Arc::new(MyEventPiiCodec);
    CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        FieldCipher::new(),
        codec,
    )
}
```

---

## Cargo features

| Feature | Default | Description |
|---------|---------|-------------|
| `derive` | | Enables `#[derive(PiiCodec)]` via the `cqrs-es-crypto-derive` proc-macro crate |
| `chrono` | | Implies `derive`; teaches the derive macro to redact `chrono::NaiveDate` secret fields. Default sentinel is `"0000-01-01"`; override per-field with `#[pii(secret, redact = "...")]` |
| `testing` | | Exposes `InMemoryEventRepository` for use in tests |

---

## Crate structure

| Module | Contents |
|--------|----------|
| `cipher` | `FieldCipher` — stateless AES-256-GCM field encryption; `PiiCipher` (deprecated) |
| `kek` | `KekProvider` trait, `StaticKekProvider`, `KekHandle`, `WrappedDek` |
| `key_store` | `KeyStore` trait, `PostgresKeyStore`, `PostgresKeyStoreOptions`, `InMemoryKeyStore` |
| `rewrap` | `RewrapWorker` — background and one-shot DEK re-wrap sweeper |
| `repository` | `PiiEventCodec` trait, `CryptoShreddingEventRepository`, `InMemoryEventRepository` |

---

## Security notes

- KEK bytes must be exactly 32 bytes. Load them from a secrets manager or
  environment variable — never hardcode or commit them to source control.
- `StaticKekProvider` uses [`zeroize`](https://docs.rs/zeroize) to erase KEK
  bytes from memory when dropped. `KeyMaterial` (DEK) is also zeroized on drop.
- Each `FieldCipher::encrypt` call generates a fresh random 96-bit nonce; nonce
  reuse under normal operation is not possible.
- Per-partition AAD (`"<aggregate_id>:<sequence>:<subject_id>:<label>"`) makes
  each partition non-fungible across events, subjects within an event, and labels
  within an event. Legacy AAD (`"<aggregate_id>:<sequence>"`) is accepted on read
  for back-compat but never produced on write.
- Partitions are encrypted independently; one subject's compromised DEK does not
  expose any other partition in the same event.
- Crypto-shredding remains per-subject: deleting a DEK redacts every partition
  encrypted under it, leaving other subjects' partitions in the same event intact.
- `PostgresKeyStore::get_or_create_key` uses `INSERT … ON CONFLICT DO NOTHING`
  to handle concurrent DEK-creation races safely.
- `PostgresKeyStore::rewrap_key` uses a compare-and-swap `UPDATE … WHERE kek_id = $old`
  so concurrent re-wraps and the background sweeper cannot regress a row.
- **Never remove a `JOURNEY_KEK_<id>` variable while rows in
  `subject_encryption_keys` still reference that `kek_id`** — those DEKs would
  become permanently unreadable. The application panics at boot if this condition
  is detected. See `docs/KEK_ROTATION_RUNBOOK.md` for the safe rotation procedure.
