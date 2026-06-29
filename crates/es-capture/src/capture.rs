//! The progressive-capture pipeline.
//!
//! [`capture`] is the domain-agnostic core of a `SetAttributes`-style command:
//! it classifies a flat batch of path-keyed changes against the
//! [`AttributeSchema`], splits them into plaintext and per-subject secret
//! slices, validates the resulting plaintext state, and (optionally) evaluates
//! a [`DecisionEngine`]. It emits no events and mutates no state — the caller
//! turns the returned [`CaptureOutcome`] into its own domain events.

use std::collections::BTreeMap;

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use jsonptr::PointerBuf;

use crate::{
    attribute_schema::{AttributeSchema, PiiClass, classify_changes},
    decision_engine::{DecisionEngine, WorkflowDecision},
    json_path::assign_all,
    schema_validator::SchemaValidator,
    subject_registry::SubjectRegistry,
};

/// One subject's secret slice produced by classifying a change batch.
///
/// The caller typically maps this onto its own per-subject event payload; the
/// `role_path` doubles as the crypto label (AAD) on the write path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSlice {
    /// Full schema path at which the subject is bound, e.g. `/persons/passenger_0`.
    pub role_path: PointerBuf,
    /// The subject whose DEK encrypts this slice.
    pub subject_id: Uuid,
    /// Path → value changes belonging to this subject.
    pub changes: BTreeMap<PointerBuf, Value>,
}

/// The result of a successful [`capture`].
#[derive(Debug, Clone)]
pub struct CaptureOutcome {
    /// Non-sensitive path → value changes.
    pub plaintext: BTreeMap<PointerBuf, Value>,
    /// One slice per subject whose secret attributes were touched, sorted by
    /// `role_path` for deterministic output.
    pub secret: Vec<SecretSlice>,
    /// The decision-engine result, present only when an engine was supplied.
    pub decision: Option<WorkflowDecision>,
}

/// Why a [`capture`] was rejected.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// One or more paths are not described by the schema at all.
    #[error("unknown attribute paths: {0:?}")]
    UnknownAttributePath(Vec<PointerBuf>),
    /// A secret path's subject could not be resolved — its role is unbound or
    /// the subject has been forgotten. Carries the unresolved `role_path`.
    #[error("no active subject bound for role path '{0}'")]
    SubjectNotResolved(PointerBuf),
    /// A change pointer could not be assigned into the state tree.
    #[error("invalid JSON pointer: {0}")]
    InvalidJsonPointer(#[from] jsonptr::assign::Error),
    /// The plaintext changes failed schema validation.
    #[error("invalid data: {0}")]
    InvalidData(String),
    /// The decision engine failed to evaluate.
    #[error("decision engine error: {0}")]
    DecisionEngine(String),
}

/// Run the capture pipeline for a batch of path-keyed `changes`.
///
/// - `schema` classifies each path as plaintext or secret-for-a-subject.
/// - `registry` resolves a secret path's role to its active (bound,
///   non-forgotten) subject.
/// - `current_state` is the aggregate's accumulated plaintext bag, used both
///   for plaintext validation and as the engine's input state.
/// - `validator` checks the merged plaintext state.
/// - `engine`, when `Some`, is evaluated against `current_state` + `changes`;
///   when `None`, no [`WorkflowDecision`] is produced.
///
/// # Errors
/// Returns [`CaptureError`] if any path is unknown, a secret path's subject is
/// unresolved, plaintext validation fails, or the engine errors.
pub async fn capture(
    schema: &AttributeSchema,
    registry: &SubjectRegistry,
    current_state: &Value,
    changes: &BTreeMap<PointerBuf, Value>,
    validator: &dyn SchemaValidator,
    engine: Option<&dyn DecisionEngine>,
) -> Result<CaptureOutcome, CaptureError> {
    // Classify every path. Secret paths resolve their subject via the registry;
    // unbound or forgotten subjects yield `None`, landing the path in `unknown`.
    let classification = classify_changes(schema, changes, |role_path| {
        registry.resolve_active(role_path)
    });

    // Paths the schema does not describe at all are rejected outright.
    let truly_unknown: Vec<PointerBuf> = classification
        .unknown
        .iter()
        .filter(|p| schema.classify(p).is_none())
        .cloned()
        .collect();
    if !truly_unknown.is_empty() {
        return Err(CaptureError::UnknownAttributePath(truly_unknown));
    }

    // Remaining unknown paths are secret paths whose subject could not be
    // resolved (role not bound, or subject forgotten).
    for path in &classification.unknown {
        if let Some(cls) = schema.classify(path)
            && let PiiClass::Secret { subject } = cls.as_ref()
        {
            return Err(CaptureError::SubjectNotResolved(subject.clone()));
        }
    }

    // Build one slice per role path, sorted deterministically.
    let mut secret: Vec<SecretSlice> = classification
        .secret_by_subject
        .into_iter()
        .map(|(role_path, (subject_id, changes))| SecretSlice {
            role_path,
            subject_id,
            changes,
        })
        .collect();
    secret.sort_by(|a, b| a.role_path.cmp(&b.role_path));

    // Validate the full prospective state — current state plus every change,
    // plaintext *and* secret — against the schema, before anything is
    // encrypted. This validates secret values too (e.g. a malformed
    // `dateOfBirth`) and does so on every batch, not only when a plaintext
    // change happens to co-occur. Redacted (shredded) partitions leave their
    // fields absent and add only an unknown `redacted` marker, so a schema
    // that does not forbid additional properties tolerates them.
    if !classification.plaintext.is_empty() || !secret.is_empty() {
        let mut merged = current_state.clone();
        assign_all(&mut merged, &classification.plaintext)?;
        for slice in &secret {
            assign_all(&mut merged, &slice.changes)?;
        }
        validator
            .validate(&merged)
            .map_err(|e| CaptureError::InvalidData(e.to_string()))?;
    }

    // Evaluate the workflow with the full (plaintext + secret) change set.
    let decision = match engine {
        Some(engine) => Some(
            engine
                .evaluate_attributes(current_state, changes)
                .await
                .map_err(|e| CaptureError::DecisionEngine(e.to_string()))?,
        ),
        None => None,
    };

    Ok(CaptureOutcome {
        plaintext: classification.plaintext,
        secret,
        decision,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::{
        attribute_schema::{AttributeSchemaConfig, NamespacePattern, SecretPathConfig},
        schema_validator::{JsonSchemaValidator, NoOpValidator},
        subject_registry::SubjectRegistry,
    };

    fn ptr(s: &str) -> PointerBuf {
        PointerBuf::parse(s).unwrap()
    }

    /// A fixed-subject schema with one secret integer field `/self/salary`
    /// (encrypted under subject `/self`) plus a data schema that types it.
    fn fixed_subject_schema() -> AttributeSchema {
        AttributeSchema::from(AttributeSchemaConfig {
            permissive: false,
            plaintext_prefixes: vec![],
            namespace_patterns: vec![],
            secret_paths: vec![SecretPathConfig {
                path: ptr("/self/salary"),
                subject: ptr("/self"),
            }],
            plaintext_paths: vec![],
        })
    }

    fn salary_validator() -> JsonSchemaValidator {
        JsonSchemaValidator::new(&json!({
            "type": "object",
            "properties": {
                "self": {
                    "type": "object",
                    "properties": { "salary": { "type": "integer" } }
                }
            }
        }))
        .unwrap()
    }

    fn registry_with_self(subject: Uuid) -> SubjectRegistry {
        let mut registry = SubjectRegistry::default();
        registry.register(subject, "a@example.com".to_string());
        registry.bind(ptr("/self"), subject);
        registry
    }

    /// Permissive schema with a `/persons/<ref>/<field>` secret namespace.
    fn schema() -> AttributeSchema {
        AttributeSchema::permissive().with_namespace_patterns(vec![NamespacePattern {
            prefix: ptr("/persons"),
            plaintext_suffixes: BTreeSet::new(),
        }])
    }

    fn changes(pairs: &[(&str, Value)]) -> BTreeMap<PointerBuf, Value> {
        pairs.iter().map(|(p, v)| (ptr(p), v.clone())).collect()
    }

    #[tokio::test]
    async fn splits_plaintext_and_secret_without_engine() {
        let subject = Uuid::from_u128(1);
        let mut registry = SubjectRegistry::default();
        registry.register(subject, "a@example.com".to_string());
        registry.bind(ptr("/persons/passenger_0"), subject);

        let outcome = capture(
            &schema(),
            &registry,
            &json!({}),
            &changes(&[
                ("/search/origin", json!("LHR")),
                ("/persons/passenger_0/passport", json!("AB123456")),
            ]),
            &NoOpValidator,
            None,
        )
        .await
        .unwrap();

        assert_eq!(outcome.plaintext.len(), 1);
        assert!(outcome.plaintext.contains_key(&ptr("/search/origin")));
        assert_eq!(outcome.secret.len(), 1);
        assert_eq!(outcome.secret[0].role_path, ptr("/persons/passenger_0"));
        assert_eq!(outcome.secret[0].subject_id, subject);
        // No engine supplied → no decision.
        assert!(outcome.decision.is_none());
    }

    #[tokio::test]
    async fn unknown_path_is_rejected() {
        // A non-permissive schema knows nothing, so any path is unknown.
        let schema = AttributeSchema::new(BTreeMap::new(), None);
        let err = capture(
            &schema,
            &SubjectRegistry::default(),
            &json!({}),
            &changes(&[("/mystery/field", json!(1))]),
            &NoOpValidator,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CaptureError::UnknownAttributePath(paths) if paths == vec![ptr("/mystery/field")])
        );
    }

    #[tokio::test]
    async fn secret_only_batch_is_validated_and_accepts_valid_value() {
        let subject = Uuid::from_u128(1);
        let outcome = capture(
            &fixed_subject_schema(),
            &registry_with_self(subject),
            &json!({}),
            &changes(&[("/self/salary", json!(100))]),
            &salary_validator(),
            None,
        )
        .await
        .unwrap();
        // No plaintext changes — yet the secret value was validated and the
        // partition produced.
        assert!(outcome.plaintext.is_empty());
        assert_eq!(outcome.secret.len(), 1);
        assert_eq!(outcome.secret[0].role_path, ptr("/self"));
    }

    #[tokio::test]
    async fn secret_only_batch_with_invalid_value_is_rejected_before_encryption() {
        let subject = Uuid::from_u128(1);
        // `salary` must be an integer; a string violates the schema. With no
        // plaintext change in the batch, the old plaintext-only guard would
        // have skipped validation entirely.
        let err = capture(
            &fixed_subject_schema(),
            &registry_with_self(subject),
            &json!({}),
            &changes(&[("/self/salary", json!("lots"))]),
            &salary_validator(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CaptureError::InvalidData(_)));
    }

    #[tokio::test]
    async fn secret_path_with_unbound_subject_is_rejected() {
        // The role path is secret per the schema, but no subject is bound.
        let err = capture(
            &schema(),
            &SubjectRegistry::default(),
            &json!({}),
            &changes(&[("/persons/passenger_0/passport", json!("AB123456"))]),
            &NoOpValidator,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CaptureError::SubjectNotResolved(role) if role == ptr("/persons/passenger_0"))
        );
    }
}
