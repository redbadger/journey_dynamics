//! [`CompositeKekProvider`] — route wrap/unwrap across two [`KekProvider`]s.
//!
//! This is the migration primitive for moving DEKs from one KEK to another (for
//! example a static in-process KEK to a cloud-KMS KEK) with zero downtime:
//!
//! - New DEKs are always wrapped under the **primary** provider.
//! - Existing DEKs are unwrapped by whichever provider recognises the stored
//!   `kek_id` — so both old and new wrapped DEKs remain readable while a re-wrap
//!   sweep migrates everything onto the primary.
//!
//! Ownership of a `kek_id` is decided by [`KekProvider::by_id`]: a provider
//! "owns" an id iff it returns `Some` for it. This keeps the composite fully
//! provider-agnostic — there is no hard-coded id scheme here.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    cipher::KeyMaterial,
    kek::{KekError, KekHandle, KekProvider, WrappedDek},
};

/// Routes across a `primary` (all new wraps) and a `secondary` (legacy unwraps).
///
/// `current()`/`wrap()` always use the primary; `unwrap()`/`by_id()` consult
/// both, so a DEK wrapped under either provider stays readable.
pub struct CompositeKekProvider {
    primary: Arc<dyn KekProvider>,
    secondary: Arc<dyn KekProvider>,
}

impl CompositeKekProvider {
    /// Create a composite. New DEKs wrap under `primary`; `secondary` exists only
    /// to unwrap DEKs it still owns (e.g. the retiring KEK during a migration).
    #[must_use]
    pub fn new(primary: Arc<dyn KekProvider>, secondary: Arc<dyn KekProvider>) -> Self {
        Self { primary, secondary }
    }

    /// The provider that owns `id`, if either does.
    fn owner_of(&self, id: &str) -> Option<&Arc<dyn KekProvider>> {
        if self.primary.by_id(id).is_some() {
            Some(&self.primary)
        } else if self.secondary.by_id(id).is_some() {
            Some(&self.secondary)
        } else {
            None
        }
    }
}

#[async_trait]
impl KekProvider for CompositeKekProvider {
    fn current(&self) -> KekHandle {
        self.primary.current()
    }

    fn by_id(&self, id: &str) -> Option<KekHandle> {
        self.primary.by_id(id).or_else(|| self.secondary.by_id(id))
    }

    async fn wrap(&self, kek: &KekHandle, dek: &KeyMaterial) -> Result<WrappedDek, KekError> {
        // Callers pass `current()` (the primary) in the normal path; route by
        // ownership so an explicit legacy handle still works if ever supplied.
        match self.owner_of(&kek.id) {
            Some(provider) => provider.wrap(kek, dek).await,
            None => Err(KekError::UnknownVersion(kek.id.clone())),
        }
    }

    async fn unwrap(&self, wrapped: &WrappedDek) -> Result<KeyMaterial, KekError> {
        match self.owner_of(&wrapped.kek_id) {
            Some(provider) => provider.unwrap(wrapped).await,
            None => Err(KekError::UnknownVersion(wrapped.kek_id.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cipher::FieldCipher, kek::StaticKekProvider};

    fn provider(id: &str, byte: u8) -> Arc<dyn KekProvider> {
        Arc::new(StaticKekProvider::single(id, vec![byte; 32]).expect("32-byte kek"))
    }

    #[tokio::test]
    async fn wraps_under_primary_and_routes_unwrap_by_owner() {
        // Stand-ins: "kms:test" plays the primary (new) KEK, "hr:v1" the legacy one.
        let primary = provider("kms:test", 1);
        let legacy = provider("hr:v1", 2);
        let composite = CompositeKekProvider::new(Arc::clone(&primary), Arc::clone(&legacy));

        // New wraps go under the primary.
        let dek = FieldCipher::generate_dek();
        let fresh = composite.wrap(&composite.current(), &dek).await.unwrap();
        assert_eq!(fresh.kek_id, "kms:test");

        // A DEK wrapped by the legacy provider still unwraps through the composite.
        let legacy_dek = FieldCipher::generate_dek();
        let legacy_wrapped = legacy.wrap(&legacy.current(), &legacy_dek).await.unwrap();
        let recovered = composite.unwrap(&legacy_wrapped).await.unwrap();
        assert_eq!(*recovered.key, *legacy_dek.key);

        // And a primary-wrapped DEK round-trips too.
        let recovered = composite.unwrap(&fresh).await.unwrap();
        assert_eq!(*recovered.key, *dek.key);
    }

    #[tokio::test]
    async fn by_id_is_the_union_and_unknown_ids_error() {
        let composite = CompositeKekProvider::new(provider("kms:test", 1), provider("hr:v1", 2));
        assert!(composite.by_id("kms:test").is_some());
        assert!(composite.by_id("hr:v1").is_some());
        assert!(composite.by_id("nope").is_none());

        let orphan = WrappedDek {
            key_id: uuid::Uuid::new_v4(),
            kek_id: "retired:v0".to_string(),
            wrapped_key: vec![0; 40],
        };
        assert!(matches!(
            composite.unwrap(&orphan).await,
            Err(KekError::UnknownVersion(_))
        ));
    }
}
