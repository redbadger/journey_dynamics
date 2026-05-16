# Split `journey_person`: Core Lookup vs Read-Model Projection

**Status:** Plan — ready for implementation
**Audience:** Implementing agent

---

## 1. Problem

The `journey_person` table serves two fundamentally different roles:

1. **Core operational lookup** — `email → subject_id` mapping used by
   `DELETE /subjects/by-email` to resolve GDPR erasure requests. The system
   _cannot shred by email_ without this mapping, and email is encrypted in
   the event store so the mapping cannot be derived from events alone.

2. **Read-model projection** — person data (name, email, phone, details)
   displayed by `GET /journeys/{id}`. Rebuildable from events while DEKs
   exist.

Today both roles live in the same table, populated by `Query::dispatch`
**after** the event-store transaction has already committed. The `dispatch`
implementation even swallows errors with `eprintln!`. A crash or error
between event-persist and projection-update silently loses the
`email → subject_id` mapping — making future GDPR erasure by email
impossible for that subject.

Additionally, DEK creation in `subject_encryption_keys` also happens
outside the event-persist transaction (inside `encrypt_events` via
`KeyStore::get_or_create_key`). A crash between DEK creation and event
persist leaves an orphan DEK row — harmless but wasteful.

## 2. Goal

Split the table so that:

| Concern | Table | Written | Consistency |
|---------|-------|---------|-------------|
| `email → subject_id` lookup | **`subject_lookup`** (new) | Inside the same DB transaction as event INSERT and DEK creation | **Atomic with event persist** |
| DEK (subject encryption key) | **`subject_encryption_keys`** (existing) | Same transaction | **Atomic with event persist** |
| Person read-model (name, email, phone, details, forgotten) | **`journey_person`** (existing, unchanged) | Via `Query::dispatch` after commit | Eventual (acceptable) |

## 3. Architectural approach

### Why we can't just "put it in the same transaction" using upstream crates

The transaction that persists events is managed inside
`PostgresEventRepository::insert_events` (`postgres-es` crate, upstream).
Its `pool` field is private; `insert_events` and `persist_events` are
`pub(crate)`. There is no hook, callback, or extension point to inject
additional writes into that transaction from outside the crate.

### What we do instead

Add a **transactional write path** directly to
`CryptoShreddingEventRepository` (our crate, `cqrs-es-crypto`). When
configured with a `Pool<Postgres>` and optional persist hooks, it manages
the entire write — DEK creation, event encryption, event insertion, and
hook writes — in a single Postgres transaction. Reads continue to
delegate to the inner `PostgresEventRepository` as before.

```
BEFORE (current)                          AFTER

CryptoShreddingEventRepository            CryptoShreddingEventRepository
  │                                         │
  ├─ encrypt_events():                      ├─ persist() [transactional path]:
  │    key_store.get_or_create_key()        │    BEGIN
  │      → auto-commit to                   │    SELECT/INSERT subject_encryption_keys
  │        subject_encryption_keys          │    encrypt events (in-memory AES-GCM)
  │    cipher.encrypt()                     │    INSERT INTO events
  │                                         │    hooks.on_persist() → e.g. INSERT subject_lookup
  ├─ persist():                             │    COMMIT
  │    inner.persist()                      │
  │      → PostgresEventRepository          ├─ get_events() / stream_events() / etc.:
  │        → BEGIN                          │    inner.get_events() [unchanged — still
  │          INSERT INTO events             │    delegates to PostgresEventRepository]
  │          COMMIT                         │
  │                                         └─ persist() [legacy path, pool = None]:
  └─ (then CqrsFramework dispatches            encrypt_events() + inner.persist()
      to Query::dispatch, which writes          [unchanged — for InMemoryEventRepository]
      journey_person outside any tx)
```

`PostgresEventRepository` remains in the dependency tree (reads still flow
through it), but its `persist` method is **never called** when the
transactional path is active — the write path is entirely owned by
`CryptoShreddingEventRepository`.

### Why this is the right place

`CryptoShreddingEventRepository` already holds everything needed:

- `cipher: Arc<PiiCipher>` — `generate_dek()` and `wrap_dek()` are
  in-memory operations; `unwrap_dek()` for the fast-path read is also
  available. No need to go through the `KeyStore` trait for the
  transactional path.
- `codec: Arc<dyn PiiEventCodec>` — `classify()` identifies which events
  carry PII and returns the
 `subject_id` + plaintext fields.
- It lives in `cqrs-es-crypto`, the same crate as `PostgresKeyStore`, so it
  already knows the `subject_encryption_keys` table schema.
- The `key_store` field is still used on the **read** path (`get_key` for
  decryption) and for the legacy non-transactional write path.

### What stays domain-agnostic

The `PersistHook` trait receives `&[SerializedEvent]` (the unencrypted
events) and a `&mut Transaction<Postgres>`. The hook is implemented in
the application crate (`journey_dynamics`) and carries the domain knowledge
of which event types map to which lookup rows. The `cqrs-es-crypto` crate
knows nothing about `PersonCaptured` or `subject_lookup`.

---

## 4. Detailed changes

### 4.1 New migration: `subject_lookup` table

```sql
-- Core operational table: maps subject_id → normalised email.
-- Written transactionally with the event store.
-- Rows are DELETED when the subject is crypto-shredded (email is PII).
CREATE TABLE subject_lookup (
    subject_id  UUID      NOT NULL PRIMARY KEY,
    email_lower TEXT      NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_subject_lookup_email
    ON subject_lookup (email_lower);

-- Backfill from the existing projection so that deployments with
-- existing data don't lose the ability to shred by email.
INSERT INTO subject_lookup (subject_id, email_lower)
SELECT DISTINCT ON (subject_id) subject_id, lower(email)
FROM   journey_person
WHERE  NOT forgotten AND email IS NOT NULL
ON CONFLICT (subject_id) DO NOTHING;
```

Down migration:

```sql
DROP TABLE IF EXISTS subject_lookup CASCADE;
```

**Design notes:**

- One row per `subject_id` (primary key). Multiple subject_ids can share
  the same `email_lower` (e.g. random-UUID-per-slot strategy).
- No `forgotten` flag — on shredding, the row is **deleted** (email is PII;
  keeping it would undermine crypto-shredding). Idempotency of the shred
  flow is unaffected because DEK deletion is already idempotent.
- Future lookup dimensions (phone, passport) can be added as additional
  columns, or the table can be generalised to
  `(subject_id, lookup_type, lookup_value)` later. Keep it simple for now.

### 4.2 `cqrs-es-crypto`: new `PersistHook` trait

Add to `crates/cqrs-es-crypto/src/repository.rs` (and re-export from
`lib.rs`):

```rust
/// Hook called within the transactional persist path.
///
/// Receives the **unencrypted** serialised events and a live Postgres
/// transaction. Implementations can inspect event payloads and perform
/// domain-specific writes (e.g. subject-lookup inserts) that will be
/// committed atomically with the event and DEK inserts.
///
/// If the hook returns an error, the entire transaction is rolled back.
#[async_trait]
pub trait PersistHook: Send + Sync {
    async fn on_persist(
        &self,
        events: &[SerializedEvent],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), PersistenceError>;
}
```

### 4.3 `cqrs-es-crypto`: extend `CryptoShreddingEventRepository`

Add two optional fields and builder methods:

```rust
pub struct CryptoShreddingEventRepository<R: PersistedEventRepository> {
    pub(crate) inner: R,
    key_store: Arc<dyn KeyStore>,
    cipher: Arc<PiiCipher>,
    codec: Arc<dyn PiiEventCodec>,
    /// When set, `persist` uses a single Postgres transaction for DEK
    /// creation, event insertion, and hook writes. When `None`, falls back
    /// to the legacy path (encrypt → inner.persist).
    pool: Option<sqlx::Pool<sqlx::Postgres>>,
    /// Hooks called within the transactional persist. Ignored when `pool`
    /// is `None`.
    persist_hooks: Vec<Arc<dyn PersistHook>>,
}

impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    // Existing new() unchanged — pool defaults to None, hooks to empty vec.

    /// Enable the transactional write path.
    ///
    /// When set, `persist` will manage its own Postgres transaction that
    /// atomically commits DEKs, encrypted events, and any persist-hook
    /// writes. The inner repository's `persist` is bypassed for writes
    /// (reads still delegate through it).
    #[must_use]
    pub fn with_transactional_writes(
        mut self,
        pool: sqlx::Pool<sqlx::Postgres>,
    ) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Register a hook that participates in the persist transaction.
    ///
    /// Hooks receive the **unencrypted** events and a `&mut Transaction`
    /// and can perform additional writes (e.g. subject-lookup upserts).
    /// Multiple hooks are called in registration order.
    #[must_use]
    pub fn with_persist_hook(mut self, hook: Arc<dyn PersistHook>) -> Self {
        self.persist_hooks.push(hook);
        self
    }
}
```

### 4.4 `cqrs-es-crypto`: transactional `persist` implementation

Replace the `persist` method in the `PersistedEventRepository` impl:

```rust
async fn persist<A: Aggregate>(
    &self,
    events: &[SerializedEvent],
    snapshot_update: Option<(String, Value, usize)>,
) -> Result<(), PersistenceError> {
    let Some(pool) = &self.pool else {
        // Legacy path: encrypt then delegate to inner (e.g. InMemoryEventRepository).
        let encrypted = self.encrypt_events(events).await?;
        return self.inner.persist::<A>(&encrypted, snapshot_update).await;
    };

    // ── Transactional path ────────────────────────────────────────────

    let mut tx = pool.begin().await
        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

    // 1. Encrypt events, creating DEKs inside the transaction.
    let encrypted = self.encrypt_events_in_tx(events, &mut tx).await?;

    // 2. Insert encrypted events.
    for event in &encrypted {
        let payload = serde_json::to_value(&event.payload)?;
        let metadata = serde_json::to_value(&event.metadata)?;
        sqlx::query(
            "INSERT INTO events \
             (aggregate_type, aggregate_id, sequence, \
              event_type, event_version, payload, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)"
        )
        .bind(&event.aggregate_type)
        .bind(&event.aggregate_id)
        .bind(event.sequence as i64)
        .bind(&event.event_type)
        .bind(&event.event_version)
        .bind(&payload)
        .bind(&metadata)
        .execute(&mut *tx)
        .await
        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
    }

    // 3. Handle snapshot (currently always None for event-sourced stores).
    if let Some((aggregate_id, aggregate, current_snapshot)) = snapshot_update {
        sqlx::query(
            "INSERT INTO snapshots \
             (aggregate_type, aggregate_id, last_sequence, \
              current_snapshot, payload) \
             VALUES ($1, $2, 0, $3, $4) \
             ON CONFLICT (aggregate_type, aggregate_id) DO UPDATE \
             SET last_sequence = EXCLUDED.last_sequence, \
                 current_snapshot = $3, payload = $4"
        )
        .bind(A::TYPE)
        .bind(&aggregate_id)
        .bind(current_snapshot as i64)
        .bind(&aggregate)
        .execute(&mut *tx)
        .await
        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
    }

    // 4. Call hooks with the UNENCRYPTED events inside the transaction.
    for hook in &self.persist_hooks {
        hook.on_persist(events, &mut tx).await?;
    }

    // 5. Commit — DEKs, events, and hook writes are now atomically visible.
    tx.commit().await
        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

    Ok(())
}
```

### 4.5 `cqrs-es-crypto`: new `encrypt_events_in_tx` method

This is the key new method. It mirrors `encrypt_events` but creates/fetches
DEKs within the transaction instead of using `self.key_store`:

```rust
impl<R: PersistedEventRepository> CryptoShreddingEventRepository<R> {
    /// Like `encrypt_events`, but creates DEKs within the provided
    /// transaction so that key creation is atomic with event persistence.
    async fn encrypt_events_in_tx(
        &self,
        events: &[SerializedEvent],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let mut event = event.clone();
            if let Some(pii) = self.codec.classify(&event) {
                let dek = self.get_or_create_key_in_tx(
                    &pii.subject_id, tx
                ).await?;

                let aad = format!("{}:{}", event.aggregate_id, event.sequence)
                    .into_bytes();
                let plaintext = serde_json::to_vec(&pii.plaintext_pii)?;
                let encrypted = self.cipher.encrypt(&dek, &plaintext, &aad);

                let sentinel = EncryptedPiiSentinel {
                    ciphertext_b64: BASE64.encode(&encrypted.ciphertext),
                    nonce_b64: BASE64.encode(&encrypted.nonce),
                };

                event.payload = (pii.build_encrypted_payload)(sentinel);
            }
            out.push(event);
        }
        Ok(out)
    }

    /// Get or create a DEK within a transaction.
    ///
    /// Mirrors `PostgresKeyStore::get_or_create_key` but uses the provided
    /// transaction executor instead of an independent pool connection, so
    /// the DEK INSERT is atomic with the caller's transaction.
    async fn get_or_create_key_in_tx(
        &self,
        subject_id: &Uuid,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<KeyMaterial, PersistenceError> {
        use sqlx::Row;

        // Fast path: DEK already exists (may be from a prior committed tx
        // or from an earlier event in the same batch).
        let existing = sqlx::query(
            "SELECT key_id, wrapped_key \
             FROM subject_encryption_keys WHERE subject_id = $1"
        )
        .bind(subject_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

        if let Some(row) = existing {
            let key_id: Uuid = row.get("key_id");
            let wrapped_key: Vec<u8> = row.get("wrapped_key");
            let material = self.cipher.unwrap_dek(key_id, &wrapped_key)
                .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
            return Ok(material);
        }

        // Generate, wrap, and insert within the transaction.
        let dek = PiiCipher::generate_dek();
        let wrapped_key = self.cipher.wrap_dek(&dek);

        let result = sqlx::query(
            "INSERT INTO subject_encryption_keys \
             (key_id, subject_id, wrapped_key) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (subject_id) DO NOTHING"
        )
        .bind(dek.key_id)
        .bind(subject_id)
        .bind(&wrapped_key)
        .execute(&mut **tx)
        .await
        .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

        if result.rows_affected() == 0 {
            // Concurrent insert won the race — re-read from the tx.
            let row = sqlx::query(
                "SELECT key_id, wrapped_key \
                 FROM subject_encryption_keys WHERE subject_id = $1"
            )
            .bind(subject_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

            let key_id: Uuid = row.get("key_id");
            let wrapped_key: Vec<u8> = row.get("wrapped_key");
            let material = self.cipher.unwrap_dek(key_id, &wrapped_key)
                .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
            Ok(material)
        } else {
            Ok(dek)
        }
    }
}
```

**Note:** `get_or_create_key_in_tx` mirrors `PostgresKeyStore::get_or_create_key`
exactly, but executes against `&mut **tx` instead of `&self.pool`. The
`PiiCipher` is used directly for `generate_dek`, `wrap_dek`, and
`unwrap_dek` — no `KeyStore` trait involved on the transactional path.

### 4.6 `journey_dynamics`: implement `PersistHook`

New file `crates/journey_dynamics/src/subject_lookup_hook.rs`:

```rust
use async_trait::async_trait;
use cqrs_es::persist::{PersistenceError, SerializedEvent};
use cqrs_es_crypto::PersistHook;
use uuid::Uuid;

/// Writes `(subject_id, email_lower)` rows to `subject_lookup` for every
/// `PersonCaptured` event, within the persist transaction.
pub struct SubjectLookupHook;

#[async_trait]
impl PersistHook for SubjectLookupHook {
    async fn on_persist(
        &self,
        events: &[SerializedEvent],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), PersistenceError> {
        for event in events {
            if event.event_type != "PersonCaptured" {
                continue;
            }
            let Some(inner) = event.payload.get("PersonCaptured") else {
                continue;
            };
            let Some(subject_id) = inner
                .get("subject_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Uuid>().ok())
            else {
                continue;
            };
            let Some(email) = inner.get("email").and_then(|v| v.as_str()) else {
                continue;
            };

            sqlx::query(
                "INSERT INTO subject_lookup (subject_id, email_lower) \
                 VALUES ($1, lower($2)) \
                 ON CONFLICT (subject_id) DO UPDATE SET email_lower = lower($2)"
            )
            .bind(subject_id)
            .bind(email)
            .execute(&mut **tx)
            .await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
        }
        Ok(())
    }
}
```

### 4.7 Update `config.rs`: wire in pool + hook

```rust
// BEFORE
let inner = PostgresEventRepository::new(pool);
let codec = Arc::new(JourneyPiiCodec);
let crypto_repo = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec);
let store = PersistedEventStore::new_event_store(crypto_repo);

// AFTER
let inner = PostgresEventRepository::new(pool.clone());
let codec = Arc::new(JourneyPiiCodec);
let crypto_repo = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec)
    .with_transactional_writes(pool)
    .with_persist_hook(Arc::new(SubjectLookupHook));
let store = PersistedEventStore::new_event_store(crypto_repo);
```

**The `CryptoCqrs` type alias does not change.** The outer type is still
`CryptoShreddingEventRepository<PostgresEventRepository>`.

### 4.8 Update `find_subjects_by_email` to use `subject_lookup`

In `view_repository.rs`:

```rust
// BEFORE
pub async fn find_subjects_by_email(&self, email: &str) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        r"
        SELECT DISTINCT subject_id
        FROM journey_person
        WHERE lower(email) = lower($1)
          AND NOT forgotten
        ",
    )
    .bind(email)
    .fetch_all(&self.pool)
    .await?;
    // ...
}

// AFTER
pub async fn find_subjects_by_email(&self, email: &str) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows = sqlx::query(
        r"
        SELECT subject_id
        FROM subject_lookup
        WHERE email_lower = lower($1)
        ",
    )
    .bind(email)
    .fetch_all(&self.pool)
    .await?;
    // ...
}
```

No `AND NOT forgotten` filter needed — rows are deleted from
`subject_lookup` on shredding, so only active subjects are present.

### 4.9 Update shredding flow: delete from `subject_lookup`

In `route_handler.rs`, in the `shred_subject` function, after deleting the
DEK, also delete the subject from the lookup table:

```rust
// After: state.key_store.delete_key(&subject_id).await
// Add:
sqlx::query("DELETE FROM subject_lookup WHERE subject_id = $1")
    .bind(subject_id)
    .execute(&*state.pool)  // need pool on ApplicationState
    .await
    .ok(); // best-effort — DEK is already gone, which is the real shredding
```

This requires adding `pool: Pool<Postgres>` to `ApplicationState` (it's
already used elsewhere via `journey_query`, but exposing it directly is
cleaner for this write).

Alternatively, add a `delete_subject_lookup` method to
`StructuredJourneyViewRepository` or to a new `SubjectLookupRepository`.

### 4.10 `journey_person` becomes a pure projection

No schema change to `journey_person` — it continues to work exactly as it
does today. Its email column is still populated by the projection and still
nulled by `SubjectForgotten`. The only change is that nothing reads
`journey_person.email` for operational purposes any more — it is purely for
the read API.

Optionally, drop the `find_by_email` method (which queries
`journey_person.email`) or rewrite it as:
`subject_lookup → subject_ids → find_journeys_by_subject (events table)`.
This is a separate concern and can be done later.

---

## 5. Transaction scope — what's atomic now

With the transactional path, a single `persist` call produces **one**
Postgres transaction containing:

| Write | Table | Notes |
|-------|-------|-------|
| DEK creation | `subject_encryption_keys` | Only for new subjects; existing DEKs are read from the tx |
| Encrypted events | `events` | Same INSERT SQL as `PostgresEventRepository` |
| Subject lookup | `subject_lookup` | Via `SubjectLookupHook` |

If any step fails, the entire transaction rolls back. No orphan DEKs, no
orphan lookups, no events without lookups.

**The read path is unchanged:** `get_events`, `get_last_events`,
`stream_events` etc. still delegate to the inner
`CryptoShreddingEventRepository → PostgresEventRepository` chain and use
`self.key_store` for DEK retrieval on decryption. This is correct because
DEKs created in a committed transaction are visible to subsequent reads
via the pool.

## 6. Legacy (non-transactional) path

When `pool` is `None` (e.g. in tests using `InMemoryEventRepository` +
`InMemoryKeyStore`), `persist` falls through to the existing
`encrypt_events` → `inner.persist` path. No behavioural change for tests.

## 7. Testing strategy

### Unit tests (`cqrs-es-crypto`)

- **`encrypt_events_in_tx`**: use a test database. Begin a transaction,
  call `encrypt_events_in_tx`, verify the DEK was inserted within the
  transaction (visible inside the tx but not outside until commit). Commit,
  verify it's now globally visible.
- **`persist` transactional path**: persist a batch of events including a
  PII event. Verify that both `events` and `subject_encryption_keys`
  contain the expected rows. Then verify that a failed persist (e.g.
  duplicate sequence) rolls back both the event and the DEK.
- **`persist` with hooks**: register a mock hook, persist events, verify
  the hook received the unencrypted events and its writes are committed.

### Unit tests (`journey_dynamics`)

- **`SubjectLookupHook`**: feed it various `SerializedEvent` payloads
  (PersonCaptured, Modified, PersonDetailsUpdated, SubjectForgotten) and
  assert it only writes lookups for PersonCaptured.
- **End-to-end persist**: persist a PersonCaptured via the full CQRS stack,
  verify `events`, `subject_encryption_keys`, and `subject_lookup` all
  contain the expected rows.

### Integration / Hurl tests

- **Existing `full-flight-booking_with_shredding_by_email.hurl`**: should
  pass unchanged — the behaviour is identical, just the underlying table
  is different.
- **New test**: capture a person, verify `subject_lookup` contains the
  mapping, then shred by email and verify the lookup row is gone.

### Regression

- All existing tests should pass. `journey_person` is still populated by
  the projection and still serves the read API. The only query that changes
  is `find_subjects_by_email`.

## 8. Migration / deployment order

1. **Deploy the migration** — creates `subject_lookup`, backfills from
   `journey_person`. Zero downtime; additive only.
2. **Deploy the code** — `find_subjects_by_email` switches to
   `subject_lookup`; new events write to both tables (projection writes
   `journey_person`, transactional write writes `subject_lookup`).
3. Optionally: remove the `email` index from `journey_person` if it was
   only serving the lookup query.

No breaking changes; fully backwards-compatible.

## 9. Files to change

| File | Change |
|---|---|
| `migrations/YYYYMMDDHHMMSS_subject_lookup.up.sql` | New migration |
| `migrations/YYYYMMDDHHMMSS_subject_lookup.down.sql` | Down migration |
| `crates/cqrs-es-crypto/src/repository.rs` | Add `PersistHook` trait; add `pool`, `persist_hooks` fields + builder methods to `CryptoShreddingEventRepository`; add `encrypt_events_in_tx` + `get_or_create_key_in_tx`; update `persist` with transactional path |
| `crates/cqrs-es-crypto/src/lib.rs` | Re-export `PersistHook` |
| `crates/journey_dynamics/src/subject_lookup_hook.rs` | **New file** — `SubjectLookupHook` implementing `PersistHook` |
| `crates/journey_dynamics/src/config.rs` | Wire `.with_transactional_writes(pool).with_persist_hook(...)` |
| `crates/journey_dynamics/src/state.rs` | Expose `pool` on `ApplicationState` |
| `crates/journey_dynamics/src/view_repository.rs` | `find_subjects_by_email` → query `subject_lookup`; add `delete_subject_lookup` |
| `crates/journey_dynamics/src/route_handler.rs` | After DEK deletion, delete from `subject_lookup` |
| `crates/journey_dynamics/src/lib.rs` | Add `mod subject_lookup_hook;` |
| Tests (Rust + Hurl) | Update/add as described in §7 |

**Note:** The `CryptoCqrs` type alias in `config.rs` does **not** change.

## 10. Sequence column type note

`postgres-es` casts `event.sequence` as `i32` in its INSERT. Our events
table defines `sequence` as `BIGINT`. The transactional write path should
bind as `i64` to match the column type. This is a minor improvement over
the upstream crate's narrower cast.

## 11. Future considerations

- **Generalised lookup table**: if more lookup dimensions are needed
  (phone, passport), consider `(subject_id, lookup_type, lookup_value)`
  with a composite primary key. The `PersistHook` trait already supports
  this — the hook can write multiple lookup rows per event.
- **HMAC-hashed lookups**: for defence-in-depth, store
  `HMAC-SHA256(secret, email_lower)` instead of `email_lower`. Requires
  a stable HMAC key (distinct from the KEK) and caller-side hash
  computation. Adds complexity; defer unless the threat model warrants it.
- **`find_by_email` (journey lookup)**: currently queries
  `journey_person.email`. Can be rewritten as
  `subject_lookup → find_journeys_by_subject` for consistency, or left
  as a projection query. Low priority.
- **Additional hooks**: the `PersistHook` mechanism is general-purpose.
  Future hooks could publish events to a message bus, update other
  projections transactionally, etc.
