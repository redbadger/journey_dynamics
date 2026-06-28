//! Subject registry and role bindings.
//!
//! A *subject* is a data-subject whose secret attributes are encrypted under
//! their own DEK; the registry records each subject as an `(id, email,
//! forgotten)` triple and maps *role paths* (e.g. `/persons/0`) to the subject
//! bound to that role.
//!
//! The registry owns the reusable spine of the subject lifecycle:
//!
//! - the **decision logic** for register / bind / register-and-bind / forget
//!   (what is idempotent, what conflicts), and
//! - the **invariant lookup** [`SubjectRegistry::resolve_active`]: a secret path
//!   may only be written if its role is bound to a subject that has not been
//!   forgotten.
//!
//! Event *emission* and event *sourcing* stay with the owning aggregate — the
//! registry only decides and mutates state, it does not know about any
//! particular event enum.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use jsonptr::PointerBuf;

/// Registration record for a data subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectRegistration {
    /// Contact email — used for GDPR erasure lookup.
    pub email: String,
    /// Set to `true` once the subject has been forgotten (shredded).
    pub forgotten: bool,
}

/// Errors produced by the registry's command-decision helpers.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubjectError {
    /// A bind was attempted for a subject that was never registered.
    #[error("Subject not registered — call RegisterSubject first")]
    NotRegistered,
    /// A role path is already bound to a different subject.
    #[error("Role path '{0}' is already bound to a different subject")]
    RolePathConflict(PointerBuf),
}

/// Registered subjects plus the role-path → subject-UUID bindings.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SubjectRegistry {
    /// Registered subjects, keyed by subject UUID.
    subjects: BTreeMap<Uuid, SubjectRegistration>,
    /// Role-path → subject-UUID bindings.
    bindings: BTreeMap<PointerBuf, Uuid>,
}

impl SubjectRegistry {
    // ── queries ────────────────────────────────────────────────────────────

    /// All registered subjects, keyed by UUID.
    #[must_use]
    pub const fn subjects(&self) -> &BTreeMap<Uuid, SubjectRegistration> {
        &self.subjects
    }

    /// All role-path → subject-UUID bindings.
    #[must_use]
    pub const fn bindings(&self) -> &BTreeMap<PointerBuf, Uuid> {
        &self.bindings
    }

    /// Whether `subject_id` has been registered.
    #[must_use]
    pub fn is_registered(&self, subject_id: &Uuid) -> bool {
        self.subjects.contains_key(subject_id)
    }

    /// The subject UUID bound to `role_path`, if any.
    #[must_use]
    pub fn binding(&self, role_path: &PointerBuf) -> Option<Uuid> {
        self.bindings.get(role_path).copied()
    }

    /// Resolve `role_path` to its bound, **non-forgotten** subject.
    ///
    /// Returns `None` when the role is unbound or when its subject has been
    /// forgotten. This is the invariant guarding secret writes: a secret path
    /// may only be written for a subject that is bound and not forgotten.
    #[must_use]
    pub fn resolve_active(&self, role_path: &PointerBuf) -> Option<Uuid> {
        let uuid = *self.bindings.get(role_path)?;
        if self.subjects.get(&uuid).is_some_and(|r| r.forgotten) {
            return None;
        }
        Some(uuid)
    }

    // ── command-decision helpers (decide; do not emit) ──────────────────────

    /// Whether a register should be recorded — `true` if the subject is absent
    /// or registered under a different email.
    #[must_use]
    pub fn needs_registration(&self, subject_id: &Uuid, email: &str) -> bool {
        self.subjects
            .get(subject_id)
            .is_none_or(|reg| reg.email != email)
    }

    /// Whether a forget should be recorded — `true` if the subject is present
    /// and not already forgotten.
    #[must_use]
    pub fn needs_forgetting(&self, subject_id: &Uuid) -> bool {
        self.subjects
            .get(subject_id)
            .is_some_and(|reg| !reg.forgotten)
    }

    /// Decide whether binding `role_path` to `subject_id` should be recorded.
    ///
    /// # Errors
    /// Returns [`SubjectError::RolePathConflict`] if `role_path` is already
    /// bound to a *different* subject.
    ///
    /// Returns `Ok(true)` if a new binding should be recorded, or `Ok(false)`
    /// if `role_path` is already bound to this same subject (idempotent).
    pub fn check_binding(
        &self,
        role_path: &PointerBuf,
        subject_id: &Uuid,
    ) -> Result<bool, SubjectError> {
        match self.bindings.get(role_path) {
            Some(existing) if existing != subject_id => {
                Err(SubjectError::RolePathConflict(role_path.clone()))
            }
            Some(_) => Ok(false),
            None => Ok(true),
        }
    }

    // ── apply mutations ──────────────────────────────────────────────────────

    /// Record a subject registration (upsert: updates the email if it changed).
    pub fn register(&mut self, subject_id: Uuid, email: String) {
        self.subjects
            .entry(subject_id)
            .and_modify(|reg| reg.email.clone_from(&email))
            .or_insert(SubjectRegistration {
                email,
                forgotten: false,
            });
    }

    /// Record a role-path → subject binding.
    pub fn bind(&mut self, role_path: PointerBuf, subject_id: Uuid) {
        self.bindings.insert(role_path, subject_id);
    }

    /// Mark a subject as forgotten. No-op if the subject is unknown.
    pub fn forget(&mut self, subject_id: &Uuid) {
        if let Some(reg) = self.subjects.get_mut(subject_id) {
            reg.forgotten = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn path(s: &str) -> PointerBuf {
        PointerBuf::parse(s).unwrap()
    }

    #[test]
    fn register_is_idempotent_for_same_email() {
        let mut reg = SubjectRegistry::default();
        assert!(reg.needs_registration(&id(1), "a@example.com"));
        reg.register(id(1), "a@example.com".to_string());
        assert!(!reg.needs_registration(&id(1), "a@example.com"));
        // Email change is observable and should re-register.
        assert!(reg.needs_registration(&id(1), "b@example.com"));
    }

    #[test]
    fn check_binding_new_idempotent_and_conflict() {
        let mut reg = SubjectRegistry::default();
        let role = path("/persons/0");
        assert_eq!(reg.check_binding(&role, &id(1)), Ok(true));
        reg.bind(role.clone(), id(1));
        // Same subject → idempotent.
        assert_eq!(reg.check_binding(&role, &id(1)), Ok(false));
        // Different subject → conflict.
        assert_eq!(
            reg.check_binding(&role, &id(2)),
            Err(SubjectError::RolePathConflict(role.clone()))
        );
    }

    #[test]
    fn resolve_active_requires_bound_and_not_forgotten() {
        let mut reg = SubjectRegistry::default();
        let role = path("/persons/0");
        // Unbound → None.
        assert_eq!(reg.resolve_active(&role), None);

        reg.register(id(1), "a@example.com".to_string());
        reg.bind(role.clone(), id(1));
        assert_eq!(reg.resolve_active(&role), Some(id(1)));

        // After forgetting, the path can no longer be resolved.
        assert!(reg.needs_forgetting(&id(1)));
        reg.forget(&id(1));
        assert!(!reg.needs_forgetting(&id(1)));
        assert_eq!(reg.resolve_active(&role), None);
    }
}
