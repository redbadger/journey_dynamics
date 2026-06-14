//! PII codec for the Journey aggregate — hand-written implementation.
//!
//! [`JourneyPiiCodec`] implements [`PiiEventCodec`] directly.  It is the
//! single source of truth for which fields are PII, how redaction sentinels
//! are expressed, and how multi-partition encryption is routed.
//!
//! # Event schemas
//!
//! ## `PersonCaptured` (encrypted form)
//!
//! ```json
//! {
//!   "PersonCaptured": {
//!     "person_ref":          "<string>",
//!     "subject_id":          "<uuid>",
//!     "subjects":            ["<uuid>"],
//!     "encrypted_partitions": [{ "subject_id": "…", "label": "default",
//!                                "nonce": "…", "ciphertext": "…" }]
//!   }
//! }
//! ```
//!
//! Partition payload: `{ "name": "…", "email": "…", "phone": "…|null" }`
//!
//! ## `PersonDetailsUpdated` (encrypted form)
//!
//! ```json
//! {
//!   "PersonDetailsUpdated": {
//!     "person_ref":          "<string>",
//!     "subject_id":          "<uuid>",
//!     "subjects":            ["<uuid>"],
//!     "encrypted_partitions": [{ "subject_id": "…", "label": "default",
//!                                "nonce": "…", "ciphertext": "…" }]
//!   }
//! }
//! ```
//!
//! Partition payload: the raw `data` JSON value.
//!
//! ## `AttributesSet` (encrypted form)
//!
//! ```json
//! {
//!   "AttributesSet": {
//!     "plaintext": { "<path>": <value>, … },
//!     "secret_partitions": [
//!       { "person_ref": "<ref>", "subject_id": "<uuid>", "changes": {} },
//!       …
//!     ],
//!     "subjects":            ["<uuid>", …],
//!     "encrypted_partitions": [
//!       { "subject_id": "…", "label": "<person_ref>", "nonce": "…", "ciphertext": "…" },
//!       …
//!     ]
//!   }
//! }
//! ```
//!
//! Each partition's payload is the JSON serialisation of the corresponding
//! `changes: BTreeMap<AttributePath, Value>`.  The label equals `person_ref`,
//! allowing `reconstruct` and `redact_partitions` to route decrypted bytes
//! back to the correct `SecretPartitionData` entry.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::{
    DecryptedPartition, EncryptedPiiExtract, PiiCodecError, PiiEventCodec, SecretPartition,
};
use serde_json::{Value, json};
use uuid::Uuid;

/// The PII codec for [`JourneyEvent`](crate::domain::events::JourneyEvent).
///
/// Handles three encrypted event types:
/// - `PersonCaptured` — single partition, label `"default"`.
/// - `PersonDetailsUpdated` — single partition, label `"default"`.
/// - `AttributesSet` — one partition per `SecretPartitionData` entry,
///   label = `person_ref`.
pub struct JourneyPiiCodec;

impl PiiEventCodec for JourneyPiiCodec {
    // ── Write path ────────────────────────────────────────────────────────────

    fn extract_partitions(
        &self,
        event: &mut SerializedEvent,
    ) -> Result<Vec<SecretPartition>, PiiCodecError> {
        match event.event_type.as_str() {
            "PersonCaptured" => {
                let key = "PersonCaptured";
                let Some(subject_id) = event.payload[key]["subject_id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    return Ok(vec![]);
                };

                let pii = {
                    let Some(inner) = event.payload[key].as_object() else {
                        return Ok(vec![]);
                    };
                    json!({
                        "name":  inner.get("name").cloned().unwrap_or(Value::Null),
                        "email": inner.get("email").cloned().unwrap_or(Value::Null),
                        "phone": inner.get("phone").cloned().unwrap_or(Value::Null),
                    })
                };

                if let Some(obj) = event.payload[key].as_object_mut() {
                    obj.remove("name");
                    obj.remove("email");
                    obj.remove("phone");
                }

                Ok(vec![SecretPartition {
                    subject_id,
                    label: "default".to_string(),
                    payload: serde_json::to_vec(&pii)?,
                }])
            }

            "PersonDetailsUpdated" => {
                let key = "PersonDetailsUpdated";
                let Some(subject_id) = event.payload[key]["subject_id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    return Ok(vec![]);
                };

                let pii = event.payload[key]["data"].clone();
                if pii.is_null() {
                    return Ok(vec![]);
                }

                if let Some(obj) = event.payload[key].as_object_mut() {
                    obj.remove("data");
                }

                Ok(vec![SecretPartition {
                    subject_id,
                    label: "default".to_string(),
                    payload: serde_json::to_vec(&pii)?,
                }])
            }

            "AttributesSet" => {
                let key = "AttributesSet";
                let n = event.payload[key]["secret_partitions"]
                    .as_array()
                    .map_or(0, Vec::len);

                if n == 0 {
                    return Ok(vec![]);
                }

                let mut result = Vec::with_capacity(n);

                for i in 0..n {
                    // Read identifying information (owned values, no live borrow).
                    let Some(subject_id) = event.payload[key]["secret_partitions"][i]["subject_id"]
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                    else {
                        continue;
                    };
                    // Use role_path as the crypto label (new format).
                    let Some(role_path) = event.payload[key]["secret_partitions"][i]["role_path"]
                        .as_str()
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let changes = event.payload[key]["secret_partitions"][i]["changes"].clone();
                    // Skip partitions with no actual secret data.
                    if changes.is_null() || changes == json!({}) {
                        continue;
                    }

                    // Clear the changes from the stored payload; they will be
                    // stored encrypted in the `encrypted_partitions` array.
                    if let Some(arr) = event.payload[key]["secret_partitions"].as_array_mut()
                        && let Some(obj) = arr[i].as_object_mut()
                    {
                        obj.insert("changes".to_string(), json!({}));
                    }

                    result.push(SecretPartition {
                        subject_id,
                        label: role_path,
                        payload: serde_json::to_vec(&changes)?,
                    });
                }

                Ok(result)
            }

            _ => Ok(vec![]),
        }
    }

    // ── Read path (new format) ────────────────────────────────────────────────

    fn reconstruct(
        &self,
        event: &mut SerializedEvent,
        partitions: Vec<DecryptedPartition>,
    ) -> Result<(), PiiCodecError> {
        match event.event_type.as_str() {
            "PersonCaptured" => {
                let key = "PersonCaptured";
                for part in partitions {
                    if part.label == "default" {
                        let pii: Value = serde_json::from_slice(&part.payload)?;
                        if let Some(obj) = event.payload[key].as_object_mut() {
                            obj.remove("encrypted_pii");
                            obj.remove("nonce");
                            obj.insert("name".to_string(), pii["name"].clone());
                            obj.insert("email".to_string(), pii["email"].clone());
                            obj.insert("phone".to_string(), pii["phone"].clone());
                        }
                    }
                }
                Ok(())
            }

            "PersonDetailsUpdated" => {
                let key = "PersonDetailsUpdated";
                for part in partitions {
                    if part.label == "default" {
                        let pii: Value = serde_json::from_slice(&part.payload)?;
                        if let Some(obj) = event.payload[key].as_object_mut() {
                            obj.remove("encrypted_data");
                            obj.remove("nonce");
                            obj.insert("data".to_string(), pii);
                        }
                    }
                }
                Ok(())
            }

            "AttributesSet" => {
                let key = "AttributesSet";
                for part in partitions {
                    let n = event.payload[key]["secret_partitions"]
                        .as_array()
                        .map_or(0, Vec::len);

                    for i in 0..n {
                        // Try role_path (new format) then person_ref (old format).
                        let label = event.payload[key]["secret_partitions"][i]["role_path"]
                            .as_str()
                            .or_else(|| {
                                event.payload[key]["secret_partitions"][i]["person_ref"].as_str()
                            })
                            .map(str::to_string);

                        if label.as_deref() == Some(&part.label) {
                            let changes: Value = serde_json::from_slice(&part.payload)?;
                            if let Some(arr) =
                                event.payload[key]["secret_partitions"].as_array_mut()
                                && let Some(obj) = arr[i].as_object_mut()
                            {
                                obj.insert("changes".to_string(), changes);
                            }
                            break;
                        }
                    }
                }
                Ok(())
            }

            _ => Ok(()),
        }
    }

    fn redact_partitions(
        &self,
        event: &mut SerializedEvent,
        labels: &[String],
    ) -> Result<(), PiiCodecError> {
        match event.event_type.as_str() {
            "PersonCaptured" => {
                let key = "PersonCaptured";
                if labels.iter().any(|l| l == "default")
                    && let Some(obj) = event.payload[key].as_object_mut()
                {
                    obj.remove("encrypted_pii");
                    obj.remove("nonce");
                    obj.insert("name".to_string(), json!("[redacted]"));
                    obj.insert("email".to_string(), json!("[redacted]"));
                    obj.insert("phone".to_string(), Value::Null);
                }
                Ok(())
            }

            "PersonDetailsUpdated" => {
                let key = "PersonDetailsUpdated";
                if labels.iter().any(|l| l == "default")
                    && let Some(obj) = event.payload[key].as_object_mut()
                {
                    obj.remove("encrypted_data");
                    obj.remove("nonce");
                    obj.insert("data".to_string(), json!({}));
                }
                Ok(())
            }

            "AttributesSet" => {
                let key = "AttributesSet";
                let n = event.payload[key]["secret_partitions"]
                    .as_array()
                    .map_or(0, Vec::len);

                for i in 0..n {
                    // Try role_path (new format) then person_ref (old format).
                    // Read as owned String before any mutable borrow.
                    let label = event.payload[key]["secret_partitions"][i]["role_path"]
                        .as_str()
                        .or_else(|| {
                            event.payload[key]["secret_partitions"][i]["person_ref"].as_str()
                        })
                        .map(str::to_string);

                    let should_redact = label
                        .as_deref()
                        .is_some_and(|lbl| labels.iter().any(|l| l == lbl));

                    if should_redact
                        && let Some(arr) = event.payload[key]["secret_partitions"].as_array_mut()
                        && let Some(obj) = arr[i].as_object_mut()
                    {
                        // Project sentinel convention: `{"redacted": true}`.
                        obj.insert("changes".to_string(), json!({ "redacted": true }));
                    }
                }
                Ok(())
            }

            _ => Ok(()),
        }
    }

    // ── Read path (legacy single-ciphertext format) ───────────────────────────

    fn extract_encrypted_legacy(&self, event: &SerializedEvent) -> Option<EncryptedPiiExtract> {
        match event.event_type.as_str() {
            "PersonCaptured" => {
                let key = "PersonCaptured";
                event.payload[key].get("encrypted_pii")?;
                let subject_id =
                    Uuid::parse_str(event.payload[key]["subject_id"].as_str()?).ok()?;
                let ciphertext = BASE64
                    .decode(event.payload[key]["encrypted_pii"].as_str()?)
                    .ok()?;
                let nonce = BASE64.decode(event.payload[key]["nonce"].as_str()?).ok()?;
                Some(EncryptedPiiExtract {
                    subject_id,
                    ciphertext,
                    nonce,
                })
            }

            "PersonDetailsUpdated" => {
                let key = "PersonDetailsUpdated";
                event.payload[key].get("encrypted_data")?;
                let subject_id =
                    Uuid::parse_str(event.payload[key]["subject_id"].as_str()?).ok()?;
                let ciphertext = BASE64
                    .decode(event.payload[key]["encrypted_data"].as_str()?)
                    .ok()?;
                let nonce = BASE64.decode(event.payload[key]["nonce"].as_str()?).ok()?;
                Some(EncryptedPiiExtract {
                    subject_id,
                    ciphertext,
                    nonce,
                })
            }

            _ => None,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use cqrs_es::persist::{PersistedEventRepository, SerializedEvent};
    use cqrs_es_crypto::{
        CryptoShreddingEventRepository, FieldCipher, InMemoryEventRepository, InMemoryKeyStore,
        KeyStore,
    };
    use uuid::Uuid;

    use crate::domain::journey::Journey;

    use super::JourneyPiiCodec;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_repo() -> CryptoShreddingEventRepository<InMemoryEventRepository> {
        let key_store: Arc<dyn KeyStore> = Arc::new(InMemoryKeyStore::new());
        let codec = Arc::new(JourneyPiiCodec);
        CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            key_store,
            FieldCipher::new(),
            codec,
        )
    }

    fn make_repo_with_parts() -> (
        CryptoShreddingEventRepository<InMemoryEventRepository>,
        Arc<InMemoryKeyStore>,
    ) {
        let key_store = Arc::new(InMemoryKeyStore::new());
        let codec = Arc::new(JourneyPiiCodec);
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            FieldCipher::new(),
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

    /// Build an `AttributesSet` serialised event (new `role_path` format).
    ///
    /// `secret_partitions` is a list of `(role_path, subject_id, changes)`
    /// triples that will appear in the `secret_partitions` JSON array.
    fn attributes_set_event(
        aggregate_id: &str,
        sequence: usize,
        secret_partitions: Vec<(String, Uuid, serde_json::Value)>,
    ) -> SerializedEvent {
        let parts: Vec<serde_json::Value> = secret_partitions
            .into_iter()
            .map(|(role_path, subject_id, changes)| {
                serde_json::json!({
                    "role_path": role_path,
                    "subject_id": subject_id.to_string(),
                    "changes":    changes,
                })
            })
            .collect();

        SerializedEvent::new(
            aggregate_id.to_string(),
            sequence,
            "Journey".to_string(),
            "AttributesSet".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "AttributesSet": {
                    "plaintext":         { "search/origin": "LHR" },
                    "secret_partitions": parts,
                }
            }),
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
        // even when persisted alongside PII events.
        let repo = make_repo();
        let aggregate_id = "journey-modified-passthrough";
        let subject_id = Uuid::new_v4();

        let pc = person_captured_event(aggregate_id, 1, subject_id);
        let mod_ev = modified_event(aggregate_id, 2);
        let original_mod_payload = mod_ev.payload.clone();

        repo.persist::<Journey>(&[pc, mod_ev], None).await.unwrap();

        let raw = repo.inner().all_events();
        let stored_mod = raw
            .iter()
            .find(|e| e.event_type == "JourneyModified")
            .unwrap();
        assert_eq!(stored_mod.payload, original_mod_payload);
    }

    // ── PersonCaptured ────────────────────────────────────────────────────────

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
        // PII fields must be absent from the stored payload.
        assert!(raw[0].payload["PersonCaptured"]["name"].is_null());
        assert!(raw[0].payload["PersonCaptured"]["email"].is_null());
        assert!(raw[0].payload["PersonCaptured"]["phone"].is_null());
        // Encrypted partitions must be present.
        assert!(raw[0].payload["PersonCaptured"]["encrypted_partitions"].is_array());
        // Non-PII fields must be intact.
        assert_eq!(
            raw[0].payload["PersonCaptured"]["person_ref"]
                .as_str()
                .unwrap(),
            "passenger_0"
        );
    }

    #[tokio::test]
    async fn test_person_captured_without_subject_id_passes_through_on_write() {
        // An event missing subject_id cannot be encrypted; it must be stored as-is.
        let repo = make_repo();
        let aggregate_id = "journey-pc-no-subject";
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref": "passenger_0",
                    "name":       "Alice Smith",
                    "email":      "alice@example.com",
                    "phone":      null
                    // subject_id missing
                }
            }),
            serde_json::json!({}),
        );
        let original_payload = event.payload.clone();

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw[0].payload, original_payload);
    }

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
        assert_eq!(
            events[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "Alice Smith"
        );
        assert_eq!(
            events[0].payload["PersonCaptured"]["email"]
                .as_str()
                .unwrap(),
            "alice@example.com"
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

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]"
        );
        assert_eq!(
            events[0].payload["PersonCaptured"]["email"]
                .as_str()
                .unwrap(),
            "[redacted]"
        );
        assert!(events[0].payload["PersonCaptured"]["phone"].is_null());
    }

    #[tokio::test]
    async fn test_plaintext_person_captured_passes_through_on_read() {
        // Events written without the partitioned format (no encrypted_partitions)
        // and without a legacy sentinel are returned as-is.
        let repo = make_repo();
        let aggregate_id = "journey-pc-plaintext-read";
        let subject_id = Uuid::new_v4();

        // Bypass the crypto layer — write directly to the inner store.
        repo.inner()
            .persist::<Journey>(&[person_captured_event(aggregate_id, 1, subject_id)], None)
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        // PII fields should still be present (plaintext passthrough).
        assert_eq!(
            events[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "Alice Smith"
        );
    }

    // ── PersonDetailsUpdated ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_persist_encrypts_person_details_updated() {
        let repo = make_repo();
        let aggregate_id = "journey-pdu-encrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        let raw = repo.inner().all_events();
        assert!(raw[0].payload["PersonDetailsUpdated"]["data"].is_null());
        assert!(raw[0].payload["PersonDetailsUpdated"]["encrypted_partitions"].is_array());
    }

    #[tokio::test]
    async fn test_person_details_updated_without_subject_id_passes_through() {
        let repo = make_repo();
        let aggregate_id = "journey-pdu-no-subject";
        let event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonDetailsUpdated": {
                    "person_ref": "passenger_0",
                    "data": { "passportNumber": "GB123456789" }
                    // subject_id missing
                }
            }),
            serde_json::json!({}),
        );
        let original_payload = event.payload.clone();

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw[0].payload, original_payload);
    }

    #[tokio::test]
    async fn test_get_events_decrypts_person_details_updated() {
        let repo = make_repo();
        let aggregate_id = "journey-pdu-decrypt";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"]["passportNumber"]
                .as_str()
                .unwrap(),
            "GB123456789"
        );
    }

    #[tokio::test]
    async fn test_get_events_redacts_person_details_updated_when_key_deleted() {
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "journey-pdu-redact";
        let subject_id = Uuid::new_v4();

        repo.persist::<Journey>(
            &[person_details_updated_event(aggregate_id, 1, subject_id)],
            None,
        )
        .await
        .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"],
            serde_json::json!({})
        );
    }

    #[tokio::test]
    async fn test_plaintext_person_details_updated_passes_through_on_read() {
        let repo = make_repo();
        let aggregate_id = "journey-pdu-plaintext-read";
        let subject_id = Uuid::new_v4();

        repo.inner()
            .persist::<Journey>(
                &[person_details_updated_event(aggregate_id, 1, subject_id)],
                None,
            )
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"]["passportNumber"]
                .as_str()
                .unwrap(),
            "GB123456789"
        );
    }

    // ── Legacy single-ciphertext format ───────────────────────────────────────

    #[tokio::test]
    async fn test_legacy_person_captured_decrypts_via_back_compat() {
        use cqrs_es_crypto::FieldCipher;

        // Build a legacy-format event (the old single-sentinel shape) by
        // manually encrypting the PII and constructing the stored payload.
        let key_store = Arc::new(InMemoryKeyStore::new());
        let cipher = FieldCipher::new(); // used for manual encryption below
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            FieldCipher::new(),
            Arc::new(JourneyPiiCodec),
        );

        let aggregate_id = "journey-legacy-pc";
        let subject_id = Uuid::new_v4();

        // Create the DEK via the key store and encrypt using the same cipher.
        // Legacy AAD format (as used by the repository): "aggregate_id:sequence".
        let dek = key_store.get_or_create_key(&subject_id).await.unwrap();
        let pii = serde_json::json!({ "name": "Alice Smith", "email": "alice@example.com", "phone": null });
        let aad = b"journey-legacy-pc:1";
        let encrypted = cipher.encrypt(&dek, &serde_json::to_vec(&pii).unwrap(), aad);

        // Build a legacy-format stored payload.
        let legacy_event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref":    "passenger_0",
                    "subject_id":    subject_id.to_string(),
                    "encrypted_pii": BASE64.encode(&encrypted.ciphertext),
                    "nonce":         BASE64.encode(&encrypted.nonce)
                }
            }),
            serde_json::json!({}),
        );

        // Write directly to the inner store so the crypto layer reads it back.
        repo.inner()
            .persist::<Journey>(&[legacy_event], None)
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "Alice Smith"
        );
    }

    #[tokio::test]
    async fn test_legacy_person_captured_redacts_when_key_deleted() {
        use cqrs_es_crypto::FieldCipher;

        let key_store = Arc::new(InMemoryKeyStore::new());
        let cipher = FieldCipher::new();
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            FieldCipher::new(),
            Arc::new(JourneyPiiCodec),
        );

        let aggregate_id = "journey-legacy-pc-redact";
        let subject_id = Uuid::new_v4();
        let dek = key_store.get_or_create_key(&subject_id).await.unwrap();
        let pii =
            serde_json::json!({ "name": "Alice", "email": "alice@example.com", "phone": null });
        let aad = b"journey-legacy-pc-redact:1";
        let encrypted = cipher.encrypt(&dek, &serde_json::to_vec(&pii).unwrap(), aad);

        let legacy_event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonCaptured".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonCaptured": {
                    "person_ref":    "passenger_0",
                    "subject_id":    subject_id.to_string(),
                    "encrypted_pii": BASE64.encode(&encrypted.ciphertext),
                    "nonce":         BASE64.encode(&encrypted.nonce)
                }
            }),
            serde_json::json!({}),
        );
        repo.inner()
            .persist::<Journey>(&[legacy_event], None)
            .await
            .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonCaptured"]["name"]
                .as_str()
                .unwrap(),
            "[redacted]"
        );
    }

    #[tokio::test]
    async fn test_legacy_person_details_updated_decrypts_via_back_compat() {
        use cqrs_es_crypto::FieldCipher;

        let key_store = Arc::new(InMemoryKeyStore::new());
        let cipher = FieldCipher::new();
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            FieldCipher::new(),
            Arc::new(JourneyPiiCodec),
        );

        let aggregate_id = "journey-legacy-pdu";
        let subject_id = Uuid::new_v4();
        let dek = key_store.get_or_create_key(&subject_id).await.unwrap();
        let data = serde_json::json!({ "passportNumber": "GB123456789" });
        let aad = b"journey-legacy-pdu:1";
        let encrypted = cipher.encrypt(&dek, &serde_json::to_vec(&data).unwrap(), aad);

        let legacy_event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonDetailsUpdated": {
                    "person_ref":     "passenger_0",
                    "subject_id":     subject_id.to_string(),
                    "encrypted_data": BASE64.encode(&encrypted.ciphertext),
                    "nonce":          BASE64.encode(&encrypted.nonce)
                }
            }),
            serde_json::json!({}),
        );
        repo.inner()
            .persist::<Journey>(&[legacy_event], None)
            .await
            .unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"]["passportNumber"]
                .as_str()
                .unwrap(),
            "GB123456789"
        );
    }

    #[tokio::test]
    async fn test_legacy_person_details_updated_redacts_when_key_deleted() {
        use cqrs_es_crypto::FieldCipher;

        let key_store = Arc::new(InMemoryKeyStore::new());
        let cipher = FieldCipher::new();
        let repo = CryptoShreddingEventRepository::new(
            InMemoryEventRepository::default(),
            Arc::clone(&key_store) as Arc<dyn KeyStore>,
            FieldCipher::new(),
            Arc::new(JourneyPiiCodec),
        );

        let aggregate_id = "journey-legacy-pdu-redact";
        let subject_id = Uuid::new_v4();
        let dek = key_store.get_or_create_key(&subject_id).await.unwrap();
        let data = serde_json::json!({ "passportNumber": "GB123456789" });
        let aad = b"journey-legacy-pdu-redact:1";
        let encrypted = cipher.encrypt(&dek, &serde_json::to_vec(&data).unwrap(), aad);

        let legacy_event = SerializedEvent::new(
            aggregate_id.to_string(),
            1,
            "Journey".to_string(),
            "PersonDetailsUpdated".to_string(),
            "1.0".to_string(),
            serde_json::json!({
                "PersonDetailsUpdated": {
                    "person_ref":     "passenger_0",
                    "subject_id":     subject_id.to_string(),
                    "encrypted_data": BASE64.encode(&encrypted.ciphertext),
                    "nonce":          BASE64.encode(&encrypted.nonce)
                }
            }),
            serde_json::json!({}),
        );
        repo.inner()
            .persist::<Journey>(&[legacy_event], None)
            .await
            .unwrap();

        key_store.delete_key(&subject_id).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(
            events[0].payload["PersonDetailsUpdated"]["data"],
            serde_json::json!({})
        );
    }

    // ── Cross-journey and multi-subject shredding ─────────────────────────────

    #[tokio::test]
    async fn test_single_key_deletion_shreds_all_journeys_for_subject() {
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

        key_store.delete_key(&subject_a).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();

        let ev_a = events
            .iter()
            .find(|e| e.payload["PersonCaptured"]["person_ref"].as_str() == Some("passenger_0"))
            .unwrap();
        assert_eq!(
            ev_a.payload["PersonCaptured"]["name"].as_str().unwrap(),
            "[redacted]"
        );

        let ev_b = events
            .iter()
            .find(|e| e.payload["PersonCaptured"]["person_ref"].as_str() == Some("passenger_1"))
            .unwrap();
        assert_eq!(
            ev_b.payload["PersonCaptured"]["name"].as_str().unwrap(),
            "Bob Jones"
        );

        let mod_event = events
            .iter()
            .find(|e| e.event_type == "JourneyModified")
            .unwrap();
        assert_eq!(mod_event.payload, original_mod_payload);
    }

    // ── stream_events ─────────────────────────────────────────────────────────

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

    // ── AAD binding (existing variants) ───────────────────────────────────────

    #[tokio::test]
    async fn test_aad_binds_person_captured_ciphertext_to_event_position() {
        let repo = make_repo();
        let aggregate_id = "journey-aad-pc";
        let subject_id = Uuid::new_v4();

        let ev1 = person_captured_event(aggregate_id, 1, subject_id);
        let ev2 = person_captured_event(aggregate_id, 2, subject_id);
        repo.persist::<Journey>(&[ev1, ev2], None).await.unwrap();

        let raw = repo.inner().all_events();
        let ct1 = raw[0].payload["PersonCaptured"]["encrypted_partitions"][0]["ciphertext"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["PersonCaptured"]["encrypted_partitions"][0]["ciphertext"]
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
        let ct1 = raw[0].payload["PersonDetailsUpdated"]["encrypted_partitions"][0]["ciphertext"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["PersonDetailsUpdated"]["encrypted_partitions"][0]["ciphertext"]
            .as_str()
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "identical plaintext at different positions must produce different ciphertexts"
        );
    }

    // ── AttributesSet ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_attributes_set_passes_through_when_no_secret_partitions() {
        // An AttributesSet event with no secret partitions carries no PII and
        // must be stored and retrieved completely unchanged.
        let repo = make_repo();
        let aggregate_id = "journey-attrs-no-secrets";

        let event = attributes_set_event(aggregate_id, 1, vec![]);
        let original_payload = event.payload.clone();

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw.len(), 1);
        assert_eq!(
            raw[0].payload, original_payload,
            "no-secret AttributesSet must pass through unchanged"
        );
    }

    #[tokio::test]
    async fn test_attributes_set_encrypts_each_partition_under_its_own_dek() {
        // After persisting, the `changes` inside each SecretPartitionData must be
        // cleared and an `encrypted_partitions` array added.  The `plaintext`
        // field must be untouched.
        let repo = make_repo();
        let aggregate_id = "journey-attrs-encrypt";
        let subject_id = Uuid::new_v4();

        let event = attributes_set_event(
            aggregate_id,
            1,
            vec![(
                "persons/passenger_0".to_string(),
                subject_id,
                serde_json::json!({ "persons/passenger_0/passport": "AB123456" }),
            )],
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let raw = repo.inner().all_events();
        assert_eq!(raw.len(), 1);

        // The changes must be cleared.
        assert_eq!(
            raw[0].payload["AttributesSet"]["secret_partitions"][0]["changes"],
            serde_json::json!({}),
            "changes must be cleared from the stored payload"
        );
        // The encrypted_partitions array must contain one entry.
        let enc_parts = raw[0].payload["AttributesSet"]["encrypted_partitions"]
            .as_array()
            .expect("encrypted_partitions must be present");
        assert_eq!(enc_parts.len(), 1);
        assert_eq!(
            enc_parts[0]["label"].as_str().unwrap(),
            "persons/passenger_0",
            "label must equal role_path"
        );
        // The plaintext field must be intact.
        assert_eq!(
            raw[0].payload["AttributesSet"]["plaintext"]["search/origin"]
                .as_str()
                .unwrap(),
            "LHR"
        );
    }

    #[tokio::test]
    async fn test_attributes_set_decrypts_multi_subject_partitions() {
        // Two subjects' changes in one event must each decrypt independently and
        // be routed back to the correct SecretPartitionData entry by person_ref.
        let repo = make_repo();
        let aggregate_id = "journey-attrs-decrypt";
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        let event = attributes_set_event(
            aggregate_id,
            1,
            vec![
                (
                    "persons/passenger_0".to_string(),
                    subject_a,
                    serde_json::json!({ "persons/passenger_0/passport": "AB111111" }),
                ),
                (
                    "persons/passenger_1".to_string(),
                    subject_b,
                    serde_json::json!({ "persons/passenger_1/passport": "CD222222" }),
                ),
            ],
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(events.len(), 1);

        let parts = events[0].payload["AttributesSet"]["secret_partitions"]
            .as_array()
            .expect("secret_partitions must be present");
        assert_eq!(parts.len(), 2);

        let p0 = parts
            .iter()
            .find(|p| p["role_path"].as_str() == Some("persons/passenger_0"))
            .expect("persons/passenger_0 partition must be present");
        assert_eq!(
            p0["changes"]["persons/passenger_0/passport"]
                .as_str()
                .unwrap(),
            "AB111111"
        );

        let p1 = parts
            .iter()
            .find(|p| p["role_path"].as_str() == Some("persons/passenger_1"))
            .expect("persons/passenger_1 partition must be present");
        assert_eq!(
            p1["changes"]["persons/passenger_1/passport"]
                .as_str()
                .unwrap(),
            "CD222222"
        );
    }

    #[tokio::test]
    async fn test_attributes_set_partial_shred_keeps_intact_partitions() {
        // Deleting subject A's DEK must redact only A's SecretPartitionData.changes
        // and leave subject B's changes fully readable.
        let (repo, key_store) = make_repo_with_parts();
        let aggregate_id = "journey-attrs-shred";
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        let event = attributes_set_event(
            aggregate_id,
            1,
            vec![
                (
                    "persons/passenger_0".to_string(),
                    subject_a,
                    serde_json::json!({ "persons/passenger_0/passport": "AB111111" }),
                ),
                (
                    "persons/passenger_1".to_string(),
                    subject_b,
                    serde_json::json!({ "persons/passenger_1/passport": "CD222222" }),
                ),
            ],
        );

        repo.persist::<Journey>(&[event], None).await.unwrap();

        // Shred only subject A.
        key_store.delete_key(&subject_a).await.unwrap();

        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        let parts = events[0].payload["AttributesSet"]["secret_partitions"]
            .as_array()
            .unwrap();

        let p_a = parts
            .iter()
            .find(|p| p["role_path"].as_str() == Some("persons/passenger_0"))
            .unwrap();
        assert!(
            p_a["changes"]["redacted"].as_bool().unwrap(),
            "subject A's changes must carry the redaction sentinel"
        );

        let p_b = parts
            .iter()
            .find(|p| p["role_path"].as_str() == Some("persons/passenger_1"))
            .unwrap();
        assert_eq!(
            p_b["changes"]["persons/passenger_1/passport"]
                .as_str()
                .unwrap(),
            "CD222222",
            "subject B's changes must remain intact"
        );
    }

    #[tokio::test]
    async fn test_attributes_set_aad_binds_partition_to_subject_and_label() {
        // Identical plaintext in two events at different sequence numbers must
        // produce different ciphertexts (AAD includes aggregate_id:seq:subject:label).
        let repo = make_repo();
        let aggregate_id = "journey-attrs-aad";
        let subject_id = Uuid::new_v4();

        let ev1 = attributes_set_event(
            aggregate_id,
            1,
            vec![(
                "persons/passenger_0".to_string(),
                subject_id,
                serde_json::json!({ "persons/passenger_0/passport": "AB123456" }),
            )],
        );
        let ev2 = attributes_set_event(
            aggregate_id,
            2,
            vec![(
                "persons/passenger_0".to_string(),
                subject_id,
                serde_json::json!({ "persons/passenger_0/passport": "AB123456" }),
            )],
        );

        repo.persist::<Journey>(&[ev1, ev2], None).await.unwrap();

        let raw = repo.inner().all_events();
        let ct1 = raw[0].payload["AttributesSet"]["encrypted_partitions"][0]["ciphertext"]
            .as_str()
            .unwrap();
        let ct2 = raw[1].payload["AttributesSet"]["encrypted_partitions"][0]["ciphertext"]
            .as_str()
            .unwrap();

        assert_ne!(
            ct1, ct2,
            "identical plaintext at different sequence numbers must produce different ciphertexts"
        );
    }

    #[tokio::test]
    async fn test_attributes_set_old_person_ref_format_decrypts_correctly() {
        // Events written before the person_ref → role_path rename used
        // `person_ref: "passenger_0"` (short slot name, no prefix) as both the
        // JSON field name and the encryption label.  The codec must still be
        // able to decrypt such events after the rename.
        let repo = make_repo();
        let aggregate_id = "journey-attrs-legacy-person-ref";
        let subject_id = Uuid::new_v4();

        // Build the event in the OLD format (person_ref key, no prefix).
        let old_format_event = {
            let parts = serde_json::json!([{
                "person_ref": "passenger_0",
                "subject_id": subject_id.to_string(),
                "changes":    { "persons/passenger_0/passport": "AB123456" },
            }]);
            SerializedEvent::new(
                aggregate_id.to_string(),
                1,
                "Journey".to_string(),
                "AttributesSet".to_string(),
                "1.0".to_string(),
                serde_json::json!({
                    "AttributesSet": {
                        "plaintext":         {},
                        "secret_partitions": parts,
                    }
                }),
                serde_json::json!({}),
            )
        };

        repo.persist::<Journey>(&[old_format_event], None)
            .await
            .unwrap();

        // The event was encrypted with the label "passenger_0" (old person_ref).
        // Decrypting must route the plaintext back to the correct partition.
        let events = repo.get_events::<Journey>(aggregate_id).await.unwrap();
        assert_eq!(events.len(), 1);

        let parts = events[0].payload["AttributesSet"]["secret_partitions"]
            .as_array()
            .expect("secret_partitions must be present");
        assert_eq!(parts.len(), 1);
        assert_eq!(
            parts[0]["changes"]["persons/passenger_0/passport"]
                .as_str()
                .unwrap(),
            "AB123456",
            "old person_ref event must decrypt correctly"
        );
    }
}
