use async_trait::async_trait;
use cqrs_es::{EventEnvelope, Query};
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row};
use uuid::Uuid;

use crate::domain::events::JourneyEvent;
use crate::domain::journey::Journey;
use crate::queries::{JourneyState, JourneyView, WorkflowDecisionView};

/// Person data captured during a journey
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PersonView {
    pub journey_id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
}

/// A structured database view repository for journeys that persists data
/// to properly structured SQL tables instead of JSON blobs.
#[derive(Clone)]
pub struct StructuredJourneyViewRepository {
    pool: Pool<Postgres>,
}

impl StructuredJourneyViewRepository {
    #[must_use]
    pub fn new(pool: Pool<Postgres>) -> Self {
        Self { pool }
    }

    /// Load a journey view by ID from the structured database
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load(&self, journey_id: &Uuid) -> Result<Option<JourneyView>, sqlx::Error> {
        // Load the main journey record
        let journey_row = sqlx::query(
            r"
            SELECT id, state, current_step, version, created_at, updated_at
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
        let state_str: String = row.get("state");
        let state = match state_str.as_str() {
            "Complete" => JourneyState::Complete,
            _ => JourneyState::InProgress,
        };
        let current_step: Option<String> = row.get("current_step");
        let accumulated_data: serde_json::Value = row.get("accumulated_data");

        // Load latest workflow decision
        let workflow_decision_row = sqlx::query(
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

        let latest_workflow_decision = workflow_decision_row.map(|row| {
            let suggested_actions: Vec<String> = row.get("suggested_actions");
            WorkflowDecisionView { suggested_actions }
        });

        Ok(Some(JourneyView {
            id,
            state,
            accumulated_data,
            current_step,
            latest_workflow_decision,
        }))
    }

    /// Load all journey views from the database
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load_all(&self) -> Result<Vec<JourneyView>, sqlx::Error> {
        let journey_ids = sqlx::query(
            r"
            SELECT id
            FROM journey_view
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut views = Vec::new();
        for row in journey_ids {
            let id: Uuid = row.get("id");
            if let Some(view) = self.load(&id).await? {
                views.push(view);
            }
        }

        Ok(views)
    }

    /// Load person data for a journey
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load_person(&self, journey_id: &Uuid) -> Result<Option<PersonView>, sqlx::Error> {
        let person = sqlx::query_as::<_, PersonView>(
            r"
            SELECT journey_id, name, email, phone
            FROM journey_person
            WHERE journey_id = $1
            ",
        )
        .bind(journey_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(person)
    }

    /// Find journeys by email address
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn find_by_email(&self, email: &str) -> Result<Vec<JourneyView>, sqlx::Error> {
        let journey_ids = sqlx::query(
            r"
            SELECT journey_id
            FROM journey_person
            WHERE email = $1
            ORDER BY created_at DESC
            ",
        )
        .bind(email)
        .fetch_all(&self.pool)
        .await?;

        let mut views = Vec::new();
        for row in journey_ids {
            let id: Uuid = row.get("journey_id");
            if let Some(view) = self.load(&id).await? {
                views.push(view);
            }
        }

        Ok(views)
    }

    /// Load all persons from all journeys
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub async fn load_all_persons(&self) -> Result<Vec<PersonView>, sqlx::Error> {
        let persons = sqlx::query_as::<_, PersonView>(
            r"
            SELECT journey_id, name, email, phone
            FROM journey_person
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(persons)
    }

    /// Update the journey view based on an event
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
                // Insert new journey record
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
                // Update accumulated_data by merging new data
                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET accumulated_data = accumulated_data || $2,
                        version = $3,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE id = $1
                    ",
                )
                .bind(journey_id)
                .bind(data)
                .bind(event.sequence as i64)
                .execute(&self.pool)
                .await?;
            }

            JourneyEvent::WorkflowEvaluated { suggested_actions } => {
                // Mark all previous decisions as not latest
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

                // Insert new workflow decision
                sqlx::query(
                    r"
                    INSERT INTO journey_workflow_decision (journey_id, suggested_actions, is_latest)
                    VALUES ($1, $2, TRUE)
                    ",
                )
                .bind(journey_id)
                .bind(suggested_actions)
                .execute(&self.pool)
                .await?;

                // Update version and timestamp
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

            JourneyEvent::PersonCaptured { name, email, phone } => {
                // Insert or update person data in journey_person table
                sqlx::query(
                    r"
                    INSERT INTO journey_person (journey_id, name, email, phone)
                    VALUES ($1, $2, $3, $4)
                    ON CONFLICT (journey_id) DO UPDATE
                    SET name = $2, email = $3, phone = $4, updated_at = CURRENT_TIMESTAMP
                    ",
                )
                .bind(journey_id)
                .bind(name)
                .bind(email)
                .bind(phone)
                .execute(&self.pool)
                .await?;

                // Update version and timestamp on journey_view
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

            JourneyEvent::StepProgressed {
                from_step: _,
                to_step,
            } => {
                // Update current step
                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET current_step = $1, version = $2, updated_at = CURRENT_TIMESTAMP
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
                // Update state to complete
                sqlx::query(
                    r"
                    UPDATE journey_view
                    SET state = $1, version = $2, updated_at = CURRENT_TIMESTAMP
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

#[async_trait]
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

        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to database")
    }

    async fn cleanup_test_journey(pool: &Pool<Postgres>, journey_id: &Uuid) {
        let _ = sqlx::query("DELETE FROM journey_view WHERE id = $1")
            .bind(journey_id)
            .execute(pool)
            .await;
    }

    #[tokio::test]
    #[ignore = "Only run with a real database"]
    async fn test_journey_started_event() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();

        let event = EventEnvelope {
            aggregate_id: journey_id.to_string(),
            sequence: 1,
            payload: JourneyEvent::Started { id: journey_id },
            metadata: std::collections::HashMap::default(),
        };

        repo.dispatch(&journey_id.to_string(), &[event]).await;

        let view = repo.load(&journey_id).await.unwrap();
        assert!(view.is_some());

        let view = view.unwrap();
        assert_eq!(view.id, journey_id);
        assert_eq!(view.state, JourneyState::InProgress);

        cleanup_test_journey(&pool, &journey_id).await;
    }

    #[tokio::test]
    #[ignore = "Only run with a real database"]
    async fn test_journey_full_lifecycle() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();

        let events = vec![
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
                    step: "email".to_string(),
                    data: json!("test@example.com"),
                },
                metadata: std::collections::HashMap::default(),
            },
            EventEnvelope {
                aggregate_id: journey_id.to_string(),
                sequence: 3,
                payload: JourneyEvent::WorkflowEvaluated {
                    suggested_actions: vec!["confirmation".to_string(), "continue".to_string()],
                },
                metadata: std::collections::HashMap::default(),
            },
            EventEnvelope {
                aggregate_id: journey_id.to_string(),
                sequence: 4,
                payload: JourneyEvent::StepProgressed {
                    from_step: None,
                    to_step: "confirmation".to_string(),
                },
                metadata: std::collections::HashMap::default(),
            },
            EventEnvelope {
                aggregate_id: journey_id.to_string(),
                sequence: 5,
                payload: JourneyEvent::Completed,
                metadata: std::collections::HashMap::default(),
            },
        ];

        repo.dispatch(&journey_id.to_string(), &events).await;

        let view = repo.load(&journey_id).await.unwrap().unwrap();

        assert_eq!(view.id, journey_id);
        assert_eq!(view.state, JourneyState::Complete);
        assert_eq!(
            view.accumulated_data.get("email"),
            Some(&serde_json::json!("test@example.com"))
        );
        assert_eq!(view.current_step, Some("confirmation".to_string()));
        assert!(view.latest_workflow_decision.is_some());

        cleanup_test_journey(&pool, &journey_id).await;
    }

    #[tokio::test]
    #[ignore = "Only run with a real database"]
    async fn test_person_captured_event() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();

        let events = vec![
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
                    name: "John Doe".to_string(),
                    email: "john@example.com".to_string(),
                    phone: Some("+1234567890".to_string()),
                },
                metadata: std::collections::HashMap::default(),
            },
        ];

        repo.dispatch(&journey_id.to_string(), &events).await;

        // Verify journey was created
        let view = repo.load(&journey_id).await.unwrap();
        assert!(view.is_some());

        // Verify person data was saved
        let person = repo.load_person(&journey_id).await.unwrap();
        assert!(person.is_some());

        let person = person.unwrap();
        assert_eq!(person.journey_id, journey_id);
        assert_eq!(person.name, "John Doe");
        assert_eq!(person.email, "john@example.com");
        assert_eq!(person.phone, Some("+1234567890".to_string()));

        cleanup_test_journey(&pool, &journey_id).await;
    }

    #[tokio::test]
    #[ignore = "Only run with a real database"]
    async fn test_find_by_email() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id_1 = Uuid::new_v4();
        let journey_id_2 = Uuid::new_v4();

        // Create first journey with person
        let events_1 = vec![
            EventEnvelope {
                aggregate_id: journey_id_1.to_string(),
                sequence: 1,
                payload: JourneyEvent::Started { id: journey_id_1 },
                metadata: std::collections::HashMap::default(),
            },
            EventEnvelope {
                aggregate_id: journey_id_1.to_string(),
                sequence: 2,
                payload: JourneyEvent::PersonCaptured {
                    name: "John Doe".to_string(),
                    email: "john@example.com".to_string(),
                    phone: None,
                },
                metadata: std::collections::HashMap::default(),
            },
        ];

        repo.dispatch(&journey_id_1.to_string(), &events_1).await;

        // Create second journey with same email
        let events_2 = vec![
            EventEnvelope {
                aggregate_id: journey_id_2.to_string(),
                sequence: 1,
                payload: JourneyEvent::Started { id: journey_id_2 },
                metadata: std::collections::HashMap::default(),
            },
            EventEnvelope {
                aggregate_id: journey_id_2.to_string(),
                sequence: 2,
                payload: JourneyEvent::PersonCaptured {
                    name: "John Doe".to_string(),
                    email: "john@example.com".to_string(),
                    phone: Some("+9876543210".to_string()),
                },
                metadata: std::collections::HashMap::default(),
            },
        ];

        repo.dispatch(&journey_id_2.to_string(), &events_2).await;

        // Find journeys by email
        let journeys = repo.find_by_email("john@example.com").await.unwrap();
        assert_eq!(journeys.len(), 2);

        cleanup_test_journey(&pool, &journey_id_1).await;
        cleanup_test_journey(&pool, &journey_id_2).await;
    }

    #[tokio::test]
    #[ignore = "Only run with a real database"]
    async fn test_person_update() {
        let pool = setup_test_db().await;
        let repo = StructuredJourneyViewRepository::new(pool.clone());
        let journey_id = Uuid::new_v4();

        // Create journey and capture person
        let events_1 = vec![
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
                    name: "John Doe".to_string(),
                    email: "john@example.com".to_string(),
                    phone: None,
                },
                metadata: std::collections::HashMap::default(),
            },
        ];

        repo.dispatch(&journey_id.to_string(), &events_1).await;

        // Update person data
        let events_2 = vec![EventEnvelope {
            aggregate_id: journey_id.to_string(),
            sequence: 3,
            payload: JourneyEvent::PersonCaptured {
                name: "Jane Smith".to_string(),
                email: "jane@example.com".to_string(),
                phone: Some("+1234567890".to_string()),
            },
            metadata: std::collections::HashMap::default(),
        }];

        repo.dispatch(&journey_id.to_string(), &events_2).await;

        // Verify person data was updated
        let person = repo.load_person(&journey_id).await.unwrap().unwrap();
        assert_eq!(person.name, "Jane Smith");
        assert_eq!(person.email, "jane@example.com");
        assert_eq!(person.phone, Some("+1234567890".to_string()));

        cleanup_test_journey(&pool, &journey_id).await;
    }
}
