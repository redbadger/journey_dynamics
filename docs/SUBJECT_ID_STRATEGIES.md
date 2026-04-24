# Subject ID Strategies

`subject_id` is the stable UUID that ties a person's PII to their Data Encryption Key (DEK).
It is stored in plaintext in the event store and in `journey_person`, and a single
`DELETE /subjects/{subject_id}` call shreds that person's PII across every journey they
appear in.

This document discusses how callers should mint or resolve `subject_id` values, with
particular attention to the "additional passenger" case where no existing identity-system
UUID is available.

---

## The simplest case — authenticated users

When the person being captured is your own authenticated user, use whatever stable ID your
identity system already assigns (Cognito `sub`, Auth0 `user_id`, your own database primary
key formatted as a UUID, etc.). Reuse it on every booking so that a single erasure request
covers all of their journeys.

---

## The additional-passenger problem

For passengers who are **not** your logged-in user (e.g. `"passenger_1"`, `"passenger_2"`),
no identity-system UUID exists at booking time. You have three broad options.

---

### Option A — Identity-mapping table (recommended)

Maintain a small `subjects` table in your own service:

```sql
CREATE TABLE subjects (
    subject_id       UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    normalised_email TEXT UNIQUE NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Resolution at booking time:

```
resolve_or_create(email) -> Uuid
  1. normalised = lower(trim(email))
  2. SELECT subject_id FROM subjects WHERE normalised_email = normalised
  3. if missing: INSERT and return a new random UUID
```

**Why this is the safest choice:**

- Email changes are handled by `UPDATE subjects SET normalised_email = $new WHERE subject_id = $id`.
  All historical bookings remain correctly linked without any re-keying.
- The mapping table is itself PII-sensitive (it links an email to a stable ID) and can be
  encrypted or placed under access controls independently of `journey_dynamics`.
- GDPR support staff can initiate erasure by looking up `subject_id` from the table and
  calling the existing `DELETE /subjects/{subject_id}` endpoint.
- No deterministic relationship between email and `subject_id` exists in the `journey_dynamics`
  event store, so a compromised database leaks nothing about which subjects are present.

---

### Option B — UUID v5 derived from email

UUID v5 is a name-based UUID: `Uuid::new_v5(&NAMESPACE, email.as_bytes())`. The same input
always produces the same UUID, giving you a stable identifier without a mapping table.

This is a reasonable pragmatic shortcut, but carries several trade-offs:

#### Trade-off 1 — Email rotation breaks erasure

`V5(NS, "alice@old.com")` ≠ `V5(NS, "alice@new.com")`. If Alice books under her old address,
changes email, then requests erasure using her new address, the derived `subject_id` will not
match any stored DEK. Her old PII sits in the event store indefinitely.

Mitigation: add a `DELETE /subjects/by-email` endpoint (see below) that resolves backwards
through `journey_person.email` rather than re-deriving the ID.

#### Trade-off 2 — Correlation risk if the namespace is not secret

UUID v5 uses SHA-1, which is not keyed. If your namespace UUID is public or discoverable,
anyone with a list of candidate email addresses can compute the corresponding `subject_id`
values and confirm which appear in your event store or `journey_person` table — even after
shredding, because `subject_id` is **not** nulled by `SubjectForgotten`.

Mitigation: treat the namespace UUID like `JOURNEY_KEK` — load it from a secrets manager
rather than embedding it in source code.

Alternatively, use a keyed HMAC-SHA-256 over the email and format the first 16 bytes as a
UUID (setting the version and variant bits appropriately). This gives the same determinism
as V5 but is cryptographically keyed.

#### Trade-off 3 — Normalisation lock-in

Whatever normalisation rule you apply (`lowercase`, strip `+`-tags, Unicode NFC, etc.) becomes
a permanent part of your identity scheme. Changing it later silently splits subjects: old
events refer to the un-normalised form, new events to the normalised form.

Define the rule once, document it, and enforce it in a single helper function:

```rust
/// Derive a `subject_id` from an email address.
///
/// Uses HMAC-SHA-256 under a secret namespace key so that the mapping cannot
/// be reversed or enumerated without the key.
///
/// Normalisation: lowercase + Unicode NFC trim only. `+`-tags are NOT stripped
/// because they are not universally equivalent across mail providers.
pub fn subject_id_from_email(namespace_key: &[u8; 32], email: &str) -> Uuid {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let normalised = email.trim().to_lowercase();
    let mut mac = Hmac::<Sha256>::new_from_slice(namespace_key)
        .expect("HMAC accepts any key length");
    mac.update(normalised.as_bytes());
    let result = mac.finalize().into_bytes();
    // Take the first 16 bytes and format as a UUID with version 5 / variant 1 bits.
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&result[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50; // version 5
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1
    Uuid::from_bytes(bytes)
}
```

#### Trade-off 4 — Silent cross-journey subject merging

Two bookings for the same email address will automatically share a `subject_id`. This is
usually desirable, but consider the case where `"family@example.com"` is a shared address used
by multiple family members across different bookings. All their data would be merged under one
subject and erased together.

---

### Option C — Fresh random UUID per booking slot (simplest, weakest erasure)

Generate `Uuid::new_v4()` at booking time, store it alongside the booking record, and pass it
to `CapturePerson`. This is what the example code does.

The downside: the same physical person across two separate bookings gets two unrelated
`subject_id` values. An erasure request deletes only the DEK for the specific subject_id you
supply; PII from the other booking survives.

This is acceptable if:
- Each booking journey is truly independent and no cross-journey erasure is required, **or**
- You maintain a record of all `(person_email, subject_id)` pairs in your own system and issue
  a `DELETE /subjects/{subject_id}` for each one when a deletion request arrives.

---

## Supporting erasure-by-email

Regardless of which option you choose for minting subject IDs, it is worth adding a
`DELETE /subjects/by-email` endpoint to `journey_dynamics` so that support staff can honour
erasure requests without needing to know the caller's subject-ID derivation scheme:

```
DELETE /subjects/by-email
  Body: { "email": "alice@example.com" }

  1. SELECT DISTINCT subject_id
       FROM journey_person
      WHERE lower(email) = lower($1)
        AND NOT forgotten
  2. For each subject_id: run the existing shredding flow
  3. Return 204 No Content
```

This works with all three options above and is robust against email changes provided the
email stored in `journey_person` reflects the address in use at booking time.

---

## Comparison summary

| | Option A (mapping table) | Option B (V5 / HMAC) | Option C (random per slot) |
|---|---|---|---|
| **Cross-journey erasure** | ✅ automatic | ✅ automatic (if same email) | ⚠️ only if caller tracks all IDs |
| **Handles email changes** | ✅ via UPDATE | ❌ breaks linkage | n/a |
| **Correlation resistance** | ✅ strong | ⚠️ needs secret namespace | ✅ strong |
| **Extra infrastructure** | mapping table | secret namespace value | none |
| **Implementation complexity** | low | very low | trivial |
| **GDPR support UX** | easy (look up by email) | needs by-email endpoint | needs by-email endpoint or manual tracking |

For most production deployments **Option A** is the right choice. **Option B** is a sensible
MVP shortcut if you use a secret namespace and accept the email-change limitation. **Option C**
is fine for exploratory or single-booking contexts where cross-journey erasure is not required.