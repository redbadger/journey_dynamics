use cqrs_es::{EventEnvelope, Query};
use sqlx::{Pool, Postgres, Row};
use uuid::Uuid;

use crate::{
    domain::{events::JourneyEvent, journey::Journey},
    queries::{JourneyState, JourneyView, PersonView, WorkflowDecisionView},
};

/// A structured database view repository for journeys.
#[derive(Clone)]
pub struct StructuredJourneyViewRepository {
    pool: Pool<Postgres>,
}

impl StructuredJourneyViewRepository {
    #[must_use]
    pub const fn new(pool: Pool<Postgres>) -> Self {
        Self { pool }
    }

    /// Load a journey view by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load(&self, journey_id: &Uuid) -> Result<Option<JourneyView>, sqlx::Error> {
        let mut tx = self.begin_repeatable_read().await?;
        self.load_in_tx(&mut tx, journey_id).await
    }

    /// Inner load: runs all three queries against an already-open transaction.
    /// The caller is responsible for setting the desired isolation level before
    /// calling this.
    async fn load_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        journey_id: &Uuid,
    ) -> Result<Option<JourneyView>, sqlx::Error> {
        let journey_row = sqlx::query(
            r"
            SELECT id, state, shared_data, current_step, version
            FROM journey_view
            WHERE id = $1
            ",
        )
        .bind(journey_id)
        .fetch_optional(&mut **tx)
        .await?;

        let Some(row) = journey_row else {
            return Ok(None);
        };

        let id: Uuid = row.get("id");
        let state = match row.get::<String, _>("state").as_str() {
            "Complete" => JourneyState::Complete,
            _ => JourneyState::InProgress,
        };
        let current_step: Option<String> = row.get("current_step");
        let shared_data: serde_json::Value = row.get("shared_data");

        let workflow_row = sqlx::query(
            r"
            SELECT suggested_actions
            FROM journey_workflow_decision
            WHERE journey_id = $1 AND is_latest = TRUE
            ORDER BY created_at DESC
            LIMIT 1
            ",
        )
        .bind(journey_id)
        .fetch_optional(&mut **tx)
        .await?;

        let latest_workflow_decision = workflow_row.map(|r| WorkflowDecisionView {
            suggested_actions: r.get("suggested_actions"),
        });

        let persons = self.load_persons_with(&mut **tx, journey_id).await?;

        Ok(Some(JourneyView {
            id,
            state,
            shared_data,
            current_step,
            latest_workflow_decision,
            persons,
        }))
    }

    /// Load all journey views.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load_all(&self) -> Result<Vec<JourneyView>, sqlx::Error> {
        let mut tx = self.begin_repeatable_read().await?;

        let rows = sqlx::query("SELECT id FROM journey_view ORDER BY created_at DESC")
            .fetch_all(&mut *tx)
            .await?;

        let mut views = Vec::new();
        for row in rows {
            let id: Uuid = row.get("id");
            if let Some(view) = self.load_in_tx(&mut tx, &id).await? {
                views.push(view);
            }
        }
        Ok(views)
    }

    /// Load all person slots for a journey, ordered by `person_ref`.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load_persons(&self, journey_id: &Uuid) -> Result<Vec<PersonView>, sqlx::Error> {
        self.load_persons_with(&self.pool, journey_id).await
    }

    /// Open a transaction and immediately promote it to REPEATABLE READ.
    ///
    /// REPEATABLE READ ensures that all queries within the transaction see the
    /// same committed snapshot. The default READ COMMITTED gives each statement
    /// a fresh snapshot, which can produce torn reads across multi-query loads.
    async fn begin_repeatable_read(&self) -> Result<sqlx::Transaction<'_, Postgres>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }

    async fn load_persons_with<'e, E>(
        &self,
        executor: E,
        journey_id: &Uuid,
    ) -> Result<Vec<PersonView>, sqlx::Error>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        sqlx::query_as::<_, PersonView>(
            r"
            SELECT journey_id, person_ref, subject_id,
                   name, email, phone, details, forgotten
            FROM journey_person
            WHERE journey_id = $1
            ORDER BY person_ref
            ",
        )
        .bind(journey_id)
        .fetch_all(executor)
        .await
    }

    /// Find journeys that have a non-forgotten person with the given email address.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn find_by_email(&self, email: &str) -> Result<Vec<JourneyView>, sqlx::Error> {
        let mut tx = self.begin_repeatable_read().await?;

        let rows = sqlx::query(
            r"
            SELECT DISTINCT journey_id
            FROM journey_person
            WHERE email = $1 AND forgotten = FALSE
            ",
        )
        .bind(email)
        .fetch_all(&mut *tx)
        .await?;

        let mut views = Vec::new();
        for row in rows {
            let id: Uuid = row.get("journey_id");
            if let Some(view) = self.load_in_tx(&mut tx, &id).await? {
                views.push(view);
            }
        }
        Ok(views)
    }

    /// Load all person slots across all journeys, ordered by `(journey_id, person_ref)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load_all_persons(&self) -> Result<Vec<PersonView>, sqlx::Error> {
        sqlx::query_as::<_, PersonView>(
            r"
            SELECT journey_id, person_ref, subject_id,
                   name, email, phone, details, forgotten
            FROM journey_person
            ORDER BY journey_id, person_ref
            ",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// Find all journey aggregate IDs that have referenced the given subject.
    ///
    /// Queries `PersonCaptured` and `PersonDetailsUpdated` events in the event store directly —
    /// both carry `subject_id` in plaintext, so no decryption is needed.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn find_journeys_by_subject(
        &self,
        subject_id: &Uuid,
    ) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query(
            r"
            SELECT DISTINCT aggregate_id
            FROM events
            WHERE aggregate_type = 'Journey'
              AND (
                (event_type = 'PersonCaptured'
                 AND payload -> 'PersonCaptured' ->> 'subject_id' = $1)
                OR
                (event_type = 'PersonDetailsUpdated'
                 AND payload -> 'PersonDetailsUpdated' ->> 'subject_id' = $1)
              )
            ",
        )
        .bind(subject_id.to_string())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("aggregate_id"))
            .collect())
    }

    /// Find all non-forgotten subject IDs associated with the given email address.
    ///
    /// The comparison is case-insensitive. Subjects that have already been forgotten
    /// are excluded, so a duplicate erasure request is a safe no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn find_subjects_by_email(&self, email: &str) -> Result<Vec<Uuid>, sqlx::Error> {
        // subject_lookup is the authoritative email → subject_id index.
        // Rows are deleted on shredding, so no forgotten-filter is needed.
        let rows = sqlx::query(
            r"
            SELECT subject_id
            FROM subject_lookup
            WHERE email_lower = lower($1)
            ",
        )
        .bind(email)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| r.get::<Uuid, _>("subject_id"))
            .collect())
    }

    /// Remove the `subject_lookup` row for `subject_id`.
    ///
    /// Called by the shredding route handler after the DEK has been deleted.
    /// The deletion is the GDPR erasure of the email address from this table.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn delete_subject_lookup(&self, subject_id: &Uuid) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM subject_lookup WHERE subject_id = $1")
            .bind(subject_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    fn parse_journey_id(view_id: &str) -> Result<Uuid, sqlx::Error> {
        Uuid::parse_str(view_id).map_err(|e| {
            sqlx::Error::Decode(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid UUID: {e}"),
            )))
        })
    }

    #[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
    async fn apply_event_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Postgres>,
        journey_id: Uuid,
        event: &EventEnvelope<Journey>,
    ) -> Result<(), sqlx::Error> {
        match &event.payload {
            JourneyEvent::Started { id } => {
                sqlx::query(
                    r"
                    INSERT INTO journey_view (id, state, current_step, version)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (id) DO NOTHING
                    ",
                )
                .bind(id)
                .bind("InProgress")
                .bind::<Option<String>>(None)
                .bind(event.sequence as i64)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::Modified { step: _, data } => {
                // Merge new data into shared_data via JSONB concatenation.
                // shared_data never contains PII and is never cleared by shredding.
                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET shared_data = shared_data || $2,
                        version     = $3,
                        updated_at  = CURRENT_TIMESTAMP
                    WHERE id = $1
                    ",
                )
                .bind(journey_id)
                .bind(data)
                .bind(event.sequence as i64)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::PersonCaptured {
                person_ref,
                subject_id,
                name,
                email,
                phone,
            } => {
                // Upsert on the composite PK (journey_id, person_ref).
                // If the slot already exists (identity field update for the same subject),
                // overwrite identity fields but leave details and forgotten untouched.
                sqlx::query(
                    r"
                    INSERT INTO journey_person
                        (journey_id, person_ref, subject_id, name, email, phone)
                    VALUES ($1, $2, $3, $4, $5, $6)
                    ON CONFLICT (journey_id, person_ref) DO UPDATE
                    SET subject_id = $3,
                        name       = $4,
                        email      = $5,
                        phone      = $6,
                        updated_at = CURRENT_TIMESTAMP
                    ",
                )
                .bind(journey_id)
                .bind(person_ref)
                .bind(subject_id)
                .bind(name)
                .bind(email)
                .bind(phone)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET version = $1, updated_at = CURRENT_TIMESTAMP
                    WHERE id = $2
                    ",
                )
                .bind(event.sequence as i64)
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::PersonDetailsUpdated {
                person_ref, data, ..
            } => {
                // Merge new detail fields into the existing JSONB details column.
                sqlx::query(
                    r"
                    UPDATE journey_person
                    SET details    = details || $3,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE journey_id = $1 AND person_ref = $2
                    ",
                )
                .bind(journey_id)
                .bind(person_ref)
                .bind(data)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET version = $1, updated_at = CURRENT_TIMESTAMP
                    WHERE id = $2
                    ",
                )
                .bind(event.sequence as i64)
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::SubjectForgotten { subject_id } => {
                // Null out PII for the specific subject in this journey only.
                // shared_data in journey_view is NOT touched — it never contained PII.
                // Other persons in the same journey are NOT affected.
                sqlx::query(
                    r"
                    UPDATE journey_person
                    SET name       = NULL,
                        email      = NULL,
                        phone      = NULL,
                        details    = '{}',
                        forgotten  = TRUE,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE journey_id = $1 AND subject_id = $2
                    ",
                )
                .bind(journey_id)
                .bind(subject_id)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET version = $1, updated_at = CURRENT_TIMESTAMP
                    WHERE id = $2
                    ",
                )
                .bind(event.sequence as i64)
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::WorkflowEvaluated { suggested_actions } => {
                sqlx::query(
                    r"
                    UPDATE journey_workflow_decision
                    SET is_latest = FALSE
                    WHERE journey_id = $1
                    ",
                )
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    r"
                    INSERT INTO journey_workflow_decision
                        (journey_id, suggested_actions, is_latest)
                    VALUES ($1, $2, TRUE)
                    ",
                )
                .bind(journey_id)
                .bind(suggested_actions)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET version = $1, updated_at = CURRENT_TIMESTAMP
                    WHERE id = $2
                    ",
                )
                .bind(event.sequence as i64)
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::StepProgressed { to_step, .. } => {
                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET current_step = $1,
                        version      = $2,
                        updated_at   = CURRENT_TIMESTAMP
                    WHERE id = $3
                    ",
                )
                .bind(to_step)
                .bind(event.sequence as i64)
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;
            }

            JourneyEvent::Completed => {
                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET state      = $1,
                        version    = $2,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE id = $3
                    ",
                )
                .bind("Complete")
                .bind(event.sequence as i64)
                .bind(journey_id)
                .execute(&mut **tx)
                .await?;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl Query<Journey> for StructuredJourneyViewRepository {
    async fn dispatch(&self, view_id: &str, events: &[EventEnvelope<Journey>]) {
        if events.is_empty() {
            return;
        }

        let journey_id = match Self::parse_journey_id(view_id) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("Invalid journey ID '{view_id}': {e:?}");
                return;
            }
        };

        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                eprintln!("Error starting transaction for journey '{view_id}': {e:?}");
                return;
            }
        };

        for event in events {
            if let Err(e) = self.apply_event_in_tx(&mut tx, journey_id, event).await {
                eprintln!("Error applying event to journey view '{view_id}': {e:?}");
                // tx is dropped here, rolling back all events in this batch.
                return;
            }
        }

        if let Err(e) = tx.commit().await {
            eprintln!("Error committing journey view update for '{view_id}': {e:?}");
        }
    }
}
