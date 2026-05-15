# KEK Rotation Runbook

| | |
|---|---|
| **Applies to** | `journey_dynamics` service |
| **Related design** | [KEK_ROTATION_DESIGN.md](./KEK_ROTATION_DESIGN.md) |
| **Estimated time** | 15 min hands-on; hours–days for the sweep to complete depending on data volume |

---

## Overview

Every DEK (Data Encryption Key) stored in `subject_encryption_keys` is wrapped
(encrypted) by a KEK (Key Encryption Key).  Rotating the KEK means:

1. Creating a new KEK version alongside the old.
2. Promoting the new version so all *new* DEKs are wrapped under it.
3. Re-wrapping all *existing* DEKs without downtime.
4. Retiring the old version once no rows reference it.

The application handles steps 2 and 3 automatically via lazy re-wrap (on every
read) and the embedded background sweeper.  The operator only needs to manage
the environment variables (steps 1 and 4) and verify progress (step 3).

---

## Environment-variable schemas

### Multi-version schema (used during and after rotation)

```text
JOURNEY_KEK_PRIMARY=v2           # which version to use for new DEKs
JOURNEY_KEK_v1=<base64-32-bytes> # old version — must stay until all DEKs are re-wrapped
JOURNEY_KEK_v2=<base64-32-bytes> # new version
```

The application reads all variables matching `JOURNEY_KEK_<id>` at startup and
uses the one identified by `JOURNEY_KEK_PRIMARY` for new wraps.  All listed
versions can be used for unwrapping.

### Legacy single-variable schema (backwards-compatible)

```text
JOURNEY_KEK=<base64-32-bytes>
```

Existing deployments that set only `JOURNEY_KEK` continue to work unchanged —
the application automatically maps this to the id `legacy:v1`.

### Generating a new KEK

```bash
openssl rand -base64 32
```

The output is exactly 32 bytes of random data, base64-encoded.  Use it as the
value of the new version variable.

---

## Pre-flight check

Before starting, verify the current state of the database:

```sql
SELECT kek_id, COUNT(*) AS deks
FROM subject_encryption_keys
GROUP BY kek_id
ORDER BY kek_id;
```

Note the existing `kek_id` values.  You will query this again in step 3 to
confirm that re-wrapping is complete.

Also confirm that the application can start correctly with the current
configuration by checking that it serves traffic and that no `kek.rotation`
errors appear in the logs.

---

## Step 1 — Introduce the new KEK version

**Goal:** make the new key available to all replicas without changing behaviour.

Generate a new 32-byte key:

```bash
openssl rand -base64 32
# example output: dGhpcyBpcyBub3QgYSByZWFsIGtleQ==
```

Add it to your secrets manager / deployment configuration alongside the
existing key, **without** changing `JOURNEY_KEK_PRIMARY`:

```text
JOURNEY_KEK_PRIMARY=v1           # unchanged
JOURNEY_KEK_v1=<existing-base64> # unchanged
JOURNEY_KEK_v2=<new-base64>      # added
```

Roll out this configuration change to all replicas.  The application restarts
will pick up `v2` but will not use it for new DEKs yet.

**Verify** in the logs:

```
StaticKekProvider: known kek_ids = ["v1", "v2"], primary = "v1"
```

At this point behaviour is unchanged.  No DEKs are wrapped under `v2` yet.

---

## Step 2 — Promote the new version to primary

**Goal:** new DEKs are wrapped under `v2`; existing DEKs still unwrap from `v1`.

Change `JOURNEY_KEK_PRIMARY`:

```text
JOURNEY_KEK_PRIMARY=v2           # changed
JOURNEY_KEK_v1=<existing-base64> # must remain — still needed to unwrap old rows
JOURNEY_KEK_v2=<new-base64>      # unchanged
```

Roll out to all replicas.

After rollout, new `subject_encryption_keys` rows will have `kek_id = 'v2'`.
Existing rows still have `kek_id = 'v1'` and continue to unwrap correctly.
The embedded background sweeper will begin re-wrapping stale rows.

The lazy re-wrap path in `PostgresKeyStore::get_key` also fires a background
re-wrap task on every read of a stale subject — so active subjects converge
quickly without waiting for the full sweeper cycle.

---

## Step 3 — Monitor re-wrap progress

Query the database periodically to track how many rows remain under each
KEK version:

```sql
SELECT kek_id, COUNT(*) AS deks
FROM subject_encryption_keys
GROUP BY kek_id
ORDER BY kek_id;
```

While the sweep is running you will see something like:

```
 kek_id  | deks
---------+------
 v1      |  347   ← decreasing
 v2      |  153   ← increasing
```

The sweep is complete when `v1` count reaches zero:

```
 kek_id  | deks
---------+------
 v2      |  500
```

### Forcing a one-shot sweep

The background sweeper runs automatically every 5 minutes.  To trigger an
immediate full sweep (e.g. during a maintenance window), run the CLI binary:

```bash
cargo run --bin rewrap
# or, in production:
# DATABASE_URL=... JOURNEY_KEK_PRIMARY=v2 JOURNEY_KEK_v1=... JOURNEY_KEK_v2=... ./rewrap
```

The binary exits 0 on a clean sweep and 1 if any subject failed.  It is safe
to run while the server is live — the CAS `UPDATE … WHERE kek_id = $old`
makes concurrent re-wraps idempotent.

---

## Step 4 — Retire the old version

**Only proceed once the step 3 query shows zero rows for `v1`.**

Wait an additional safety margin (e.g. 24 hours) to allow for any in-flight
reads that may have observed a stale row and are about to re-wrap it.

Remove the old version from configuration:

```text
JOURNEY_KEK_PRIMARY=v2      # unchanged
JOURNEY_KEK_v2=<new-base64> # unchanged
# JOURNEY_KEK_v1 — deleted
```

Roll out.  The application will now refuse to unwrap any row whose `kek_id`
is `v1`.  Because no such rows exist in the database, this is safe.

For KMS-backed KEKs: schedule key deletion at the vault (e.g.
`aws kms schedule-key-deletion`) after removing the variable, so the raw key
material is cryptographically destroyed.

---

## Rollback procedure

If you need to revert a rotation at any point before step 4 (i.e. while both
versions are still configured):

1. Change `JOURNEY_KEK_PRIMARY` back to `v1` and roll out.
2. The sweeper and lazy re-wrap will re-wrap any `v2` rows back to `v1`
   automatically.  Monitor with the step 3 SQL until `v2` count reaches zero.
3. Remove `JOURNEY_KEK_v2` from configuration.

**Never remove a KEK version while any rows in `subject_encryption_keys`
reference it.**  Doing so will make those DEKs permanently unreadable —
effectively shredding the affected subjects without their consent.  The pre-flight
check SQL makes it easy to verify that no rows reference a version before
removing it.

---

## Safety notes

| Rule | Reason |
|---|---|
| **Never remove a KEK version that still has rows.** | The wrapped DEKs become permanently unreadable. |
| **Keep both versions active throughout the sweep.** | Replicas may still be serving reads of `v1` rows. |
| **Roll out step 1 before step 2.** | If a replica that doesn't know `v2` tries to unwrap a `v2` row, it will return a `KekError::UnknownVersion` and the request will fail. |
| **Use the CLI binary during maintenance windows for large data sets.** | The embedded sweeper is gentle (100 ms inter-batch pause); the CLI binary can be run with custom `RewrapWorkerOptions` for faster one-off sweeps. |
| **Back up before the first rotation.** | Standard practice for any key-management operation. A backup lets you restore if something goes wrong outside the application layer (e.g. corrupted vault state). |

---

## Troubleshooting

### `KekError::UnknownVersion` in logs after promoting `v2`

A replica is receiving a request for a `v2`-wrapped row but does not have
`JOURNEY_KEK_v2` in its environment.  Step 1 was not rolled out to all replicas
before step 2.  Re-add `JOURNEY_KEK_v2` to all replicas and redeploy.

### Sweep making no progress (`scanned` stays at the same value)

The `rewrap` binary or the embedded sweeper is failing to re-wrap rows.
Check the application logs for `kek.rotation.sweep` or `kek.rotation.lazy_rewrap`
warnings.  Common causes:

- The provider cannot unwrap the old version (e.g. `JOURNEY_KEK_v1` is missing
  or incorrect).
- Database connectivity issues (`KeyStoreError::Database`).

### Large number of subjects — sweep is slow

Run the CLI binary with a tighter batch pause:

```rust
// Adjust RewrapWorkerOptions in rewrap.rs if needed:
RewrapWorkerOptions {
    batch_size: 500,
    max_concurrency: 16,
    batch_pause: Duration::ZERO,
    ..Default::default()
}
```

Or run multiple instances of the CLI in parallel — the CAS UPDATE guarantees
only one wins per row, so there is no correctness risk.
