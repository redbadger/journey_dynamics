# KEK Rotation & Cloud Key-Vault Integration — Design Document

| | |
|---|---|
| **Crate** | `cqrs-es-crypto` (primary), `journey_dynamics` (wiring) |
| **Feature** | Rotatable KEK sourced from a cloud key vault; zero-downtime re-wrap of DEKs |
| **Status** | Proposed — ready to hand off to an implementing agent |
| **Depends on** | [MULTI_SUBJECT_DESIGN.md](./MULTI_SUBJECT_DESIGN.md) (Implemented) |

---

## Table of Contents

1. [Motivation](#motivation)
2. [Goals & Non-Goals](#goals--non-goals)
3. [Background: where the KEK lives today](#background-where-the-kek-lives-today)
4. [Design Overview](#design-overview)
5. [Key Concepts](#key-concepts)
   - [KEK version / KEK id](#kek-version--kek-id)
   - [Primary vs. retired KEK versions](#primary-vs-retired-kek-versions)
   - [Envelope-encryption mode vs. local-wrap mode](#envelope-encryption-mode-vs-local-wrap-mode)
6. [Database Schema Changes](#database-schema-changes)
7. [Crate API Changes](#crate-api-changes)
   - [New trait: `KekProvider`](#new-trait-kekprovider)
   - [`PiiCipher` becomes version-aware](#piicipher-becomes-version-aware)
   - [`KeyStore` trait additions](#keystore-trait-additions)
   - [`PostgresKeyStore` changes](#postgreskeystore-changes)
8. [Built-in `KekProvider` implementations](#built-in-kekprovider-implementations)
   - [`StaticKekProvider` (config / env var)](#statickekprovider-config--env-var)
   - [`MultiVersionKekProvider` (rotation glue)](#multiversionkekprovider-rotation-glue)
   - [`AwsKmsKekProvider` (optional feature)](#awskmskekprovider-optional-feature)
   - [Sketches for GCP KMS / Azure Key Vault / HashiCorp Vault](#sketches-for-gcp-kms--azure-key-vault--hashicorp-vault)
9. [Rotation Flow](#rotation-flow)
   - [Step 1 — Introduce a new KEK version](#step-1--introduce-a-new-kek-version)
   - [Step 2 — Promote the new version to primary](#step-2--promote-the-new-version-to-primary)
   - [Step 3 — Re-wrap existing DEKs](#step-3--re-wrap-existing-deks)
   - [Step 4 — Retire the old version](#step-4--retire-the-old-version)
10. [Lazy Re-wrap on Read](#lazy-re-wrap-on-read)
11. [Background Re-wrap Worker](#background-re-wrap-worker)
12. [Concurrency, Correctness & Safety](#concurrency-correctness--safety)
13. [Wiring in `journey_dynamics`](#wiring-in-journey_dynamics)
14. [Observability](#observability)
15. [Testing Strategy](#testing-strategy)
16. [Step-by-Step Execution Plan](#step-by-step-execution-plan)
17. [Risk Assessment](#risk-assessment)
18. [Future Work](#future-work)

---

## Motivation

Today the KEK is loaded once from the `JOURNEY_KEK` environment variable and held
in memory for the lifetime of the process (see `crates/journey_dynamics/src/state.rs`).
A `PiiCipher` is constructed with the 32 raw bytes and used both by
`PostgresKeyStore` (for DEK wrap/unwrap) and by `CryptoShreddingEventRepository`
(for field-level AES-GCM encryption — note: field encryption uses the *DEK*, not
the KEK, so the KEK does not actually need to be present in the cipher used for
field encryption; that is one of the small cleanups this work picks up).

In a production setting the KEK should:

1. Live in a cloud key vault (AWS KMS, GCP KMS, Azure Key Vault, HashiCorp Vault
   Transit) — never in a config file or environment variable beyond bootstrap.
2. Be **rotatable** — operators create a new version, the system starts using
   it for new writes immediately, and existing DEKs get re-wrapped without
   downtime.
3. Be **retire-able** — once all DEKs have been re-wrapped under the new
   version, the old version can be deleted at the vault, which is itself a
   useful security property (forward-secrecy boundary).

The crate already documents this as a future direction
([CRYPTO_SHREDDING_DESIGN.md § Future Considerations](./CRYPTO_SHREDDING_DESIGN.md#key-rotation)):

> KEK rotation is simpler [than DEK rotation] — re-wrap all DEKs with the new
> KEK without touching the event store. The design should prioritise KEK
> rotation and treat DEK rotation as an exceptional maintenance operation.

This document turns that direction into a concrete plan.

---

## Goals & Non-Goals

### Goals

1. Abstract KEK access behind a trait so the implementation can be swapped
   between a local in-memory KEK (tests / dev) and a cloud KMS (production)
   without touching anything downstream.
2. Allow **multiple KEK versions to coexist**: every wrapped DEK records the id
   of the KEK that wrapped it, so the system can always unwrap legacy DEKs
   while writing new ones under the latest version.
3. Provide a **zero-downtime rotation path**:
   - The new KEK version becomes available alongside the old.
   - The application starts wrapping new DEKs with the new version
     immediately.
   - Existing DEKs are re-wrapped lazily on read **and** by a background
     sweep job.
   - Once the sweep completes, the old KEK version can be retired at the
     vault.
4. Support both **local-wrap** (RFC 5649 AES-KWP, KEK material available to
   the process — current behaviour) and **envelope-encryption against a remote
   KMS** (the KMS performs wrap/unwrap; KEK material never leaves the vault).
5. Keep the change additive: existing event payloads are untouched. Only the
   `subject_encryption_keys` table grows a column and the DEK wrap format
   gains a `kek_id` tag.

### Non-Goals

1. **DEK rotation.** Re-encrypting individual events with new DEKs is out of
   scope and is explicitly documented as an exceptional operation in
   `CRYPTO_SHREDDING_DESIGN.md`. Crypto-shredding remains the primary mechanism
   for removing a DEK from service.
2. **Snapshot encryption.** The existing limitation that aggregate snapshots
   are not encrypted is unchanged by this work.
3. **HSM integration beyond what a KMS already provides.** Where a KMS is
   HSM-backed (AWS KMS, GCP KMS), the application transparently inherits
   those guarantees; we do not add a separate PKCS#11 path.
4. **Cross-region / multi-tenant KEK partitioning.** A single logical KEK
   stream per deployment is sufficient.

---

## Background: where the KEK lives today

```
crates/journey_dynamics/src/state.rs   ── env var → 32 bytes → PiiCipher
                                                 │
                                                 ├── PostgresKeyStore     (wraps/unwraps DEKs)
                                                 └── CryptoShreddingEventRepository
                                                       (only uses the DEK; doesn't need the KEK)

crates/cqrs-es-crypto/src/cipher.rs    ── PiiCipher::{wrap_dek, unwrap_dek}     (AES-KWP)
                                          PiiCipher::{encrypt,  decrypt}        (AES-GCM, uses a DEK)

crates/cqrs-es-crypto/src/key_store.rs ── PostgresKeyStore holds a PiiCipher
                                          INSERT … VALUES (key_id, subject_id, wrapped_key)

migrations/20260423132137_init.up.sql  ── CREATE TABLE subject_encryption_keys
                                          (key_id, subject_id, wrapped_key, created_at)
```

The implication is that **all wrap/unwrap activity is funnelled through one
spot** (`PostgresKeyStore` via its `PiiCipher`). That is the surface we change.

---

## Design Overview

```
                              ┌────────────────────────────────────────┐
                              │            KekProvider (trait)         │
                              │                                        │
                              │  current() -> KekHandle                │
                              │  by_id(id) -> Option<KekHandle>        │
                              │                                        │
                              │  wrap(dek, kek_id)   -> WrappedDek     │
                              │  unwrap(WrappedDek)  -> KeyMaterial    │
                              └────────────────────────────────────────┘
                                  ▲                ▲                ▲
                                  │                │                │
                  ┌───────────────┴──┐   ┌─────────┴────────┐  ┌────┴────────────┐
                  │ StaticKek        │   │ MultiVersionKek  │  │ AwsKmsKek       │
                  │ (one local KEK,  │   │ (composes others;│  │ (calls KMS for  │
                  │  e.g. from env)  │   │  picks primary)  │  │  Encrypt/Decrypt│
                  └──────────────────┘   └──────────────────┘  └─────────────────┘

PostgresKeyStore { provider: Arc<dyn KekProvider> }   ←─ replaces the in-line PiiCipher
        │
        │   row layout becomes:
        │     (key_id, subject_id, wrapped_key BYTEA, kek_id TEXT, created_at)
        │
        └── on read:  lookup by subject_id; provider.unwrap({ kek_id, bytes });
                       if kek_id != provider.current().id { spawn re-wrap (lazy) }
            on write: provider.wrap(dek, provider.current().id)
```

The `PiiCipher` used by `CryptoShreddingEventRepository` for **field**
encryption keeps its current shape but loses the KEK — it only needs a DEK to
do AES-GCM. That cipher is renamed `FieldCipher` (or `PiiCipher` keeps its
name and drops the KEK constructor; bikeshed in code review).

---

## Key Concepts

### KEK version / KEK id

Every wrapped DEK carries a string id of the KEK that produced it. The id is
opaque to the crate; producers choose any stable identifier:

| Provider | Example id |
|---|---|
| `StaticKekProvider` | `"env:v1"`, `"env:v2"` |
| `AwsKmsKekProvider` | `"aws-kms:arn:aws:kms:eu-west-2:123:key/abc/version/<id>"` (the KMS *key-version* ARN) |
| `GcpKmsKekProvider` | `"gcp-kms:projects/p/locations/l/keyRings/r/cryptoKeys/k/cryptoKeyVersions/3"` |
| `VaultTransitProvider` | `"vault:transit/keys/journeys:7"` |

The id is stored in `subject_encryption_keys.kek_id`. There is no
interpretation in SQL — it is a plain text column.

### Primary vs. retired KEK versions

A `KekProvider` exposes:

- **`current() -> KekHandle`** — the KEK version used for new wraps.
- **`by_id(&str) -> Option<KekHandle>`** — any version we can still unwrap with.

Versions transition through a small state machine:

```
   (introduced)                  (promoted)                  (re-wrapped)               (deleted at vault)
non-existent ──► available ──────────► primary ──────────► deprecated ──────────► retired
                  │                                            │
                  └── (rolled back) ◄──────────────────────────┘
```

Only one version is `primary` at a time. The provider may report any number
of `available` / `deprecated` versions, and **must** be able to unwrap with
all of them.

### Envelope-encryption mode vs. local-wrap mode

| Mode | Where wrap/unwrap happens | Where KEK material lives | Latency per DEK fetch |
|---|---|---|---|
| **Local wrap** (today) | In-process via AES-KWP | In application memory | Microseconds |
| **Envelope encryption** (KMS) | Inside the KMS (`Encrypt`/`Decrypt`) | Vault only — never in the process | A network round-trip |

Both modes implement the same `KekProvider` trait. The difference is purely
internal: a local provider does AES-KWP; a KMS provider makes a network call.
Downstream code does not distinguish. For KMS providers, a **per-DEK cache**
of unwrapped material with a short TTL is essential to keep read latency
acceptable (see [Observability](#observability)).

---

## Database Schema Changes

A new migration adds two columns to `subject_encryption_keys`:

```sql
ALTER TABLE subject_encryption_keys
    ADD COLUMN kek_id     TEXT      NOT NULL DEFAULT 'legacy:v1',
    ADD COLUMN rewrapped_at TIMESTAMP;        -- last time wrapped_key was re-wrapped

ALTER TABLE subject_encryption_keys
    ALTER COLUMN kek_id DROP DEFAULT;          -- new rows must specify

-- Helps the background sweeper find rows still on an old KEK.
CREATE INDEX idx_subject_keys_kek_id ON subject_encryption_keys (kek_id);
```

Notes:

- The `'legacy:v1'` default tags any existing rows so they can be matched by
  `StaticKekProvider` configured with the same id. This makes the migration
  no-op-safe: the running application keeps working, and a configured
  rotation can target the legacy id explicitly.
- `rewrapped_at` is observability sugar — useful for dashboards and for
  verifying the sweeper made progress; not load-bearing logic.

Companion `down.sql` drops the column and the index.

### Migration file

Add a new file under `migrations/`:

```
migrations/<timestamp>_kek_versioning.up.sql
migrations/<timestamp>_kek_versioning.down.sql
```

Use the same date-prefix convention as `20260423132137_init.up.sql`.

---

## Crate API Changes

All changes live in `crates/cqrs-es-crypto/src/`.

### New trait: `KekProvider`

New module `kek.rs`:

```rust
// crates/cqrs-es-crypto/src/kek.rs

use async_trait::async_trait;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::cipher::KeyMaterial;

/// A wrapped DEK as it exists at rest — bytes plus the id of the KEK that
/// produced the wrap. Both are persisted in `subject_encryption_keys`.
#[derive(Clone, Debug)]
pub struct WrappedDek {
    pub key_id:      uuid::Uuid,   // identifies the DEK itself
    pub kek_id:      String,       // identifies the KEK version that wrapped it
    pub wrapped_key: Vec<u8>,      // opaque ciphertext from the provider
}

/// Identifies a KEK version and carries any provider-internal handle needed
/// to use it. Opaque from the outside.
#[derive(Clone, Debug)]
pub struct KekHandle {
    pub id: String,
}

#[derive(Debug, Error)]
pub enum KekError {
    #[error("Unknown KEK id: {0}")]
    UnknownVersion(String),
    #[error("Wrap failed: {0}")]
    Wrap(Box<dyn std::error::Error + Send + Sync>),
    #[error("Unwrap failed: {0}")]
    Unwrap(Box<dyn std::error::Error + Send + Sync>),
    #[error("Vault transport error: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync>),
}

#[async_trait]
pub trait KekProvider: Send + Sync {
    /// The KEK version used for new wraps.
    fn current(&self) -> KekHandle;

    /// Look up a specific version. Returns `None` if the version has been
    /// retired and the provider can no longer access it.
    fn by_id(&self, id: &str) -> Option<KekHandle>;

    /// Wrap a freshly-generated DEK. Implementations must use the supplied
    /// `kek` handle (so callers can pin to a specific version during a
    /// re-wrap). Most call-sites will pass `provider.current()`.
    async fn wrap(
        &self,
        kek: &KekHandle,
        dek: &KeyMaterial,
    ) -> Result<WrappedDek, KekError>;

    /// Unwrap a stored DEK using the KEK version recorded on the row.
    async fn unwrap(
        &self,
        wrapped: &WrappedDek,
    ) -> Result<KeyMaterial, KekError>;
}
```

#### Design points

- **Async** because some implementations (KMS) make network calls. Local
  implementations simply don't `await` anything internally.
- The trait owns wrap/unwrap end-to-end. We deliberately do **not** expose
  raw KEK bytes through the trait, so a KMS provider that never sees the
  material can implement it without lying.
- `wrap` takes an explicit `KekHandle` so the background re-wrap worker can
  pin to `provider.current()` and refuse to be racing against a fresh
  rotation — see [Concurrency](#concurrency-correctness--safety).

### `PiiCipher` becomes version-aware

The current `PiiCipher` mixes two responsibilities: AES-GCM field
encryption (which only needs a DEK) and AES-KWP DEK wrapping (which needs
a KEK). Split them:

- **`FieldCipher`** — what `PiiCipher` becomes minus the KEK. Used by
  `CryptoShreddingEventRepository`. Pure stateless AES-GCM. Keeps the AAD
  contract intact.
- **`LocalKwpKek`** — internal helper used by `StaticKekProvider`. Wraps a
  single KEK version and exposes `wrap_dek` / `unwrap_dek`. Not part of
  the public API.

This is mostly a rename + move. The existing test suite for `PiiCipher`
splits naturally into a `FieldCipher` test module and a `LocalKwpKek` test
module.

Backwards compatibility: re-export `PiiCipher` as a type alias for
`FieldCipher` and mark it `#[deprecated]` for one release if we want to
avoid an immediate breaking change in downstream crates.

### `KeyStore` trait additions

The trait grows two methods, both with sensible defaults so an
`InMemoryKeyStore` does not have to care:

```rust
#[async_trait]
pub trait KeyStore: Send + Sync {
    async fn get_or_create_key(&self, subject_id: &Uuid) -> Result<KeyMaterial, KeyStoreError>;
    async fn get_key(&self, subject_id: &Uuid)            -> Result<Option<KeyMaterial>, KeyStoreError>;
    async fn delete_key(&self, subject_id: &Uuid)         -> Result<(), KeyStoreError>;

    // ── New ──

    /// Iterate over subjects whose DEK is wrapped under a KEK version other
    /// than `current_kek_id`. Used by the background re-wrap worker.
    async fn list_stale_subjects(
        &self,
        current_kek_id: &str,
        batch_size: usize,
        after: Option<Uuid>,
    ) -> Result<Vec<Uuid>, KeyStoreError> {
        let _ = (current_kek_id, batch_size, after);
        Ok(Vec::new()) // default: store has no notion of staleness
    }

    /// Re-wrap a single subject's DEK with the provider's current KEK.
    /// Idempotent: if the DEK is already wrapped under the current KEK, this
    /// is a no-op that still updates `rewrapped_at`.
    ///
    /// Returns `Ok(false)` if the row no longer exists (already shredded).
    async fn rewrap_key(&self, subject_id: &Uuid) -> Result<bool, KeyStoreError> {
        let _ = subject_id;
        Ok(false)
    }
}
```

### `PostgresKeyStore` changes

```rust
pub struct PostgresKeyStore {
    pool: sqlx::Pool<sqlx::Postgres>,
    provider: Arc<dyn KekProvider>,
}
```

- `new` takes `Arc<dyn KekProvider>` instead of `PiiCipher`.
- `get_or_create_key`:
  1. Fast path: `get_key` (see below).
  2. Slow path: `PiiCipher::generate_dek()` → `provider.wrap(provider.current(), &dek)`
     → `INSERT … (key_id, subject_id, wrapped_key, kek_id) VALUES … ON CONFLICT DO NOTHING`.
- `get_key`:
  1. `SELECT key_id, wrapped_key, kek_id FROM subject_encryption_keys WHERE subject_id = $1`.
  2. `provider.unwrap(&WrappedDek { … })`.
  3. **Lazy re-wrap**: if `kek_id != provider.current().id`, spawn a detached
     task that calls `self.rewrap_key(subject_id)`. The current request
     returns the unwrapped material immediately; the re-wrap is best-effort
     and races with the background sweeper safely
     (see [Concurrency](#concurrency-correctness--safety)).
- `rewrap_key`:
  1. `SELECT key_id, wrapped_key, kek_id FROM subject_encryption_keys WHERE subject_id = $1`.
  2. Short-circuit if `kek_id == provider.current().id`.
  3. `unwrap` with the old version → `wrap` with the current.
  4. `UPDATE subject_encryption_keys SET wrapped_key = $1, kek_id = $2,
        rewrapped_at = NOW() WHERE subject_id = $3 AND kek_id = $4`.
     The `AND kek_id = $4` clause makes the UPDATE a compare-and-swap:
     concurrent re-wraps and lazy re-wraps cannot clobber each other.
- `list_stale_subjects`:
  ```sql
  SELECT subject_id FROM subject_encryption_keys
   WHERE kek_id <> $1 AND ($2::uuid IS NULL OR subject_id > $2)
   ORDER BY subject_id
   LIMIT $3;
  ```
  Backed by `idx_subject_keys_kek_id`.

`InMemoryKeyStore` gets the same surface using the same trait defaults.
For tests that exercise rotation, extend its internal map to hold
`(key_id, kek_id, key_bytes)` and implement `list_stale_subjects` /
`rewrap_key` properly.

---

## Built-in `KekProvider` implementations

### `StaticKekProvider` (config / env var)

For development and tests; replaces today's "one env var" model with a
"map of versioned env vars" model:

```rust
pub struct StaticKekProvider {
    primary: String,
    keks: HashMap<String, Zeroizing<[u8; 32]>>,
}

impl StaticKekProvider {
    pub fn from_env(prefix: &str) -> Result<Self, KekError> { /* … */ }
    pub fn builder() -> StaticKekProviderBuilder { /* … */ }
}
```

Environment-variable schema:

```
JOURNEY_KEK_PRIMARY=v2
JOURNEY_KEK_v1=<base64>      # still readable for legacy rows
JOURNEY_KEK_v2=<base64>      # used for new wraps
```

The `from_env` constructor enumerates `JOURNEY_KEK_<id>` variables and
reads `JOURNEY_KEK_PRIMARY` for the current id.

### `MultiVersionKekProvider` (rotation glue)

A thin combinator for the (rare) case where versions come from multiple
sources — e.g. legacy DEKs were wrapped with a static env-var KEK and new
DEKs should be wrapped via KMS:

```rust
pub struct MultiVersionKekProvider {
    primary: Arc<dyn KekProvider>,
    legacy:  Vec<Arc<dyn KekProvider>>,
}

#[async_trait]
impl KekProvider for MultiVersionKekProvider {
    fn current(&self) -> KekHandle { self.primary.current() }
    fn by_id(&self, id: &str) -> Option<KekHandle> {
        self.primary.by_id(id).or_else(||
            self.legacy.iter().find_map(|p| p.by_id(id)))
    }
    async fn wrap(&self, kek: &KekHandle, dek: &KeyMaterial) -> Result<WrappedDek, KekError> {
        self.primary.wrap(kek, dek).await
    }
    async fn unwrap(&self, wrapped: &WrappedDek) -> Result<KeyMaterial, KekError> {
        if let Some(_) = self.primary.by_id(&wrapped.kek_id) {
            return self.primary.unwrap(wrapped).await;
        }
        for p in &self.legacy {
            if p.by_id(&wrapped.kek_id).is_some() {
                return p.unwrap(wrapped).await;
            }
        }
        Err(KekError::UnknownVersion(wrapped.kek_id.clone()))
    }
}
```

This is the migration vehicle: while moving from env-var to KMS, the
operator runs with `primary = KMS, legacy = [StaticKekProvider]`. Once the
sweeper has re-wrapped everything (verified by `SELECT COUNT(*) … WHERE
kek_id IN (<legacy ids>)` returning zero), the `legacy` providers are
removed from configuration.

### `AwsKmsKekProvider` (optional feature)

Gated behind a new `aws-kms` Cargo feature so the crate has no AWS
dependency by default.

```toml
[features]
aws-kms = ["dep:aws-sdk-kms", "dep:aws-config"]
```

```rust
pub struct AwsKmsKekProvider {
    client:      aws_sdk_kms::Client,
    key_id:      String,          // e.g. "alias/journey-kek" or the key ARN
    current_arn: ArcSwap<String>, // the active key-version ARN
    cache:       moka::future::Cache<String, Zeroizing<Vec<u8>>>, // plaintext DEK cache
}
```

- `wrap` calls `kms:Encrypt` with `KeyId = self.key_id`, plaintext = DEK
  bytes, `EncryptionContext = { "subject_id": "<uuid>" }` (binds the DEK
  to the subject — analogous to AAD). KMS returns ciphertext and the
  ARN-with-version of the key actually used; that ARN becomes
  `WrappedDek::kek_id`.
- `unwrap` calls `kms:Decrypt` with the ciphertext and the same encryption
  context. KMS automatically uses whichever key version originally
  encrypted, so the provider does not need to track versions itself; the
  recorded `kek_id` is informational and is consumed by `list_stale_subjects`.
- `current()` returns the latest key-version ARN. Refresh strategy: a
  `current_arn` watcher polls `kms:DescribeKey` (or `ListKeyRotations`)
  every N minutes and `ArcSwap`s the value.
- Plaintext DEKs returned from KMS are cached in a small bounded LRU
  (e.g. `moka`) keyed by ciphertext bytes, TTL ≈ 5 min. This is critical
  for read latency — a `kms:Decrypt` call is multi-millisecond.

Authentication: the crate does **not** hard-code credential provisioning.
Constructors take a `aws_sdk_kms::Client`, leaving credential strategy to
the caller (instance profile, IRSA, env vars, etc.).

### Sketches for GCP KMS / Azure Key Vault / HashiCorp Vault

Each is structurally the same as `AwsKmsKekProvider`: a `Client`, a
`current()` watcher, an envelope-encryption call for `wrap` and `unwrap`,
and a plaintext-DEK cache. They live behind their own Cargo features
(`gcp-kms`, `azure-key-vault`, `vault-transit`). They are out of scope for
the first cut; only stub modules with `unimplemented!()` should be added
so the feature flags exist on day one.

---

## Rotation Flow

The flow assumes a `StaticKekProvider` for simplicity; the KMS flow is the
same modulo "create a new key version at the vault" replacing "set a new
env var".

### Step 1 — Introduce a new KEK version

Operator action:

```
# Existing
JOURNEY_KEK_PRIMARY=v1
JOURNEY_KEK_v1=<base64>

# Add
JOURNEY_KEK_v2=<base64 of a freshly-generated 32-byte key>
```

`JOURNEY_KEK_PRIMARY` is **not yet** changed. Roll out the new env var to
all replicas. Verify in logs:

```
StaticKekProvider: known kek_ids = ["v1", "v2"], primary = "v1"
```

At this point nothing changes about behaviour. The system is simply
*aware* of both versions.

### Step 2 — Promote the new version to primary

```
JOURNEY_KEK_PRIMARY=v2
```

Roll out. New DEKs are now wrapped with `v2`. Existing DEKs continue to
unwrap from `v1` because both keys are present. Lazy re-wrap kicks in on
every read.

### Step 3 — Re-wrap existing DEKs

Run the background sweeper (see [Background Re-wrap Worker](#background-re-wrap-worker)).
Monitor:

```sql
SELECT kek_id, COUNT(*) FROM subject_encryption_keys GROUP BY kek_id;
```

When the count for `v1` reaches zero, the rotation is complete.

### Step 4 — Retire the old version

After confirming zero `v1` rows for some safety margin (e.g. 24 hours, in
case a row was created and then shredded back to `v1` somehow — should be
impossible but worth verifying), remove the variable:

```
JOURNEY_KEK_PRIMARY=v2
JOURNEY_KEK_v2=<base64>
# JOURNEY_KEK_v1 deleted
```

For KMS providers: schedule `kms:ScheduleKeyDeletion` (or the equivalent
in other clouds) for the retired key version, with the cloud's mandatory
waiting period as a safety net.

---

## Lazy Re-wrap on Read

Implemented inside `PostgresKeyStore::get_key`. The sequence:

1. Read row, unwrap DEK, return material to caller. **This is the hot
   path and is never blocked by re-wrap work.**
2. If `wrapped.kek_id != provider.current().id`, fire-and-forget:
   ```rust
   let me = self.clone();
   let subject_id = *subject_id;
   tokio::spawn(async move {
       if let Err(e) = me.rewrap_key(&subject_id).await {
           tracing::warn!(?subject_id, ?e, "lazy re-wrap failed");
       }
   });
   ```

Properties:

- **At-most-once-per-read semantics; eventually-converges semantics.** If
  the spawned task fails, the next read of the same subject re-triggers it.
- The CAS in the `UPDATE` (`WHERE … AND kek_id = $4`) guarantees that
  concurrent lazy re-wraps and sweeper re-wraps cannot regress the row.
- Lazy re-wrap is the primary mechanism for "hot" subjects. The sweeper
  handles cold subjects.

Make this behaviour configurable on `PostgresKeyStore`:

```rust
pub struct PostgresKeyStoreOptions {
    pub lazy_rewrap: bool, // default: true
}
```

Disabling is useful for tests that need to assert deterministic state.

---

## Background Re-wrap Worker

A standalone struct in `cqrs-es-crypto`:

```rust
pub struct RewrapWorker<S: KeyStore> {
    store: Arc<S>,
    provider: Arc<dyn KekProvider>,
    options: RewrapWorkerOptions,
}

pub struct RewrapWorkerOptions {
    pub batch_size: usize,           // default: 100
    pub max_concurrency: usize,      // default: 8
    pub batch_pause: Duration,       // default: 100ms — gentle on the DB
}

impl<S: KeyStore> RewrapWorker<S> {
    pub async fn run_once(&self) -> Result<RewrapStats, KeyStoreError> { /* … */ }
    pub async fn run_forever(&self, poll: Duration) -> ! { /* … */ }
}

pub struct RewrapStats {
    pub scanned: usize,
    pub rewrapped: usize,
    pub failures: usize,
    pub duration: Duration,
}
```

`run_once`:

```rust
let current = self.provider.current().id;
let mut cursor: Option<Uuid> = None;
loop {
    let batch = self.store.list_stale_subjects(&current, self.options.batch_size, cursor).await?;
    if batch.is_empty() { break; }
    cursor = batch.last().copied();

    futures::stream::iter(batch.iter().copied())
        .for_each_concurrent(self.options.max_concurrency, |subject_id| async move {
            let _ = self.store.rewrap_key(&subject_id).await; // errors logged inside
        })
        .await;

    tokio::time::sleep(self.options.batch_pause).await;
}
```

Operationally the worker is either:

1. **Embedded** in the application as a long-running `tokio::spawn` task
   (simplest; one fewer deployable). `run_forever` polls every N minutes;
   when there is nothing to do, this is one indexed `SELECT … LIMIT 1`.
2. **A standalone CLI** (`cargo run --bin rewrap` in `journey_dynamics`)
   that runs `run_once` and exits — easy to schedule as a Kubernetes Job
   or one-off task during a rotation window.

Implement both; embedded is the default in `journey_dynamics`, the CLI
exists for forced re-runs.

---

## Concurrency, Correctness & Safety

| Concern | Mitigation |
|---|---|
| Two replicas re-wrap the same row simultaneously. | `UPDATE … WHERE subject_id = $1 AND kek_id = $old` — at most one wins; loser sees `rows_affected == 0` and treats it as success (someone else did it). |
| A re-wrap races with a shredding `DELETE`. | The `UPDATE` is also gated on the row existing; `rows_affected == 0` means "already shredded" — no error, no leftover state. |
| A re-wrap races with a fresh write for a new subject. | New writes insert with `kek_id = provider.current()`; the sweeper would not pick them up. The lazy re-wrap path short-circuits in step 2 when `kek_id` already matches. |
| Mid-rotation operator rolls back `JOURNEY_KEK_PRIMARY` (v2 → v1). | Some rows are now under v2 (the "new primary") while the running provider considers v1 primary. Lazy re-wrap and the sweeper will re-wrap **v2 rows back to v1**. As long as v2 is still in the provider's `by_id` table this is safe; if v2 has already been removed, those rows become unreadable. Document this loudly: **never remove a KEK version that still has DEKs wrapped under it.** The `kek_id` index makes the pre-flight check a one-liner. |
| KMS rate-limits during a big sweep. | `batch_pause` + `max_concurrency` are tunable; `RewrapStats` exposes failures. The CAS-update is idempotent so retry is safe. |
| Plaintext DEK lingering in memory after KMS decrypt. | DEKs returned through the trait are `Zeroizing`. The KMS cache stores `Zeroizing<Vec<u8>>` and bounds entry lifetime via TTL. |
| AAD changes? | **No.** Field encryption AAD remains `"<aggregate_id>:<sequence>"`. The DEK bytes are unchanged by re-wrap; only the wrapping changes. No event payload is touched. |
| `key_id` vs. `kek_id` confusion. | The DEK's `key_id` (its own UUID) is unchanged forever. The `kek_id` changes on re-wrap. The two are stored in separate columns and named carefully throughout. |

---

## Wiring in `journey_dynamics`

`crates/journey_dynamics/src/state.rs` becomes:

```rust
use cqrs_es_crypto::{
    FieldCipher, KekProvider, KeyStore, PostgresKeyStore, RewrapWorker,
    StaticKekProvider,
};

pub async fn new_application_state() -> ApplicationState {
    let pool = /* … as before … */;
    sqlx::migrate!("../../migrations").run(&pool).await.expect(/* … */);

    // 1. Build the KEK provider — env-driven for now, KMS-driven in production.
    let provider: Arc<dyn KekProvider> = Arc::new(
        StaticKekProvider::from_env("JOURNEY_KEK").expect("KEK env config")
    );

    // 2. Key store now owns the provider.
    let key_store: Arc<dyn KeyStore> = Arc::new(
        PostgresKeyStore::new(pool.clone(), Arc::clone(&provider))
    );

    // 3. Field cipher no longer needs a KEK.
    let cipher = FieldCipher::new();

    let (cqrs, journey_query) =
        cqrs_framework(pool.clone(), Arc::clone(&key_store), cipher);

    // 4. Spawn the background re-wrap worker.
    let worker = RewrapWorker::new(
        Arc::clone(&key_store),
        Arc::clone(&provider),
        Default::default(),
    );
    tokio::spawn(async move {
        worker.run_forever(Duration::from_secs(300)).await
    });

    ApplicationState { cqrs, journey_query, key_store }
}
```

Optional CLI binary:

```rust
// crates/journey_dynamics/src/bin/rewrap.rs
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let state = journey_dynamics::state::new_application_state().await;
    let provider = /* … same wiring … */;
    let worker = RewrapWorker::new(state.key_store, provider, Default::default());
    let stats = worker.run_once().await?;
    println!("{stats:?}");
    Ok(())
}
```

---

## Observability

Emit structured tracing events from:

| Event | Fields |
|---|---|
| `kek.rotation.write` | `kek_id`, `subject_id` (DEBUG) |
| `kek.rotation.read` | `kek_id`, `subject_id`, `stale: bool` (TRACE) |
| `kek.rotation.lazy_rewrap` | `subject_id`, `from_kek_id`, `to_kek_id`, `outcome` (INFO) |
| `kek.rotation.sweep` | `scanned`, `rewrapped`, `failures`, `duration_ms` (INFO at end of each `run_once`) |
| `kek.rotation.kms_call` | `op = encrypt|decrypt`, `latency_ms`, `result` (DEBUG) |

Add a Prometheus / OpenTelemetry-friendly counter set if/when the project
gains a metrics layer; for now `tracing` events with structured fields are
enough.

Operator dashboard query:

```sql
SELECT kek_id, COUNT(*) AS deks
FROM subject_encryption_keys
GROUP BY kek_id
ORDER BY kek_id;
```

---

## Testing Strategy

New tests live alongside the modules they cover.

### `kek.rs`

- `StaticKekProvider::from_env` parses, errors on missing `_PRIMARY`,
  errors on non-32-byte values, accepts at least one version.
- `MultiVersionKekProvider` falls through to legacy providers on
  `unwrap`, refuses to fall through on `wrap`.
- A `MockKekProvider` (test-only) that records every `wrap`/`unwrap`
  call — useful for asserting the lazy re-wrap path.

### `key_store.rs`

- Round-trip with a single-version provider (regression of today's
  behaviour).
- Two-version provider:
  1. Write under v1.
  2. Promote v2.
  3. Read — material is correct; row's `kek_id` flips to v2 (after
     awaiting the spawned re-wrap, or by disabling lazy re-wrap and
     calling `rewrap_key` explicitly).
- `rewrap_key` is idempotent — calling it twice changes the row once.
- `rewrap_key` is a no-op on a missing subject.
- CAS UPDATE: simulate concurrent re-wraps by interleaving two
  in-flight tasks; assert that the row ends in a consistent state and
  neither task errors.
- Shredding wins over re-wrap: delete the row mid-flight; the in-flight
  `rewrap_key` returns `Ok(false)` and leaves no row behind.

### `RewrapWorker`

- Empty store → `RewrapStats { scanned: 0, .. }`.
- Mixed-version store → only the stale rows are touched.
- Sweeper plus running application: spawn the worker and a stream of
  reads/writes; assert termination and final consistency.

### `journey_dynamics` integration

- Add a `tests/rotation.rs` that:
  1. Builds an in-memory `StaticKekProvider` with two versions.
  2. Drives `PersonCaptured` events.
  3. Rotates `PRIMARY`.
  4. Drives a `PersonDetailsUpdated`.
  5. Runs `RewrapWorker::run_once`.
  6. Asserts `kek_id` for all rows equals the new primary.
  7. Decrypts events from the repository and asserts plaintext is intact
     across the rotation.

### Property test (optional but recommended)

Using `proptest`: generate a random sequence of `{Write, Read, Rotate,
Sweep, Shred}` operations over a small fixed subject set; after each
sequence assert (a) every DEK that exists is unwrappable, (b) every
shredded subject's row is gone.

---

## Step-by-Step Execution Plan

Ordered so each step is independently mergeable and tested.

### Phase 1 — Crate API foundation

1. Add `kek.rs` with the `KekProvider` trait, `KekHandle`, `WrappedDek`,
   `KekError`.
2. Split `PiiCipher` into `FieldCipher` (public) and `LocalKwpKek`
   (private helper). Keep `PiiCipher` as a deprecated alias for one
   release.
3. Update tests in `cipher.rs`. No behaviour change.

### Phase 2 — Static provider & key-store rewire

4. Implement `StaticKekProvider` and `StaticKekProviderBuilder`.
   Implement `from_env`.
5. Add the migration `<ts>_kek_versioning.up.sql` /`.down.sql`.
6. Change `PostgresKeyStore::new` to take `Arc<dyn KekProvider>`. Update
   `get_or_create_key` / `get_key` / `delete_key` to read/write
   `kek_id`. Default `lazy_rewrap = false` for this phase.
7. Update `InMemoryKeyStore` to track `kek_id` per entry.
8. Update `journey_dynamics::state` to construct a one-version
   `StaticKekProvider` and pass it through. Confirm the system runs
   unchanged.

### Phase 3 — Rotation primitives

9. Add `list_stale_subjects` and `rewrap_key` to the `KeyStore` trait
   with defaults; implement on both `InMemoryKeyStore` and
   `PostgresKeyStore` (with the CAS UPDATE).
10. Add `RewrapWorker` + `RewrapWorkerOptions` + `RewrapStats`.
11. Enable `lazy_rewrap = true` in `PostgresKeyStore` and wire the
    detached spawn.
12. Add unit + integration tests for rotation. Add a property test if
    time allows.

### Phase 4 — `journey_dynamics` wiring & ops

13. Switch `state.rs` to read `JOURNEY_KEK_PRIMARY` + `JOURNEY_KEK_<id>`
    and spawn `RewrapWorker::run_forever`.
14. Add `crates/journey_dynamics/src/bin/rewrap.rs` for one-shot runs.
15. Add operator runbook: `docs/KEK_ROTATION_RUNBOOK.md` describing
    Steps 1–4 of [Rotation Flow](#rotation-flow), the verification SQL,
    and the rollback procedure.

### Phase 5 — AWS KMS provider (optional, behind a feature)

16. Add the `aws-kms` Cargo feature and `aws-sdk-kms` / `aws-config`
    dependencies (optional).
17. Implement `AwsKmsKekProvider` with envelope encryption, encryption
    context = `{"subject_id": "..."}`, a `moka::future::Cache` for
    plaintext DEKs, and an `ArcSwap<String>` for the current
    key-version ARN refreshed via a background poller.
18. Document IAM permissions required (`kms:Encrypt`, `kms:Decrypt`,
    `kms:DescribeKey`, `kms:GenerateDataKey` — see note in Future Work).
19. Add a `MockKmsClient` test that exercises the same property test
    from Phase 3 against the KMS-flavoured provider.

### Phase 6 — Stubs for other vaults

20. Add empty modules + Cargo features for `gcp-kms`, `azure-key-vault`,
    `vault-transit`. Each module exports a struct that
    `unimplemented!()`s for now. This locks in the surface so adding a
    provider later is purely additive.

Each phase ends with `cargo test --all-features` green and (where
relevant) the integration test in `journey_dynamics` green.

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Operator removes a KEK version that still has wrapped DEKs. | Medium | Catastrophic (those DEKs become unreadable; events effectively shredded). | Operator runbook with the verification SQL; refuse to start the application if `kek_id` values exist that the provider cannot resolve (fail fast on boot, before any traffic). |
| Lazy re-wrap thunders the database after a rotation. | Low | Performance spike. | `lazy_rewrap` is fire-and-forget and CAS-gated; the sweeper paces itself. Add a global semaphore (`tokio::sync::Semaphore`) bounding in-flight lazy re-wraps if needed. |
| KMS latency dominates request times. | High in KMS mode. | User-visible. | Plaintext DEK cache (TTL ~5min, bounded size). Most reads hit the cache; only the first read per subject pays the KMS round-trip. |
| Re-wrap worker dies mid-batch. | Medium | Sweep takes longer; eventual consistency unaffected. | Cursor-based pagination is idempotent; the next `run_once` resumes from `kek_id <> primary` naturally. |
| Test environment forgets to clean up `kek_id` after schema migration in a previous test. | Medium | Flaky tests. | Use the test-isolation helpers already in place for `PostgresKeyStore` tests; assert `kek_id` explicitly in each test. |
| Boot-time fail-fast check is too strict (e.g. a KMS hiccup at startup wrongly aborts the process). | Medium | Outage. | Boot-time check only enumerates `SELECT DISTINCT kek_id`; provider `by_id` returns `None` only on truly-unknown ids, not transient network errors. KMS providers return `Err` for transport problems, which the check treats as "unknown — retry, do not abort". |

---

## Future Work

- **`kms:GenerateDataKey`** — KMS providers can use this instead of
  `Encrypt` to source a DEK *and* its wrapped form in one call. Slight
  efficiency win at write-time; punt until profiling justifies it.
- **DEK rotation.** The infrastructure here (CAS-guarded rewrites of
  wrapped keys) is half of what DEK rotation needs; the other half is
  re-encrypting event payloads, which is the expensive part documented
  elsewhere. Out of scope for this work.
- **Snapshot encryption.** Independent track; this design is forward
  compatible because snapshots will use the same `KeyStore` / `KekProvider`
  surface as events.
- **Per-tenant KEKs.** If/when the platform becomes multi-tenant, the
  `KekProvider` trait could be parameterised by tenant id. The current
  trait deliberately does not foreclose this.
- **HSM-backed local KEKs via PKCS#11.** Strictly less useful than a
  managed KMS; revisit only if there is a regulatory requirement.
