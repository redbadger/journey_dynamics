use cqrs_es::{EventEnvelope, Query};
use sqlx::{Pool, Postgres, Row};
use uuid::Uuid;

use crate::domain::events::JourneyEvent;
use crate::domain::journey::Journey;
use crate::queries::{JourneyState, JourneyView, PersonView, WorkflowDecisionView};

/// A structured database view repository for journeys.
#[derive(Clone)]
pub struct StructuredJourneyViewRepository {
    pool: Pool<Postgres>,
}

impl StructuredJourneyViewRepository {
    #[must_use]
    pub fn new(pool: Pool<Postgres>) -> Self {
        Self { pool }
    }

    /// Load a journey view by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load(&self, journey_id: &Uuid) -> Result<Option<JourneyView>, sqlx::Error> {
        let journey_row = sqlx::query(
            r"
            SELECT id, state, shared_data, current_step, version
            FROM journey_view
            WHERE id = $1
            ",
        )
        .bind(journey_id)
        .fetch_optional(&self.pool)
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
        .fetch_optional(&self.pool)
        .await?;

        let latest_workflow_decision = workflow_row.map(|r| WorkflowDecisionView {
            suggested_actions: r.get("suggested_actions"),
        });

        let persons = self.load_persons(journey_id).await?;

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
        let rows = sqlx::query("SELECT id FROM journey_view ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;

        let mut views = Vec::new();
        for row in rows {
            let id: Uuid = row.get("id");
            if let Some(view) = self.load(&id).await? {
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
        .fetch_all(&self.pool)
        .await
    }

    /// Find journeys that have a non-forgotten person with the given email address.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn find_by_email(&self, email: &str) -> Result<Vec<JourneyView>, sqlx::Error> {
        let rows = sqlx::query(
            r"
            SELECT DISTINCT journey_id
            FROM journey_person
            WHERE email = $1 AND forgotten = FALSE
            ",
        )
        .bind(email)
        .fetch_all(&self.pool)
        .await?;

        let mut views = Vec::new();
        for row in rows {
            let id: Uuid = row.get("journey_id");
            if let Some(view) = self.load(&id).await? {
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

    #[allow(clippy::too_many_lines, clippy::cast_possible_wrap)]
    async fn update_view(
        &self,
        view_id: &str,
        event: &EventEnvelope<Journey>,
    ) -> Result<(), sqlx::Error> {
        let journey_id = Uuid::parse_str(view_id).map_err(|e| {
            sqlx::Error::Decode(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Invalid UUID: {e}"),
            )))
        })?;

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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
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
                .execute(&self.pool)
                .await?;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl Query<Journey> for StructuredJourneyViewRepository {
    async fn dispatch(&self, view_id: &str, events: &[EventEnvelope<Journey>]) {
        for event in events {
            if let Err(e) = self.update_view(view_id, event).await {
                eprintln!("Error updating journey view {view_id}: {e:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    async fn setup_test_db() -> Pool<Postgres> {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgres@localhost:5432/journey_dynamics".to_string()
        });

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to database");

        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("Failed to run database migrations");

        pool
    }

    async fn cleanup_test_journey(pool: &Pool<Postgres>, journey_id: &Uuid) {
        let _ = sqlx::query("DELETE FROM journey_view WHERE id = $1")
            .bind(journey_id)
            .execute(pool)
            .await;
    }

    // ── Journey lifecycle ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_journey_started_event() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[EventEnvelope {
                aggregate_id: journey_id.to_string(),
                sequence: 1,
                payload: JourneyEvent::Started { id: journey_id },
                metadata: std::collections::HashMap::default(),
            }],
        )
        .await;

        let view = repo.load(&journey_id).await.unwrap().unwrap();
        assert_eq!(view.id, journey_id);
        assert_eq!(view.state, JourneyState::InProgress);
        assert_eq!(view.shared_data, json!({}));
        assert!(view.current_step.is_none());

        cleanup_test_journey(&pool, &journey_id).await;
    }

    #[tokio::test]
    async fn test_journey_full_lifecycle() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::Modified {
                        step: "search".to_string(),
                        data: json!({"origin": "LHR", "destination": "JFK"}),
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 3,
                    payload: JourneyEvent::WorkflowEvaluated {
                        suggested_actions: vec!["passenger_details".to_string()],
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 4,
                    payload: JourneyEvent::StepProgressed {
                        from_step: None,
                        to_step: "passenger_details".to_string(),
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 5,
                    payload: JourneyEvent::Completed,
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        let view = repo.load(&journey_id).await.unwrap().unwrap();
        assert_eq!(view.id, journey_id);
        assert_eq!(view.state, JourneyState::Complete);
        assert_eq!(view.shared_data["origin"], json!("LHR"));
        assert_eq!(view.shared_data["destination"], json!("JFK"));
        assert_eq!(view.current_step, Some("passenger_details".to_string()));
        assert!(view.latest_workflow_decision.is_some());

        cleanup_test_journey(&pool, &journey_id).await;
    }

    // ── PersonCaptured ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_person_captured_event() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        name: "Alice Smith".to_string(),
                        email: "alice@example.com".to_string(),
                        phone: Some("+44-7700-900000".to_string()),
                    },
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        let persons = repo.load_persons(&journey_id).await.unwrap();
        assert_eq!(persons.len(), 1);

        let p = &persons[0];
        assert_eq!(p.journey_id, journey_id);
        assert_eq!(p.person_ref, "passenger_0");
        assert_eq!(p.subject_id, subject_id);
        assert_eq!(p.name.as_deref(), Some("Alice Smith"));
        assert_eq!(p.email.as_deref(), Some("alice@example.com"));
        assert_eq!(p.phone.as_deref(), Some("+44-7700-900000"));
        assert_eq!(p.details, json!({}));
        assert!(!p.forgotten);

        cleanup_test_journey(&pool, &journey_id).await;
    }

    #[tokio::test]
    async fn test_multiple_persons_captured() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id: subject_a,
                        name: "Alice Smith".to_string(),
                        email: "alice@example.com".to_string(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 3,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_1".to_string(),
                        subject_id: subject_b,
                        name: "Bob Jones".to_string(),
                        email: "bob@example.com".to_string(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        let persons = repo.load_persons(&journey_id).await.unwrap();
        assert_eq!(persons.len(), 2);
        // Results are ordered by person_ref
        assert_eq!(persons[0].person_ref, "passenger_0");
        assert_eq!(persons[0].subject_id, subject_a);
        assert_eq!(persons[1].person_ref, "passenger_1");
        assert_eq!(persons[1].subject_id, subject_b);

        cleanup_test_journey(&pool, &journey_id).await;
    }

    #[tokio::test]
    async fn test_person_captured_updates_identity_fields() {
        // A second PersonCaptured for the same person_ref must update, not insert.
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        name: "Alice Smith".to_string(),
                        email: "alice@example.com".to_string(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 3,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        name: "Alice J. Smith".to_string(),
                        email: "alice.new@example.com".to_string(),
                        phone: Some("+44-7700-900001".to_string()),
                    },
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        let persons = repo.load_persons(&journey_id).await.unwrap();
        assert_eq!(
            persons.len(),
            1,
            "second PersonCaptured must update, not insert"
        );
        assert_eq!(persons[0].name.as_deref(), Some("Alice J. Smith"));
        assert_eq!(persons[0].email.as_deref(), Some("alice.new@example.com"));
        assert_eq!(persons[0].phone.as_deref(), Some("+44-7700-900001"));

        cleanup_test_journey(&pool, &journey_id).await;
    }

    // ── PersonDetailsUpdated ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_person_details_updated() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        name: "Alice Smith".to_string(),
                        email: "alice@example.com".to_string(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 3,
                    payload: JourneyEvent::PersonDetailsUpdated {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        data: json!({
                            "passportNumber": "GB123456789",
                            "dateOfBirth":    "1990-05-15"
                        }),
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 4,
                    payload: JourneyEvent::PersonDetailsUpdated {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        data: json!({ "nationality": "GB" }),
                    },
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        let persons = repo.load_persons(&journey_id).await.unwrap();
        assert_eq!(persons.len(), 1);
        let p = &persons[0];
        assert_eq!(p.details["passportNumber"], json!("GB123456789"));
        assert_eq!(p.details["dateOfBirth"], json!("1990-05-15"));
        assert_eq!(p.details["nationality"], json!("GB"));

        cleanup_test_journey(&pool, &journey_id).await;
    }

    // ── SubjectForgotten ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_subject_forgotten_only_affects_target_person() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();
        let subject_a = Uuid::new_v4();
        let subject_b = Uuid::new_v4();

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::Modified {
                        step: "search".to_string(),
                        data: json!({"origin": "LHR", "destination": "JFK"}),
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 3,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id: subject_a,
                        name: "Alice Smith".to_string(),
                        email: "alice@example.com".to_string(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 4,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_1".to_string(),
                        subject_id: subject_b,
                        name: "Bob Jones".to_string(),
                        email: "bob@example.com".to_string(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 5,
                    payload: JourneyEvent::SubjectForgotten {
                        subject_id: subject_a,
                    },
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        // shared_data must be completely untouched
        let view = repo.load(&journey_id).await.unwrap().unwrap();
        assert_eq!(view.shared_data["origin"], json!("LHR"));
        assert_eq!(view.shared_data["destination"], json!("JFK"));

        let persons = repo.load_persons(&journey_id).await.unwrap();
        assert_eq!(persons.len(), 2);

        let pa = persons
            .iter()
            .find(|p| p.person_ref == "passenger_0")
            .unwrap();
        assert!(pa.forgotten, "passenger_0 must be marked forgotten");
        assert!(pa.name.is_none(), "name must be nulled");
        assert!(pa.email.is_none(), "email must be nulled");
        assert!(pa.phone.is_none(), "phone must be nulled");
        assert_eq!(pa.details, json!({}), "details must be cleared");

        let pb = persons
            .iter()
            .find(|p| p.person_ref == "passenger_1")
            .unwrap();
        assert!(!pb.forgotten, "passenger_1 must NOT be forgotten");
        assert_eq!(pb.name.as_deref(), Some("Bob Jones"));
        assert_eq!(pb.email.as_deref(), Some("bob@example.com"));

        cleanup_test_journey(&pool, &journey_id).await;
    }

    // ── find_by_email ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_find_by_email() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id_1 = Uuid::new_v4();
        let journey_id_2 = Uuid::new_v4();
        let unique_email = format!("alice+{}@example.com", Uuid::new_v4());

        // Two journeys, both containing the same email address.
        for journey_id in [journey_id_1, journey_id_2] {
            repo.dispatch(
                &journey_id.to_string(),
                &[
                    EventEnvelope {
                        aggregate_id: journey_id.to_string(),
                        sequence: 1,
                        payload: JourneyEvent::Started { id: journey_id },
                        metadata: std::collections::HashMap::default(),
                    },
                    EventEnvelope {
                        aggregate_id: journey_id.to_string(),
                        sequence: 2,
                        payload: JourneyEvent::PersonCaptured {
                            person_ref: "passenger_0".to_string(),
                            subject_id: Uuid::new_v4(),
                            name: "Alice Smith".to_string(),
                            email: unique_email.clone(),
                            phone: None,
                        },
                        metadata: std::collections::HashMap::default(),
                    },
                ],
            )
            .await;
        }

        let journeys = repo.find_by_email(&unique_email).await.unwrap();
        assert_eq!(journeys.len(), 2);

        cleanup_test_journey(&pool, &journey_id_1).await;
        cleanup_test_journey(&pool, &journey_id_2).await;
    }

    #[tokio::test]
    async fn test_find_by_email_excludes_forgotten_persons() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let unique_email = format!("forgotten+{}@example.com", Uuid::new_v4());

        repo.dispatch(
            &journey_id.to_string(),
            &[
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 1,
                    payload: JourneyEvent::Started { id: journey_id },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 2,
                    payload: JourneyEvent::PersonCaptured {
                        person_ref: "passenger_0".to_string(),
                        subject_id,
                        name: "Alice Smith".to_string(),
                        email: unique_email.clone(),
                        phone: None,
                    },
                    metadata: std::collections::HashMap::default(),
                },
                EventEnvelope {
                    aggregate_id: journey_id.to_string(),
                    sequence: 3,
                    payload: JourneyEvent::SubjectForgotten { subject_id },
                    metadata: std::collections::HashMap::default(),
                },
            ],
        )
        .await;

        let journeys = repo.find_by_email(&unique_email).await.unwrap();
        assert!(
            journeys.is_empty(),
            "forgotten subject must not appear in email search results"
        );

        cleanup_test_journey(&pool, &journey_id).await;
    }
}
