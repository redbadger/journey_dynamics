use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::AttributePath;

#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    /// Create a new journey.
    Start { id: Uuid },

    /// Capture non-PII shared data for a step.
    /// The `data` field MUST NOT contain PII — use `CapturePerson` or
    /// `CapturePersonDetails` for any personally identifiable information.
    ///
    /// # Deprecated
    /// Use [`JourneyCommand::SetAttributes`] instead.
    #[deprecated(since = "0.3.0", note = "use SetAttributes (path-keyed attributes)")]
    Capture { step: String, data: Value },

    /// Set one or more journey attributes in a single command.
    ///
    /// `changes` is a flat map of [`AttributePath`] to value. A single
    /// `SetAttributes` may touch attributes for **multiple subjects** (e.g.
    /// two passengers' passport numbers in one form submission). The
    /// aggregate classifies each path as plaintext or secret, validates
    /// plaintext changes against the JSON Schema, and encrypts secret
    /// changes under the appropriate subject's DEK.
    SetAttributes {
        changes: BTreeMap<AttributePath, Value>,
    },

    /// Register or update a person's identity fields in a named slot.
    ///
    /// `person_ref` is a client-assigned, journey-local slot name
    /// (e.g. `"lead_booker"`, `"passenger_0"`). It has no meaning outside
    /// the journey and is not PII.
    ///
    /// Creates the slot if it does not exist. If the slot already exists
    /// with the same `subject_id`, the identity fields are updated
    /// (idempotent). If the slot already exists with a **different**
    /// `subject_id`, the command is rejected with `PersonRefConflict`.
    CapturePerson {
        person_ref: String,
        subject_id: Uuid,
        name: String,
        email: String,
        phone: Option<String>,
    },

    /// Capture free-form PII details for an existing person slot.
    ///
    /// The slot must have been created by a prior `CapturePerson` command
    /// for the same `person_ref`. The `data` is merged (JSON merge-patch)
    /// into the slot's `details` field and encrypted under the subject's
    /// DEK by the crypto layer.
    ///
    /// # Deprecated
    /// Use [`JourneyCommand::SetAttributes`] with path-keyed secret fields instead.
    #[deprecated(since = "0.3.0", note = "use SetAttributes (path-keyed attributes)")]
    CapturePersonDetails { person_ref: String, data: Value },

    /// Mark the journey as complete.
    Complete,

    /// Emit a `SubjectForgotten` audit event.
    ///
    /// Called by the shredding route handler after the subject's DEK has
    /// already been deleted. The event serves as an immutable audit record
    /// and triggers the read-model projection to null out the person slot.
    ForgetSubject { subject_id: Uuid },
}
