//! Generic PII codec for the path-keyed `AttributesSet` capture event.
//!
//! [`AttributesSetCodec`] implements [`PiiEventCodec`] for the standard
//! `AttributesSet` event shape produced by the capture pipeline. Its contents
//! are domain-agnostic — one encrypted partition per secret slice, keyed by the
//! per-partition `role_path` (the crypto label) — so most domains can use it as
//! their codec directly without hand-writing anything. Every other event type
//! passes through unchanged.
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
//!       { "role_path": "<role>", "subject_id": "<uuid>", "changes": {} },
//!       …
//!     ],
//!     "subjects":            ["<uuid>", …],
//!     "encrypted_partitions": [
//!       { "subject_id": "…", "label": "<role_path>", "nonce": "…", "ciphertext": "…" },
//!       …
//!     ]
//!   }
//! }
//! ```
//!
//! Each partition's payload is the JSON serialisation of the corresponding
//! `changes` map. The label equals `role_path`, allowing `reconstruct` and
//! `redact_partitions` to route decrypted bytes back to the correct partition.

use cqrs_es::persist::SerializedEvent;
use cqrs_es_crypto::{DecryptedPartition, PiiCodecError, PiiEventCodec, SecretPartition};
use serde_json::{Value, json};
use uuid::Uuid;

/// PII codec for the path-keyed `AttributesSet` capture event.
///
/// Handles one encrypted partition per secret slice, with the per-partition
/// `role_path` used as the crypto label. All other event types pass through
/// unchanged.
pub struct AttributesSetCodec;

impl PiiEventCodec for AttributesSetCodec {
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
                    // Use role_path as the crypto label.
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

    // ── Read path ───────────────────────────────────────────────────────────--

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
