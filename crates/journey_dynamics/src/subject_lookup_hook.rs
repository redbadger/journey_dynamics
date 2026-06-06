//! [`PersistHook`] that maintains the `subject_lookup` table.
//!
//! For every `PersonCaptured` event, a `(subject_id, email_lower)` row is
//! written to `subject_lookup` inside the same transaction as the event
//! INSERT.  This guarantees the email → `subject_id` mapping is always
//! consistent with the event store — no crash window.

use async_trait::async_trait;
use cqrs_es::persist::{PersistenceError, SerializedEvent};
use cqrs_es_crypto::PersistHook;
use uuid::Uuid;

/// Upserts a `subject_lookup` row for every `PersonCaptured` event,
/// atomically with the event INSERT.
pub struct SubjectLookupHook;

#[async_trait]
impl PersistHook for SubjectLookupHook {
    async fn on_persist(
        &self,
        events: &[SerializedEvent],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), PersistenceError> {
        for event in events {
            // Extract (subject_id, email) from either PersonCaptured (legacy)
            // or SubjectCaptured (new).
            let (subject_id, email) = match event.event_type.as_str() {
                "PersonCaptured" => {
                    let Some(inner) = event.payload.get("PersonCaptured") else {
                        continue;
                    };
                    let Some(subject_id) = inner
                        .get("subject_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<Uuid>().ok())
                    else {
                        continue;
                    };
                    let Some(email) = inner.get("email").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    (subject_id, email)
                }
                "SubjectCaptured" => {
                    let Some(inner) = event.payload.get("SubjectCaptured") else {
                        continue;
                    };
                    let Some(subject_id) = inner
                        .get("subject_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<Uuid>().ok())
                    else {
                        continue;
                    };
                    let Some(email) = inner.get("email").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    (subject_id, email)
                }
                _ => continue,
            };

            // Upsert — re-capture with a new email updates the stored address.
            sqlx::query(
                "INSERT INTO subject_lookup (subject_id, email_lower) \
                 VALUES ($1, lower($2)) \
                 ON CONFLICT (subject_id) DO UPDATE SET email_lower = lower($2)",
            )
            .bind(subject_id)
            .bind(email)
            .execute(&mut **tx)
            .await
            .map_err(|e| PersistenceError::UnknownError(Box::new(e)))?;
        }

        Ok(())
    }
}
