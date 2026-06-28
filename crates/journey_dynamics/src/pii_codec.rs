//! PII codec for the Journey aggregate — hand-written implementation.
//!
//! [`JourneyPiiCodec`] implements [`PiiEventCodec`] directly.  It is the
//! single source of truth for how redaction sentinels are expressed and how
//! multi-partition encryption is routed for the path-keyed `AttributesSet`
//! event.
//!
//! # Event schema
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
//! `changes: BTreeMap<PointerBuf, Value>`.  The label equals `person_ref`,
//! allowing `reconstruct` and `redact_partitions` to route decrypted bytes
//! back to the correct `SecretPartitionData` entry.

use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::{DecryptedPartition, PiiCodecError, PiiEventCodec, SecretPartition};
use serde_json::{Value, json};
use uuid::Uuid;

/// The PII codec for [`JourneyEvent`](crate::domain::events::JourneyEvent).
///
/// Handles the `AttributesSet` event: one encrypted partition per
/// `SecretPartitionData` entry, with the per-partition `role_path` used as the
/// crypto label. All other event types pass through unchanged.
pub struct JourneyPiiCodec;

impl PiiEventCodec for JourneyPiiCodec {
    // ── Write path ────────────────────────────────────────────────────────────

    fn extract_partitions(
        &self,
        event: &mut SerializedEvent,
    ) -> Result<Vec<SecretPartition>, PiiCodecError> {
        match event.event_type.as_str() {
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
            "AttributesSet" => {
                let key = "AttributesSet";
                for part in partitions {
                    let n = event.payload[key]["secret_partitions"]
                        .as_array()
                        .map_or(0, Vec::len);

                    for i in 0..n {
                        let label = event.payload[key]["secret_partitions"][i]["role_path"]
                            .as_str()
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
            "AttributesSet" => {
                let key = "AttributesSet";
                let n = event.payload[key]["secret_partitions"]
                    .as_array()
                    .map_or(0, Vec::len);

                for i in 0..n {
                    // Read as owned String before any mutable borrow.
                    let label = event.payload[key]["secret_partitions"][i]["role_path"]
                        .as_str()
                        .map(str::to_string);

                    let should_redact = label
                        .as_deref()
                        .is_some_and(|lbl| labels.iter().any(|l| l == lbl));

                    if should_redact
                        && let Some(arr) = event.payload[key]["secret_partitions"].as_array_mut()
                        && let Some(obj) = arr[i].as_object_mut()
                    {
                        // Project sentinel convention: `{"/redacted": true}`.
                        obj.insert("changes".to_string(), json!({ "/redacted": true }));
                    }
                }
                Ok(())
            }

            _ => Ok(()),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
                serde_json::json!({ "/persons/passenger_0/passport": "AB123456" }),
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
                    serde_json::json!({ "/persons/passenger_0/passport": "AB111111" }),
                ),
                (
                    "persons/passenger_1".to_string(),
                    subject_b,
                    serde_json::json!({ "/persons/passenger_1/passport": "CD222222" }),
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
            p0["changes"]["/persons/passenger_0/passport"]
                .as_str()
                .unwrap(),
            "AB111111"
        );

        let p1 = parts
            .iter()
            .find(|p| p["role_path"].as_str() == Some("persons/passenger_1"))
            .expect("persons/passenger_1 partition must be present");
        assert_eq!(
            p1["changes"]["/persons/passenger_1/passport"]
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
                    serde_json::json!({ "/persons/passenger_0/passport": "AB111111" }),
                ),
                (
                    "persons/passenger_1".to_string(),
                    subject_b,
                    serde_json::json!({ "/persons/passenger_1/passport": "CD222222" }),
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
            p_a["changes"]["/redacted"].as_bool().unwrap(),
            "subject A's changes must carry the redaction sentinel"
        );

        let p_b = parts
            .iter()
            .find(|p| p["role_path"].as_str() == Some("persons/passenger_1"))
            .unwrap();
        assert_eq!(
            p_b["changes"]["/persons/passenger_1/passport"]
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
                serde_json::json!({ "/persons/passenger_0/passport": "AB123456" }),
            )],
        );
        let ev2 = attributes_set_event(
            aggregate_id,
            2,
            vec![(
                "persons/passenger_0".to_string(),
                subject_id,
                serde_json::json!({ "/persons/passenger_0/passport": "AB123456" }),
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
}
