# Blind indexing (`cqrs-es-crypto`, feature `blind-index`)

Keep a **crypto-shreddable** (encrypted) PII field **queryable** by storing a
deterministic keyed HMAC of it as an opaque lookup key. The value is erased when
the subject's DEK is deleted; the index is a separate artifact in a lookup
table, removed on shred via a foreign key.

See the module docs (`crates/cqrs-es-crypto/src/blind_index.rs`) for the API and
threat model. This document covers what a **consumer** must set up, plus the
backfill/rotation procedures that are intentionally not yet coded.

## Threat model in one line

Global lookup forces a **global HMAC key, distinct from any KEK**. The index is
therefore a *pseudonym*, not itself crypto-shredded — erasure is the row
deletion. Low-entropy inputs (work emails) mean the key must be protected: use
`GcpKmsBlindIndexKeyProvider` (KMS `macSign`, key never leaves KMS) in
production; `StaticBlindIndexKeyProvider` (env key) is dev/test only.

## Required consumer setup

### 1. Lookup table + the load-bearing cascade FK

The `ON DELETE CASCADE` FK to the key-store table is what erases the index
atomically on **every** shred path (`delete_key`, `delete_key_in_tx`, raw SQL) —
it is the primary erasure mechanism, not a nicety.

```sql
CREATE TABLE subject_lookup (
    subject_id   UUID  NOT NULL,
    lookup_type  TEXT  NOT NULL,
    lookup_value BYTEA NOT NULL,          -- raw 32-byte HMAC-SHA256
    PRIMARY KEY (subject_id, lookup_type),
    FOREIGN KEY (subject_id)
        REFERENCES subject_encryption_keys(subject_id) ON DELETE CASCADE
);
CREATE INDEX subject_lookup_value_idx ON subject_lookup (lookup_type, lookup_value);
-- Optional: enforce one subject per value (raises 23505 on a duplicate, which
-- BlindIndexHook surfaces as a hard error, not an optimistic-lock retry):
-- CREATE UNIQUE INDEX subject_lookup_unique ON subject_lookup (lookup_type, lookup_value);
```

Row-insert ordering is safe: encryption creates the DEK row before the hook runs
within the same persist transaction, so the FK parent always exists first.

### 2. Wiring

```rust
let keys = Arc::new(GcpKmsBlindIndexKeyProvider::new(env::var("JOURNEY_BLIND_INDEX_KMS_KEY")?));
let hook = BlindIndexHook::new(keys, vec![
    Box::new(AttributesSetSource {
        event_type: "AttributesSet".into(),
        mappings: vec![("/employment/workEmail".into(), "work_email".into())],
    }),
]);
let repo = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec)
    .with_transactional_writes(pool.clone(), kek_provider)
    .with_persist_hook(Arc::new(hook));   // hook without this errors loudly
```

The MAC key resource **must be distinct** from any KEK resource. Registering a
hook without `with_transactional_writes` is a hard error at persist time (never
a silent no-op).

### 3. Resolution

```rust
let digest = keys.compute("work_email", incoming_sso_email).await?;
// SELECT subject_id FROM subject_lookup WHERE lookup_type = 'work_email' AND lookup_value = $digest
```

## Backfill / rebuild (design — implement when adopting)

The lookup table is **write-path-only** state: replay does not re-run
`PersistHook`s. So two situations need an explicit rebuild, both the same
procedure:

- **Adoption on existing events** (a stream captured before the field was
  indexed). *(N/A for a greenfield field.)*
- **Key rotation / compromise** — rotating the MAC key changes every digest, so
  incoming lookups stop matching stored rows.

Procedure (per subject, while its DEK still exists):

1. Stream the subject's events through the crypto **reader**
   (`CryptoShreddingEventRepository::get_events`) — secrets come back decrypted.
2. Re-run the configured `BlindIndexSource`s on the decrypted values.
3. Upsert into the lookup table (idempotent).

Notes: shredded subjects have no DEK, so their events decrypt to the redaction
sentinel and simply produce no rows — correct (they should have none). Rotation
therefore implies a brief degraded-lookup window and a full re-index; there is
**no online dual-key probing** and no `key_version` column in v1 (a column
implying a rotation capability that doesn't exist is worse than none).

## What lives where

- `cqrs-es-crypto` (feature `blind-index`): primitive, key provider trait +
  static/KMS impls, extraction sources, `BlindIndexHook`. Zero cost with the
  feature off.
- Consumer app: the table DDL + FK, the key resource, hook wiring, resolution
  queries, and (when needed) the backfill worker above.

## Relationship to `subject_lookup_hook`

`SubjectLookupHook` (plaintext `email_lower`) is `#[deprecated]` in favour of
`BlindIndexHook`. It still works; apps migrate on their own schedule. Migrating
requires re-sourcing any plaintext-email reads (e.g. `view_repository`) from
decrypted events, since the HMAC is one-way.
