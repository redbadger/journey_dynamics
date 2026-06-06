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
    ///
    /// # Deprecated
    /// Use [`JourneyCommand::CaptureAndBindSubject`] followed by
    /// [`JourneyCommand::SetAttributes`] for path-keyed PII fields instead.
    #[deprecated(
        since = "0.4.0",
        note = "use CaptureAndBindSubject + SetAttributes instead"
    )]
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

    /// Register a data subject (email → `subject_id` mapping) in this journey.
    ///
    /// If the subject is already registered with the same email the command is
    /// a no-op. If registered with a *different* email, the email is updated.
    ///
    /// Email is required so the subject can be found by GDPR erasure requests.
    CaptureSubject { subject_id: Uuid, email: String },

    /// Bind a registered subject to a role path (e.g. `"persons/passenger_0"`).
    ///
    /// The subject must have been registered by a prior `CaptureSubject`
    /// command. The role path must not already be bound to a *different*
    /// subject; binding the same subject to the same path is idempotent.
    BindSubject {
        role_path: AttributePath,
        subject_id: Uuid,
    },

    /// Convenience composite: register a subject and bind it to a role path
    /// in a single command. Equivalent to `CaptureSubject` followed by
    /// `BindSubject` but avoids a round-trip when both are needed together.
    CaptureAndBindSubject {
        role_path: AttributePath,
        subject_id: Uuid,
        email: String,
    },

    /// Mark the journey as complete.
    Complete,

    /// Emit a `SubjectForgotten` audit event.
    ///
    /// Called by the shredding route handler after the subject's DEK has
    /// already been deleted. The event serves as an immutable audit record
    /// and triggers the read-model projection to null out the person slot.
    ForgetSubject { subject_id: Uuid },
}
