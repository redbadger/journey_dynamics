# cqrs-es-crypto

Transparent PII encryption and GDPR crypto-shredding for [`cqrs-es`](https://crates.io/crates/cqrs-es).

Wraps any [`PersistedEventRepository`] with a crypto layer that encrypts designated
PII fields on the write path and decrypts them on the read path — or redacts them
when a subject's key has been deleted.

## How it works

### Key hierarchy

```
KEK (Key Encryption Key)          — one per deployment, loaded from environment
 └── DEK (Data Encryption Key)    — one per data subject, stored wrapped in Postgres
      └── event PII fields        — encrypted with AES-256-GCM per event
```

- **KEK** — A 256-bit key held only in application memory (e.g. loaded from an
  environment variable). Never stored. Used to wrap and unwrap DEKs.
- **DEK** — A fresh 256-bit key generated for each data subject. Stored AES-256-KWP
  wrapped (RFC 5649) in the `subject_encryption_keys` table. Deleting this row
  permanently destroys the ability to recover any PII for that subject —
  **this is crypto-shredding**.
- **AAD** — Every encryption uses `"<aggregate_id>:<sequence>"` as additional
  authenticated data, binding each ciphertext to its event position and preventing
  transplant attacks.

### Write path

```
persist(events)
  └── for each event
        ├── PiiEventCodec::classify → None   → store verbatim
        └── PiiEventCodec::classify → Some(PiiFields)
              ├── KeyStore::get_or_create_key(subject_id)
              ├── AES-256-GCM encrypt(plaintext_pii, aad)
              └── PiiFields::build_encrypted_payload(sentinel) → store
```

### Read path

```
get_events / get_last_events / stream_events
  └── for each stored event
        ├── PiiEventCodec::extract_encrypted → None        → return verbatim
        └── PiiEventCodec::extract_encrypted → Some(extract)
              ├── KeyStore::get_key(subject_id)
              │     ├── Some(dek) → AES-256-GCM decrypt → PiiEventCodec::reconstruct
              │     └── None      → PiiEventCodec::redact   (subject forgotten)
              └── return updated event
```

## Usage

### 1. Implement `PiiEventCodec`

`PiiEventCodec` is the only domain-specific piece you need to provide. It tells
the repository which event types carry PII, how to extract it on the write path,
and how to reconstruct or redact it on the read path.

```rust
use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::{
    EncryptedPiiExtract, EncryptedPiiSentinel, PiiEventCodec, PiiFields,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;
use uuid::Uuid;

pub struct MyCodec;

impl PiiEventCodec for MyCodec {
    /// Write path — called on the unencrypted event before it is persisted.
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields> {
        if event.event_type != "UserRegistered" {
            return None;
        }
        let subject_id = Uuid::parse_str(
            event.payload["UserRegistered"]["user_id"].as_str()?
        ).ok()?;
        let user_id_str = subject_id.to_string();

        let plaintext_pii = serde_json::json!({
            "email": event.payload["UserRegistered"]["email"],
            "name":  event.payload["UserRegistered"]["name"],
        });

        Some(PiiFields {
            subject_id,
            plaintext_pii,
            build_encrypted_payload: Box::new(move |EncryptedPiiSentinel { ciphertext_b64, nonce_b64 }| {
                serde_json::json!({
                    "UserRegistered": {
                        "user_id":       user_id_str,
                        "encrypted_pii": ciphertext_b64,
                        "nonce":         nonce_b64,
                    }
                })
            }),
        })
    }

    /// Read path — extract encrypted PII metadata from a stored event.
    /// Return None for non-PII events, or events without encryption sentinels
    /// (legacy plaintext events are passed through unchanged).
    fn extract_encrypted(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
        if event.event_type != "UserRegistered" {
            return None;
        }
        // No sentinel → legacy plaintext event, pass through.
        event.payload["UserRegistered"].get("encrypted_pii")?;

        let subject_id = Uuid::parse_str(
            event.payload["UserRegistered"]["user_id"].as_str()?
        ).ok()?;
        let ciphertext = BASE64.decode(
            event.payload["UserRegistered"]["encrypted_pii"].as_str()?
        ).ok()?;
        let nonce = BASE64.decode(
            event.payload["UserRegistered"]["nonce"].as_str()?
        ).ok()?;

        Some(EncryptedPiiExtract { subject_id, ciphertext, nonce })
    }

    /// Read path — rebuild the decrypted event payload.
    /// `event` is the stored (encrypted) form; use it to recover plaintext fields
    /// like IDs that are kept in the clear.
    fn reconstruct(
        &self,
        event: &SerializedEvent,
        plaintext_pii: &Value,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let user_id = event.payload["UserRegistered"]["user_id"].clone();
        Ok(serde_json::json!({
            "UserRegistered": {
                "user_id": user_id,
                "email":   plaintext_pii["email"],
                "name":    plaintext_pii["name"],
            }
        }))
    }

    /// Read path — rebuild with redacted placeholders when the DEK is gone.
    fn redact(
        &self,
        event: &SerializedEvent,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let user_id = event.payload["UserRegistered"]["user_id"].clone();
        Ok(serde_json::json!({
            "UserRegistered": {
                "user_id": user_id,
                "email":   "[redacted]",
                "name":    "[redacted]",
            }
        }))
    }
}
```

### 2. Build the repository

```rust
use std::sync::Arc;
use cqrs_es_crypto::{CryptoShreddingEventRepository, PiiCipher, PostgresKeyStore};

// Load KEK from environment (generate with: openssl rand -base64 32)
let kek_b64 = std::env::var("APP_KEK").expect("APP_KEK must be set");
let kek = base64::engine::general_purpose::STANDARD
    .decode(kek_b64.trim())
    .expect("APP_KEK must be valid base64");

// The key store uses its own PiiCipher instance for wrapping/unwrapping DEKs.
let key_store = Arc::new(PostgresKeyStore::new(
    pool.clone(),
    PiiCipher::new(kek.clone()).expect("KEK must be 32 bytes"),
));

// The repository uses its own PiiCipher instance for field encryption.
let cipher = PiiCipher::new(kek).expect("KEK must be 32 bytes");
let codec  = Arc::new(MyCodec);

let inner = postgres_es::PostgresEventRepository::new(pool);
let repo  = CryptoShreddingEventRepository::new(inner, key_store, cipher, codec);
```

### 3. Crypto-shred a subject

```rust
// Deleting the DEK makes all encrypted events for this subject permanently
// unreadable. Subsequent reads will return redacted payloads.
key_store.delete_key(&subject_id).await?;
```

## Required database schema

The `PostgresKeyStore` expects a `subject_encryption_keys` table. Add this to
your migrations:

```sql
CREATE TABLE subject_encryption_keys
(
    key_id      UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id  UUID      NOT NULL UNIQUE,
    wrapped_key BYTEA     NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_subject_keys_subject_id
    ON subject_encryption_keys (subject_id);
```

## Testing

Use `InMemoryKeyStore` and `InMemoryEventRepository` (available via the
`testing` Cargo feature) to test your codec and application logic without a
database:

```toml
[dev-dependencies]
cqrs-es-crypto = { path = "...", features = ["testing"] }
```

```rust
use std::sync::Arc;
use cqrs_es_crypto::{
    CryptoShreddingEventRepository, InMemoryEventRepository,
    InMemoryKeyStore, KeyStore, PiiCipher,
};

fn make_test_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
    let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
    let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
    let codec  = Arc::new(MyCodec);
    CryptoShreddingEventRepository::new(
        InMemoryEventRepository::default(),
        key_store,
        cipher,
        codec,
    )
}
```

## Crate structure

| Module | Contents |
|--------|----------|
| `cipher` | `PiiCipher` — AES-256-GCM field encryption and AES-256-KWP key wrapping |
| `key_store` | `KeyStore` trait, `InMemoryKeyStore`, `PostgresKeyStore` |
| `repository` | `PiiEventCodec` trait, `CryptoShreddingEventRepository`, `InMemoryEventRepository` |

## Security notes

- The KEK must be exactly 32 bytes and should be loaded from a secrets manager
  or environment variable — never hardcoded or committed to source control.
- `PiiCipher` uses `zeroize` to erase key material from memory when dropped.
- Each encryption call generates a fresh random 96-bit nonce; nonce reuse is
  not possible under normal operation.
- The AAD scheme (`"<aggregate_id>:<sequence>"`) ensures a ciphertext extracted
  from one event cannot be inserted into a different event and pass authentication.
- `PostgresKeyStore::get_or_create_key` uses `INSERT … ON CONFLICT DO NOTHING`
  to handle concurrent DEK creation races safely.