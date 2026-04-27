//! [`JourneyPiiCodec`] — [`PiiEventCodec`] implementation for Journey domain events.
//!
//! Encodes the knowledge of which journey event types carry PII, where the subject
//! ID lives in each payload, which fields are sensitive, and how to reassemble event
//! payloads after encryption or redaction.
//!
//! # Encrypted event schemas
//!
//! ## `PersonCaptured` (encrypted form)
//!
//! ```json
//! {
//!   "PersonCaptured": {
//!     "person_ref":    "<string>",
//!     "subject_id":   "<uuid>",
//!     "encrypted_pii": "<base64 AES-256-GCM ciphertext>",
//!     "nonce":         "<base64 96-bit nonce>"
//!   }
//! }
//! ```
//!
//! The `encrypted_pii` field contains the AES-256-GCM encryption of:
//! ```json
//! { "name": "<string>", "email": "<string>", "phone": "<string|null>" }
//! ```
//!
//! ## `PersonDetailsUpdated` (encrypted form)
//!
//! ```json
//! {
//!   "PersonDetailsUpdated": {
//!     "person_ref":      "<string>",
//!     "subject_id":     "<uuid>",
//!     "encrypted_data":  "<base64 AES-256-GCM ciphertext>",
//!     "nonce":           "<base64 96-bit nonce>"
//!   }
//! }
//! ```
//!
//! The `encrypted_data` field contains the AES-256-GCM encryption of the raw
//! `data` JSON value from the original event.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::{EncryptedPiiExtract, EncryptedPiiSentinel, PiiEventCodec, PiiFields};
use serde_json::Value;
use uuid::Uuid;

// ── Event-type constants ───────────────────────────────────────────────────────

const PERSON_CAPTURED: &str = "PersonCaptured";
const PERSON_DETAILS_UPDATED: &str = "PersonDetailsUpdated";

// Outer payload keys (serde external enum tagging)
const PC_KEY: &str = "PersonCaptured";
const PD_KEY: &str = "PersonDetailsUpdated";

// ── JourneyPiiCodec ───────────────────────────────────────────────────────────

/// [`PiiEventCodec`] for the Journey aggregate.
///
/// Handles `PersonCaptured` and `PersonDetailsUpdated` events. All other event
/// types are treated as non-PII and passed through unchanged.
pub struct JourneyPiiCodec;

impl PiiEventCodec for JourneyPiiCodec {
    fn classify(&self, event: &SerializedEvent) -> Option<PiiFields> {
        match event.event_type.as_str() {
            PERSON_CAPTURED => classify_person_captured(event),
            PERSON_DETAILS_UPDATED => classify_person_details_updated(event),
            _ => None,
        }
    }

    fn extract_encrypted(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
        match event.event_type.as_str() {
            PERSON_CAPTURED => extract_person_captured(event),
            PERSON_DETAILS_UPDATED => extract_person_details_updated(event),
            _ => None,
        }
    }

    fn reconstruct(
        &self,
        event: &SerializedEvent,
        plaintext_pii: &Value,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        match event.event_type.as_str() {
            PERSON_CAPTURED => {
                let person_ref = event.payload[PC_KEY]["person_ref"].clone();
                let subject_id = event.payload[PC_KEY]["subject_id"].clone();
                Ok(serde_json::json!({
                    "PersonCaptured": {
                        "person_ref": person_ref,
                        "subject_id": subject_id,
                        "name":       plaintext_pii["name"],
                        "email":      plaintext_pii["email"],
                        "phone":      plaintext_pii["phone"],
                    }
                }))
            }
            PERSON_DETAILS_UPDATED => {
                let person_ref = event.payload[PD_KEY]["person_ref"].clone();
                let subject_id = event.payload[PD_KEY]["subject_id"].clone();
                Ok(serde_json::json!({
                    "PersonDetailsUpdated": {
                        "person_ref": person_ref,
                        "subject_id": subject_id,
                        "data":       plaintext_pii,
                    }
                }))
            }
            _ => Ok(event.payload.clone()),
        }
    }

    fn redact(
        &self,
        event: &SerializedEvent,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        match event.event_type.as_str() {
            PERSON_CAPTURED => {
                let person_ref = event.payload[PC_KEY]["person_ref"].clone();
                let subject_id = event.payload[PC_KEY]["subject_id"].clone();
                Ok(serde_json::json!({
                    "PersonCaptured": {
                        "person_ref": person_ref,
                        "subject_id": subject_id,
                        "name":       "[redacted]",
                        "email":      "[redacted]",
                        "phone":      null,
                    }
                }))
            }
            PERSON_DETAILS_UPDATED => {
                let person_ref = event.payload[PD_KEY]["person_ref"].clone();
                let subject_id = event.payload[PD_KEY]["subject_id"].clone();
                Ok(serde_json::json!({
                    "PersonDetailsUpdated": {
                        "person_ref": person_ref,
                        "subject_id": subject_id,
                        "data":       {},
                    }
                }))
            }
            _ => Ok(event.payload.clone()),
        }
    }
}

// ── Write-path helpers ────────────────────────────────────────────────────────

fn classify_person_captured(event: &SerializedEvent) -> Option<PiiFields> {
    // subject_id must be present. If missing (legacy event), return None so the
    // event is stored verbatim.
    let subject_id_str = event.payload[PC_KEY]["subject_id"].as_str()?.to_string();
    let subject_id = Uuid::parse_str(&subject_id_str).ok()?;

    // person_ref is not PII — keep in plaintext alongside subject_id.
    let person_ref_str = event.payload[PC_KEY]["person_ref"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let pc = event.payload[PC_KEY].as_object()?;
    let plaintext_pii = serde_json::json!({
        "name":  pc.get("name") .cloned().unwrap_or(Value::Null),
        "email": pc.get("email").cloned().unwrap_or(Value::Null),
        "phone": pc.get("phone").cloned().unwrap_or(Value::Null),
    });

    Some(PiiFields {
        subject_id,
        plaintext_pii,
        build_encrypted_payload: Box::new(
            move |EncryptedPiiSentinel {
                      ciphertext_b64,
                      nonce_b64,
                  }| {
                serde_json::json!({
                    "PersonCaptured": {
                        "person_ref":    person_ref_str,
                        "subject_id":    subject_id_str,
                        "encrypted_pii": ciphertext_b64,
                        "nonce":         nonce_b64,
                    }
                })
            },
        ),
    })
}

fn classify_person_details_updated(event: &SerializedEvent) -> Option<PiiFields> {
    let subject_id_str = event.payload[PD_KEY]["subject_id"].as_str()?.to_string();
    let subject_id = Uuid::parse_str(&subject_id_str).ok()?;

    let person_ref_str = event.payload[PD_KEY]["person_ref"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // The entire `data` field is the PII blob.
    let plaintext_pii = event.payload[PD_KEY]["data"].clone();

    Some(PiiFields {
        subject_id,
        plaintext_pii,
        build_encrypted_payload: Box::new(
            move |EncryptedPiiSentinel {
                      ciphertext_b64,
                      nonce_b64,
                  }| {
                serde_json::json!({
                    "PersonDetailsUpdated": {
                        "person_ref":     person_ref_str,
                        "subject_id":     subject_id_str,
                        "encrypted_data": ciphertext_b64,
                        "nonce":          nonce_b64,
                    }
                })
            },
        ),
    })
}

// ── Read-path helpers ─────────────────────────────────────────────────────────

fn extract_person_captured(event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
    // No sentinel → plaintext / legacy event, pass through unchanged.
    event.payload[PC_KEY].get("encrypted_pii")?;

    let subject_id = Uuid::parse_str(event.payload[PC_KEY]["subject_id"].as_str()?).ok()?;
    let ciphertext = BASE64
        .decode(event.payload[PC_KEY]["encrypted_pii"].as_str()?)
        .ok()?;
    let nonce = BASE64
        .decode(event.payload[PC_KEY]["nonce"].as_str()?)
        .ok()?;

    Some(EncryptedPiiExtract {
        subject_id,
        ciphertext,
        nonce,
    })
}

fn extract_person_details_updated(event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
    // No sentinel → plaintext / legacy event, pass through unchanged.
    event.payload[PD_KEY].get("encrypted_data")?;

    let subject_id = Uuid::parse_str(event.payload[PD_KEY]["subject_id"].as_str()?).ok()?;
    let ciphertext = BASE64
        .decode(event.payload[PD_KEY]["encrypted_data"].as_str()?)
        .ok()?;
    let nonce = BASE64
        .decode(event.payload[PD_KEY]["nonce"].as_str()?)
        .ok()?;

    Some(EncryptedPiiExtract {
        subject_id,
        ciphertext,
        nonce,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cqrs_es::persist::{PersistedEventRepository, SerializedEvent};
    use cqrs_es_crypto::{
        CryptoShreddingEventRepository, InMemoryEventRepository, InMemoryKeyStore, KeyStore,
        PiiCipher,
    };
    use uuid::Uuid;

    use crate::domain::journey::Journey;

    use super::JourneyPiiCodec;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
        let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
        let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
        let codec = Arc::new(JourneyPiiCodec);
        CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            key_store,
            cipher,
            codec,
        )
    }

    fn make_repo_with_parts() -> (
        CryptoShreddingEventRepository<InMemoryEventRepository>,
        Arc<InMemoryKeyStore>,
    ) {
        let key_store = Arc::new(InMemoryKeyStore::new());
        let cipher = PiiCipher::new(vec![0x42u8; 32]).unwrap();
        let codec = Arc::new(JourneyPiiCodec);
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            cipher,
            codec,
        );
        (repo, key_store)
    }

    fn person_captured_event(
        aggregate_id: &str,
        sequence: usize,
        subject_id: Uuid,
    ) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref": "passenger_0",
                    "subject_id": subject_id.to_string(),
                    "name":       "Alice Smith",
                    "email":      "alice@example.com",
                    "phone":      "+44-7700-900000"
                }
            }),
            serde_json::json!({}),
        )
    }

    fn person_details_updated_event(
        aggregate_id: &str,
        sequence: usize,
        subject_id: Uuid,
    ) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonDetailsUpdated": {
                    "person_ref": "passenger_0",
                    "subject_id": subject_id.to_string(),
                    "data": {
                        "passportNumber": "GB123456789",
                        "dateOfBirth":    "1990-05-15",
                        "nationality":    "GB"
                    }
                }
            }),
            serde_json::json!({}),
        )
    }

    fn modified_event(aggregate_id: &str, sequence: usize) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "JourneyModified".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "Modified": {
                    "step": "search",
                    "data": {
                        "tripType":    "round-trip",
                        "origin":      "LHR",
                        "destination": "JFK"
                    }
                }
            }),
            serde_json::json!({}),
        )
    }

    fn started_event(aggregate_id: &str, sequence: usize) -> SerializedEvent {
        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "JourneyOpened".to_string(),
            "1.0".to_string(),
            serde_json::json!({ "Started": { "id": aggregate_id } }),
            serde_json::json!({}),
        )
    }

    // ── Non-PII events ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_non_pii_events_pass_through_unchanged() {
        let repo = make_repo();
        let aggregate_id = "journey-pass-through";
        let event = started_event(aggregate_id, 1);
        let original_payload = event.payload.clone();

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw.len(), 1);
        assert_eq!(
            raw[0].payload, original_payload,
            "non-PII event payload must not be modified"
        );
    }

    #[tokio::test]
    async fn test_modified_events_always_pass_through_unmodified() {
        // Modified events carry only shared non-PII data and must never be encrypted,
        // regardless of whether a subject has been captured for the journey.
        let repo = make_repo();
        let aggregate_id = "journey-modified-plain";
        let subject_id = Uuid::new_v4();

        // Capture a person first (DEK now exists in the key store).
        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let mod_event = modified_event(aggregate_id, 2);
        let original_payload = mod_event.payload.clone();

        repo.persist::<Journey>(&[mod_event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(
            raw[1].payload, original_payload,
            "Modified event must be stored in plaintext even after a subject is captured"
        );
        assert!(
            raw[1].payload["Modified"]["data"]
                .get("encrypted_data")
                .is_none(),
            "Modified data must never contain an encrypted_data sentinel"
        );
    }

    // ── PersonCaptured — write path ───────────────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_person_captured_pii_fields() {
        let repo = make_repo();
        let aggregate_id = "journey-pc-encrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw.len(), 1);
        let pc = &raw[0].payload["PersonCaptured"];

        // PII must NOT be stored in plaintext.
        assert!(pc.get("name").is_none(), "name must not be in plaintext");
        assert!(pc.get("email").is_none(), "email must not be in plaintext");
        assert!(pc.get("phone").is_none(), "phone must not be in plaintext");

        // Encryption envelope must be present.
        assert!(
            pc.get("encrypted_pii").is_some(),
            "encrypted_pii must be present"
        );
        assert!(pc.get("nonce").is_some(), "nonce must be present");

        // Non-PII fields remain in plaintext.
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
            "subject_id must remain in plaintext"
        );
        assert_eq!(
            pc["person_ref"].as_str().unwrap(),
            "passenger_0",
            "person_ref must remain in plaintext"
        );
    }

    #[tokio::test]
    async fn test_person_captured_without_subject_id_passes_through_on_write() {
        // A PersonCaptured without a subject_id field must be stored unmodified.
        let repo = make_repo();
        let aggregate_id = "journey-legacy-pc-write";
        let legacy_payload = serde_json::json!({
            "PersonCaptured": {
                "name":  "Bob Jones",
                "email": "bob@example.com",
                "phone": null
            }
        });
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            legacy_payload.clone(),
            serde_json::json!({}),
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(
            raw[0].payload, legacy_payload,
            "PersonCaptured without subject_id must be stored unmodified"
        );
    }

    // ── PersonCaptured — read path ────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_events_decrypts_person_captured() {
        let repo = make_repo();
        let aggregate_id = "journey-pc-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(events.len(), 1);
        let pc = &events[0].payload["PersonCaptured"];

        assert_eq!(pc["name"].as_str().unwrap(), "Alice Smith");
        assert_eq!(pc["email"].as_str().unwrap(), "alice@example.com");
        assert_eq!(pc["phone"].as_str().unwrap(), "+44-7700-900000");
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str()
        );
        assert_eq!(pc["person_ref"].as_str().unwrap(), "passenger_0");
        assert!(
            pc.get("encrypted_pii").is_none(),
            "encrypted_pii must not appear after decryption"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_person_captured_when_key_deleted() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "journey-pc-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        // Simulate crypto-shredding.
        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let pc = &events[0].payload["PersonCaptured"];

        assert_eq!(pc["name"].as_str().unwrap(), "[redacted]");
        assert_eq!(pc["email"].as_str().unwrap(), "[redacted]");
        assert!(pc["phone"].is_null(), "phone must be null after shredding");
        assert_eq!(
            pc["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
            "subject_id must remain readable for audit purposes"
        );
        assert_eq!(
            pc["person_ref"].as_str().unwrap(),
            "passenger_0",
            "person_ref must remain readable after shredding"
        );
    }

    #[tokio::test]
    async fn test_plaintext_person_captured_passes_through_on_read() {
        // Legacy event without encrypted_pii — must be returned verbatim.
        let repo = make_repo();
        let aggregate_id = "journey-legacy-pc-read";
        let legacy_payload = serde_json::json!({
            "PersonCaptured": {
                "name":  "Carol White",
                "email": "carol@example.com",
                "phone": null
            }
        });

        // Inject directly into the inner store, bypassing the crypto write path.
        repo.inner()
            .persist::<Journey>(
                &[SerializedEvent::new(
                    aggregate_id.to_string(),
                    1,
                    "Journey".to_string(),
                    "PersonCaptured".to_string(),
                    "1.0".to_string(),
                    legacy_payload.clone(),
                    serde_json::json!({}),
                )],
                None,
            )
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload, legacy_payload,
            "legacy plaintext PersonCaptured must be returned unmodified"
        );
    }

    // ── PersonDetailsUpdated — write path ─────────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_person_details_updated() {
        let repo = make_repo();
        let aggregate_id = "journey-pd-encrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw.len(), 1);
        let pd = &raw[0].payload["PersonDetailsUpdated"];

        // PII must NOT be stored in plaintext.
        assert!(
            pd.get("data")
                .is_none_or(|d| d.get("passportNumber").is_none()),
            "passportNumber must not appear in plaintext"
        );

        // Encryption envelope must be present.
        assert!(
            pd.get("encrypted_data").is_some(),
            "encrypted_data must be present"
        );
        assert!(pd.get("nonce").is_some(), "nonce must be present");

        // Non-PII fields remain in plaintext.
        assert_eq!(
            pd["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str()
        );
        assert_eq!(pd["person_ref"].as_str().unwrap(), "passenger_0");
    }

    #[tokio::test]
    async fn test_person_details_updated_without_subject_id_passes_through() {
        // A PersonDetailsUpdated without subject_id must be stored unmodified.
        let repo = make_repo();
        let aggregate_id = "journey-pd-legacy-write";
        let legacy_payload = serde_json::json!({
            "PersonDetailsUpdated": {
                "person_ref": "passenger_0",
                "data": { "passportNumber": "XX999" }
            }
        });
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            legacy_payload.clone(),
            serde_json::json!({}),
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw[0].payload, legacy_payload);
    }

    // ── PersonDetailsUpdated — read path ──────────────────────────────────────

    #[tokio::test]
    async fn test_get_events_decrypts_person_details_updated() {
        let repo = make_repo();
        let aggregate_id = "journey-pd-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(events.len(), 1);
        let pd = &events[0].payload["PersonDetailsUpdated"];

        assert_eq!(
            pd["data"]["passportNumber"].as_str().unwrap(),
            "GB123456789"
        );
        assert_eq!(pd["data"]["dateOfBirth"].as_str().unwrap(), "1990-05-15");
        assert_eq!(pd["data"]["nationality"].as_str().unwrap(), "GB");
        assert_eq!(
            pd["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str()
        );
        assert_eq!(pd["person_ref"].as_str().unwrap(), "passenger_0");
        assert!(
            pd.get("encrypted_data").is_none(),
            "encrypted_data must not appear after decryption"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_person_details_updated_when_key_deleted() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "journey-pd-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let pd = &events[0].payload["PersonDetailsUpdated"];

        assert_eq!(
            pd["data"],
            serde_json::json!({}),
            "data must be empty after shredding"
        );
        assert_eq!(
            pd["subject_id"].as_str().unwrap(),
            subject_id.to_string().as_str(),
            "subject_id must remain readable for audit purposes"
        );
        assert_eq!(
            pd["person_ref"].as_str().unwrap(),
            "passenger_0",
            "person_ref must remain readable after shredding"
        );
    }

    #[tokio::test]
    async fn test_plaintext_person_details_updated_passes_through_on_read() {
        let repo = make_repo();
        let aggregate_id = "journey-pd-legacy-read";
        let plaintext_payload = serde_json::json!({
            "PersonDetailsUpdated": {
                "person_ref": "passenger_0",
                "subject_id": Uuid::new_v4().to_string(),
                "data": { "passportNumber": "GB000000001" }
            }
        });

        repo.inner()
            .persist::<Journey>(
                &[SerializedEvent::new(
                    aggregate_id.to_string(),
                    1,
                    "Journey".to_string(),
                    "PersonDetailsUpdated".to_string(),
                    "1.0".to_string(),
                    plaintext_payload.clone(),
                    serde_json::json!({}),
                )],
                None,
            )
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload, plaintext_payload,
            "legacy plaintext PersonDetailsUpdated must be returned unmodified"
        );
    }

    // ── Multi-subject scenarios ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_single_key_deletion_shreds_all_journeys_for_subject() {
        // The same subject captured in two journeys: deleting the single DEK must
        // make both journeys' PersonCaptured events unreadable.
        let (repo, key_store) = make_repo_with_parts();
        let subject_id = Uuid::new_v4();
        let journey_a = "journey-xj-a";
        let journey_b = "journey-xj-b";

        repo.persist::<Journey>(&[person_captured_event(journey_a, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(&[person_captured_event(journey_b, 1, subject_id)], None)
            .await
            .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events_a = repo.get_events::<Journey>(journey_a).await.unwrap();
        let events_b = repo.get_events::<Journey>(journey_b).await.unwrap();

        assert_eq!(
            events_a[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]",
            "journey A PersonCaptured must be redacted after key deletion"
        );
        assert_eq!(
            events_b[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]",
            "journey B PersonCaptured must be redacted after key deletion"
        );
    }

    #[tokio::test]
    async fn test_two_subjects_in_one_journey_shredded_independently() {
        // Two subjects captured in the same journey: shredding subject A must leave
        // subject B's events fully readable and must not affect shared Modified events.
        let (repo, key_store) = make_repo_with_parts();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();
        let aggregate_id = "journey-two-subjects";

        let pc_a = person_captured_event(aggregate_id, 1, subject_a);
        let pc_b = SerializedEvent::new(
            aggregate_id.to_string(),
            2,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref": "passenger_1",
                    "subject_id": subject_b.to_string(),
                    "name":       "Bob Jones",
                    "email":      "bob@example.com",
                    "phone":      null
                }
            }),
            serde_json::json!({}),
        );
        let mod_ev = modified_event(aggregate_id, 3);
        let original_mod_payload = mod_ev.payload.clone();

        repo.persist::<Journey>(&[pc_a, pc_b, mod_ev], None)
            .await
            .unwrap();

        // Shred only subject A.
        key_store.delete_key(&subject_a).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();

        // Subject A's PersonCaptured must be redacted.
        let ev_a = events
            .iter()
            .find(|e| e.payload["PersonCaptured"]["person_ref"].as_str() == Some("passenger_0"))
            .unwrap();
        assert_eq!(
            ev_a.payload["PersonCaptured"]["name"].as_str().unwrap(),
            "[redacted]"
        );

        // Subject B's PersonCaptured must still be readable.
        let ev_b = events
            .iter()
            .find(|e| e.payload["PersonCaptured"]["person_ref"].as_str() == Some("passenger_1"))
            .unwrap();
        assert_eq!(
            ev_b.payload["PersonCaptured"]["name"].as_str().unwrap(),
            "Bob Jones"
        );

        // The Modified event must be completely untouched.
        let mod_event = events
            .iter()
            .find(|e| e.event_type == "JourneyModified")
            .unwrap();
        assert_eq!(mod_event.payload, original_mod_payload);
    }

    // ── get_last_events ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_last_events_decrypts_correctly() {
        let repo = make_repo();
        let aggregate_id = "journey-last-events";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();
        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 2, subject_id)],
            None,
        )
        .await
        .unwrap();

        // Fetch only the last event (the PersonDetailsUpdated).
        let events = repo
            .get_last_events::<Journey>(aggregate_id, 1)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "PersonDetailsUpdated");
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"]["passportNumber"]
                .as_str()
                .unwrap(),
            "GB123456789"
        );
    }

    // ── stream_events ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_stream_events_returns_decrypted_domain_events() {
        let repo = make_repo();
        let aggregate_id = "journey-stream";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let mut stream = repo.stream_events::<Journey>(aggregate_id).await.unwrap();
        let envelope = stream
            .next::<Journey>(&[])
            .await
            .expect("stream must yield an event")
            .expect("event must deserialise without error");

        match envelope.payload {
            crate::domain::events::JourneyEvent::PersonCaptured {
                name,
                email,
                phone,
                subject_id: sid,
                ..
            } => {
                assert_eq!(name, "Alice Smith");
                assert_eq!(email, "alice@example.com");
                assert_eq!(phone.as_deref(), Some("+44-7700-900000"));
                assert_eq!(sid, subject_id);
            }
            other => panic!("unexpected event variant: {other:?}"),
        }
    }

    // ── AAD binding ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_aad_binds_person_captured_ciphertext_to_event_position() {
        // Two PersonCaptured events at different sequence numbers must produce
        // different ciphertexts even for identical plaintext, due to both the
        // different AAD and a fresh nonce per call.
        let repo = make_repo();
        let aggregate_id = "journey-aad-pc";
        let subject_id = Uuid::new_v4();

        let ev1 = person_captured_event(aggregate_id, 1, subject_id);
        let ev2 = person_captured_event(aggregate_id, 2, subject_id);
        repo.persist::<Journey>(&[ev1, ev2], None).await.unwrap();

        let raw = repo.inner().all_events();
        let ct1 = raw[0].payload["PersonCaptured"]["encrypted_pii"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["PersonCaptured"]["encrypted_pii"]
            .as_str()
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "identical plaintext at different sequence numbers must produce different ciphertexts"
        );
    }

    #[tokio::test]
    async fn test_aad_binds_person_details_ciphertext_to_event_position() {
        let repo = make_repo();
        let aggregate_id = "journey-aad-pd";
        let subject_id = Uuid::new_v4();

        let ev1 = person_details_updated_event(aggregate_id, 1, subject_id);
        let ev2 = person_details_updated_event(aggregate_id, 2, subject_id);
        repo.persist::<Journey>(&[ev1, ev2], None).await.unwrap();

        let raw = repo.inner().all_events();
        let ct1 = raw[0].payload["PersonDetailsUpdated"]["encrypted_data"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["PersonDetailsUpdated"]["encrypted_data"]
            .as_str()
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "identical plaintext at different positions must produce different ciphertexts"
        );
    }
}
