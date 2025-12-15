use async_trait::async_trait;
use cqrs_es::{EventEnvelope, Query};
use sqlx::{Pool, Postgres, Row};
use uuid::Uuid;

use crate::domain::events::JourneyEvent;
use crate::domain::journey::Journey;
use crate::queries::{DataCaptureEntry, JourneyState, JourneyView, WorkflowDecisionView};

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

        // Load data capture entries
        let data_capture_rows = sqlx::query(
            r"
            SELECT key, value, sequence
            FROM journey_data_capture
            WHERE journey_id = $1
            ORDER BY sequence ASC
            ",
        )
        .bind(journey_id)
        .fetch_all(&self.pool)
        .await?;

        let data_capture: Vec<DataCaptureEntry> = data_capture_rows
            .iter()
            .map(|row| DataCaptureEntry {
                key: row.get("key"),
                value: row.get("value"),
            })
            .collect();

        // Load latest workflow decision
        let workflow_decision_row = sqlx::query(
            r"
            SELECT available_actions, primary_next_step
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
            let available_actions: Vec<String> = row.get("available_actions");
            let primary_next_step: Option<String> = row.get("primary_next_step");
            WorkflowDecisionView {
                available_actions,
                primary_next_step,
            }
        });

        Ok(Some(JourneyView {
            id,
            state,
            data_capture,
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

            JourneyEvent::Modified { form_data } => {
                if let Some((key, value)) = form_data {
                    // Get the next sequence number for this journey
                    let sequence: i32 = sqlx::query_scalar(
                        r"
                        SELECT COALESCE(MAX(sequence), 0) + 1
                        FROM journey_data_capture
                        WHERE journey_id = $1
                        ",
                    )
                    .bind(journey_id)
                    .fetch_one(&self.pool)
                    .await?;

                    // Insert data capture entry
                    sqlx::query(
                        r"
                        INSERT INTO journey_data_capture (journey_id, key, value, sequence)
                        VALUES ($1, $2, $3, $4)
                        ",
                    )
                    .bind(journey_id)
                    .bind(key)
                    .bind(value)
                    .bind(sequence)
                    .execute(&self.pool)
                    .await?;
                }

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

            JourneyEvent::WorkflowEvaluated {
                available_actions,
                primary_next_step,
            } => {
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
                    INSERT INTO journey_workflow_decision (journey_id, available_actions, primary_next_step, is_latest)
                    VALUES ($1, $2, $3, TRUE)
                    ",
                )
                .bind(journey_id)
                .bind(available_actions)
                .bind(primary_next_step)
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
            "postgres://demo_user:demo_pass@localhost:5432/postgres".to_string()
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
                    form_data: Some(("email".to_string(), json!("test@example.com"))),
                },
                metadata: std::collections::HashMap::default(),
            },
            EventEnvelope {
                aggregate_id: journey_id.to_string(),
                sequence: 3,
                payload: JourneyEvent::WorkflowEvaluated {
                    available_actions: vec!["continue".to_string()],
                    primary_next_step: Some("confirmation".to_string()),
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
        assert_eq!(view.data_capture.len(), 1);
        assert_eq!(view.data_capture[0].key, "email");
        assert_eq!(view.current_step, Some("confirmation".to_string()));
        assert!(view.latest_workflow_decision.is_some());

        cleanup_test_journey(&pool, &journey_id).await;
    }
}
