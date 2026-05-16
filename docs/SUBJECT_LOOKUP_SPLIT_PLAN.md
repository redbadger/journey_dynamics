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

## 2. Goal

Split the table so that:

| Concern | Table | Written | Consistency |
|---------|-------|---------|-------------|
| `email → subject_id` lookup | **`subject_lookup`** (new) | Inside the same DB transaction as event INSERT | **Atomic with event persist** |
| Person read-model (name, email, phone, details, forgotten) | **`journey_person`** (existing, unchanged) | Via `Query::dispatch` after commit | Eventual (acceptable) |

## 3. Architectural approach

### Why we can't just "put it in the same transaction"

The transaction that persists events is managed inside
`PostgresEventRepository::insert_events` (`postgres-es` crate, upstream).
Its `pool` field is private; `insert_events` and `persist_events` are
`pub(crate)`. There is no hook, callback, or extension point to inject
additional writes into that transaction from outside the crate.

### What we do instead

Create a **`TransactionalEventRepository`** in the `journey_dynamics`
application crate that:

- **Wraps** `CryptoShreddingEventRepository<PostgresEventRepository>` (used
  for reads and for its `encrypt_events` method).
- **Holds its own `Pool<Postgres>`** and manages its own write-path
  transaction.
- **On `persist`**: encrypts events (via the wrapped crypto repo), opens a
  transaction, INSERTs encrypted events _and_ subject-lookup rows, commits.
- **On reads**: delegates entirely to the wrapped crypto repo (which
  delegates to `PostgresEventRepository` for raw reads, then decrypts).

The result is a single Postgres transaction containing both event inserts
and lookup inserts — full atomicity without forking either upstream crate.

```
BEFORE                                 AFTER

PersistedEventStore                    PersistedEventStore
  └─ CryptoShreddingEventRepository     └─ TransactionalEventRepository (NEW)
       └─ PostgresEventRepository              ├─ Pool<Postgres>       ← owns the write tx
                                               └─ CryptoShreddingEventRepository
                                                    └─ PostgresEventRepository ← reads only
```

`PostgresEventRepository` remains in the dependency tree (reads still flow
through it), but its `persist` method is **never called** — the write path
is entirely owned by `TransactionalEventRepository`.

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

### 4.2 `cqrs-es-crypto` crate: expose `encrypt_events`

In `crates/cqrs-es-crypto/src/repository.rs`, change the visibility of
`encrypt_events` from private to public:

```rust
// BEFORE
async fn encrypt_events(
    &self,
    events: &[SerializedEvent],
) -> Result<Vec<SerializedEvent>, PersistenceError> { ... }

// AFTER
/// Encrypt PII fields in the given events according to the configured codec.
///
/// This is used by [`TransactionalEventRepository`] (or similar wrappers)
/// that manage their own persist transaction and need access to the
/// encrypted payloads without calling `persist`.
pub async fn encrypt_events(
    &self,
    events: &[SerializedEvent],
) -> Result<Vec<SerializedEvent>, PersistenceError> { ... }
```

No logic changes — just visibility.

### 4.3 New `LookupExtractor` trait

Define this in the `journey_dynamics` application crate (not in
`cqrs-es-crypto` — it carries domain knowledge about `PersonCaptured`
events):

```rust
// crates/journey_dynamics/src/lookup_extractor.rs

use cqrs_es::persist::SerializedEvent;

/// A subject-lookup row to be written transactionally with event persist.
pub struct SubjectLookup {
    pub subject_id: Uuid,
    pub email_lower: String,
}

/// Extracts subject-lookup entries from unencrypted serialised events.
pub trait LookupExtractor: Send + Sync {
    fn extract(&self, events: &[SerializedEvent]) -> Vec<SubjectLookup>;
}
```

And the concrete implementation:

```rust
/// Extracts (subject_id, email) from PersonCaptured events.
pub struct PersonCapturedLookupExtractor;

impl LookupExtractor for PersonCapturedLookupExtractor {
    fn extract(&self, events: &[SerializedEvent]) -> Vec<SubjectLookup> {
        events
            .iter()
            .filter(|e| e.event_type == "PersonCaptured")
            .filter_map(|e| {
                let inner = e.payload.get("PersonCaptured")?;
                let subject_id = inner.get("subject_id")?.as_str()?.parse::<Uuid>().ok()?;
                let email = inner.get("email")?.as_str()?;
                Some(SubjectLookup {
                    subject_id,
                    email_lower: email.to_lowercase(),
                })
            })
            .collect()
    }
}
```

### 4.4 New `TransactionalEventRepository`

Create `crates/journey_dynamics/src/transactional_event_repository.rs`:

```rust
use std::sync::Arc;

use cqrs_es::Aggregate;
use cqrs_es::persist::{
    PersistedEventRepository, PersistenceError, ReplayStream,
    SerializedEvent, SerializedSnapshot,
};
use cqrs_es_crypto::CryptoShreddingEventRepository;
use postgres_es::PostgresEventRepository;
use serde_json::Value;
use sqlx::{Pool, Postgres, Row};

use crate::lookup_extractor::LookupExtractor;

/// A [`PersistedEventRepository`] that writes events AND subject-lookup rows
/// in a single Postgres transaction, then delegates reads to the wrapped
/// [`CryptoShreddingEventRepository`].
pub struct TransactionalEventRepository {
    /// Shared Postgres connection pool — used for the write-path transaction.
    pool: Pool<Postgres>,
    /// Wrapped crypto repo — used for reads (decrypt) and `encrypt_events`.
    crypto: CryptoShreddingEventRepository<PostgresEventRepository>,
    /// Extracts lookup rows from unencrypted events.
    extractor: Arc<dyn LookupExtractor>,
}

impl TransactionalEventRepository {
    pub fn new(
        pool: Pool<Postgres>,
        crypto: CryptoShreddingEventRepository<PostgresEventRepository>,
        extractor: Arc<dyn LookupExtractor>,
    ) -> Self {
        Self { pool, crypto, extractor }
    }
}

impl PersistedEventRepository for TransactionalEventRepository {
    // ── Reads: delegate to the crypto repo (decrypt → inner → postgres) ──

    async fn get_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        self.crypto.get_events::<A>(aggregate_id).await
    }

    async fn get_last_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
        last_sequence: usize,
    ) -> Result<Vec<SerializedEvent>, PersistenceError> {
        self.crypto.get_last_events::<A>(aggregate_id, last_sequence).await
    }

    async fn get_snapshot<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<Option<SerializedSnapshot>, PersistenceError> {
        self.crypto.get_snapshot::<A>(aggregate_id).await
    }

    async fn stream_events<A: Aggregate>(
        &self,
        aggregate_id: &str,
    ) -> Result<ReplayStream, PersistenceError> {
        self.crypto.stream_events::<A>(aggregate_id).await
    }

    async fn stream_all_events<A: Aggregate>(
        &self,
    ) -> Result<ReplayStream, PersistenceError> {
        self.crypto.stream_all_events::<A>().await
    }

    // ── Write: encrypt, then single transaction for events + lookups ─────

    async fn persist<A: Aggregate>(
        &self,
        events: &[SerializedEvent],
        snapshot_update: Option<(String, Value, usize)>,
    ) -> Result<(), PersistenceError> {
        // 1. Extract lookups from the PLAINTEXT events (before encryption).
        let lookups = self.extractor.extract(events);

        // 2. Encrypt PII fields.
        let encrypted = self.crypto.encrypt_events(events).await?;

        // 3. Open a single transaction for events + lookups.
        let mut tx = self.pool.begin().await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

        // 3a. Insert encrypted events.
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

        // 3b. Upsert subject-lookup rows.
        for lookup in &lookups {
            sqlx::query(
                "INSERT INTO subject_lookup (subject_id, email_lower) \
                 VALUES ($1, $2) \
                 ON CONFLICT (subject_id) DO UPDATE SET email_lower = $2"
            )
            .bind(lookup.subject_id)
            .bind(&lookup.email_lower)
            .execute(&mut *tx)
            .await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
        }

        // 3c. Handle snapshot (if applicable — currently always None for
        //     event-sourced stores, but handle gracefully).
        if let Some((aggregate_id, aggregate, current_snapshot)) = snapshot_update {
            sqlx::query(
                "INSERT INTO snapshots \
                 (aggregate_type, aggregate_id, last_sequence, \
                  current_snapshot, payload) \
                 VALUES ($1, $2, 0, $3, $4) \
                 ON CONFLICT (aggregate_type, aggregate_id) DO UPDATE \
                 SET last_sequence = EXCLUDED.last_sequence, \
                     current_snapshot = $3, \
                     payload = $4"
            )
            .bind(A::TYPE)
            .bind(&aggregate_id)
            .bind(current_snapshot as i64)
            .bind(&aggregate)
            .execute(&mut *tx)
            .await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
        }

        // 3d. Commit — events and lookups are now atomically visible.
        tx.commit().await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;

        Ok(())
    }
}
```

**Key property:** The `events` parameter to `persist` contains
**unencrypted** `SerializedEvent`s (the `PersistedEventStore` calls
`persist` with plain events; encryption is normally done inside
`CryptoShreddingEventRepository::persist`). Our wrapper intercepts at this
level, extracts lookups from plaintext, encrypts, and writes both in one
transaction.

**About the event INSERT SQL:** This replicates what
`PostgresEventRepository::insert_events` does internally. The SQL is simple
(one INSERT per event) and the `events` table schema is defined in our own
migration. This is explicitly **not** forking `postgres-es` — we are
implementing the `PersistedEventRepository` trait ourselves for the write
path only. The `sequence` column is cast from `usize` to `i64` to match
the `BIGINT` type.

### 4.5 Update `config.rs`

```rust
// BEFORE
pub type CryptoCqrs = CqrsFramework<
    Journey,
    PersistedEventStore<CryptoShreddingEventRepository<PostgresEventRepository>, Journey>,
>;

// AFTER
pub type CryptoCqrs = CqrsFramework<
    Journey,
    PersistedEventStore<TransactionalEventRepository, Journey>,
>;
```

In `cqrs_framework`:

```rust
// BEFORE
let inner = PostgresEventRepository::new(pool);
let codec = Arc::new(JourneyPiiCodec);
let crypto_repo = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec);
let store = PersistedEventStore::new_event_store(crypto_repo);

// AFTER
let inner = PostgresEventRepository::new(pool.clone());
let codec = Arc::new(JourneyPiiCodec);
let crypto_repo = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec);
let extractor = Arc::new(PersonCapturedLookupExtractor);
let transactional = TransactionalEventRepository::new(pool, crypto_repo, extractor);
let store = PersistedEventStore::new_event_store(transactional);
```

### 4.6 Update `find_subjects_by_email` to use `subject_lookup`

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

### 4.7 Update shredding flow: delete from `subject_lookup`

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

### 4.8 `journey_person` becomes a pure projection

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

## 5. What about the DEK creation?

`CryptoShreddingEventRepository::encrypt_events` calls
`key_store.get_or_create_key()`, which writes to `subject_encryption_keys`
**outside** our transaction. This means DEK creation and event+lookup
persist are not atomic. Failure modes:

| DEK created? | Events+lookups committed? | Outcome |
|---|---|---|
| ✅ | ✅ | Happy path |
| ✅ | ❌ | Orphan DEK — harmless, unused key material |
| ❌ | N/A | `encrypt_events` returns error, persist never attempted |

An orphan DEK wastes a row in `subject_encryption_keys` but has no
security or correctness implications. No action needed.

## 6. Testing strategy

### Unit tests

- **`LookupExtractor`**: feed it various `SerializedEvent` payloads
  (PersonCaptured, Modified, PersonDetailsUpdated, SubjectForgotten) and
  assert it only extracts lookups from PersonCaptured.
- **`TransactionalEventRepository::persist`**: use a test database.
  Persist a batch of events including a PersonCaptured. Verify that both
  `events` and `subject_lookup` contain the expected rows. Then verify that
  a failed persist (e.g. duplicate sequence) rolls back both.

### Integration / Hurl tests

- **Existing `full-flight-booking_with_shredding_by_email.hurl`**: should
  pass unchanged — the behaviour is identical, just the underlying table
  is different.
- **New test**: capture a person, verify `subject_lookup` contains the
  mapping (query the DB directly or use the existing shred-by-email
  endpoint), then shred by email and verify the lookup row is gone.

### Regression

- All existing tests should pass. `journey_person` is still populated by
  the projection and still serves the read API. The only query that changes
  is `find_subjects_by_email`.

## 7. Migration / deployment order

1. **Deploy the migration** — creates `subject_lookup`, backfills from
   `journey_person`. Zero downtime; additive only.
2. **Deploy the code** — `find_subjects_by_email` switches to
   `subject_lookup`; new events write to both tables (projection writes
   `journey_person`, transactional write writes `subject_lookup`).
3. Optionally: remove the `email` index from `journey_person` if it was
   only serving the lookup query.

No breaking changes; fully backwards-compatible.

## 8. Files to change

| File | Change |
|---|---|
| `migrations/YYYYMMDDHHMMSS_subject_lookup.up.sql` | New migration |
| `migrations/YYYYMMDDHHMMSS_subject_lookup.down.sql` | Down migration |
| `crates/cqrs-es-crypto/src/repository.rs` | Make `encrypt_events` `pub` |
| `crates/journey_dynamics/src/lookup_extractor.rs` | **New file** — `SubjectLookup`, `LookupExtractor` trait, `PersonCapturedLookupExtractor` |
| `crates/journey_dynamics/src/transactional_event_repository.rs` | **New file** — `TransactionalEventRepository` |
| `crates/journey_dynamics/src/config.rs` | Update `CryptoCqrs` type alias; wire `TransactionalEventRepository` |
| `crates/journey_dynamics/src/state.rs` | Expose `pool` on `ApplicationState` (or add lookup cleanup method) |
| `crates/journey_dynamics/src/view_repository.rs` | `find_subjects_by_email` → query `subject_lookup`; add `delete_subject_lookup` |
| `crates/journey_dynamics/src/route_handler.rs` | After DEK deletion, delete from `subject_lookup` |
| `crates/journey_dynamics/src/lib.rs` | Add `mod lookup_extractor; mod transactional_event_repository;` |
| Tests (Rust + Hurl) | Update/add as described in §6 |

## 9. Sequence column type note

`postgres-es` casts `event.sequence` as `i32` in its INSERT. Our events
table defines `sequence` as `BIGINT`. The `TransactionalEventRepository`
should bind as `i64` to match the column type. This is a minor
improvement over the upstream crate's narrower cast.

## 10. Future considerations

- **Generalised lookup table**: if more lookup dimensions are needed
  (phone, passport), consider `(subject_id, lookup_type, lookup_value)`
  with a composite primary key. The extractor trait already supports this
  — just return more `SubjectLookup` variants.
- **HMAC-hashed lookups**: for defence-in-depth, store
  `HMAC-SHA256(secret, email_lower)` instead of `email_lower`. Requires
  a stable HMAC key (distinct from the KEK) and caller-side hash
  computation. Adds complexity; defer unless the threat model warrants it.
- **`find_by_email` (journey lookup)**: currently queries
  `journey_person.email`. Can be rewritten as
  `subject_lookup → find_journeys_by_subject` for consistency, or left
  as a projection query. Low priority.
