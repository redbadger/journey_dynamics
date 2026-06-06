use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::AttributePath;

#[derive(Debug, Deserialize)]
pub enum JourneyCommand {
    /// Create a new journey.
    Start { id: Uuid },

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

    /// Register a data subject (email → `subject_id` mapping) in this journey.
    ///
    /// If the subject is already registered with the same email the command is
    /// a no-op. If registered with a *different* email, the email is updated.
    ///
    /// Email is required so the subject can be found by GDPR erasure requests.
    RegisterSubject { subject_id: Uuid, email: String },

    /// Bind a registered subject to a role path (e.g. `"persons/passenger_0"`).
    ///
    /// The subject must have been registered by a prior `RegisterSubject`
    /// command. The role path must not already be bound to a *different*
    /// subject; binding the same subject to the same path is idempotent.
    BindSubject {
        role_path: AttributePath,
        subject_id: Uuid,
    },

    /// Convenience composite: register a subject and bind it to a role path
    /// in a single command. Equivalent to `RegisterSubject` followed by
    /// `BindSubject` but avoids a round-trip when both are needed together.
    RegisterAndBindSubject {
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
