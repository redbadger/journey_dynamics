use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use thiserror::Error;
use uuid::Uuid;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum MappingError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

// ── Trait ────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait SubjectMapping: Send + Sync {
    /// Record that a journey (aggregate) belongs to a subject.
    /// If the journey is already mapped (e.g. called twice), silently succeeds (idempotent).
    async fn associate(&self, aggregate_id: &str, subject_id: &Uuid) -> Result<(), MappingError>;

    /// Look up the subject for a journey.
    /// Returns `None` if `CapturePerson` has not yet been called for this journey.
    async fn get_subject(&self, aggregate_id: &str) -> Result<Option<Uuid>, MappingError>;

    /// Find all journey `aggregate_id`s belonging to a subject.
    /// Used during shredding to clean up read-model projections.
    async fn get_journeys(&self, subject_id: &Uuid) -> Result<Vec<String>, MappingError>;
}

// ── InMemorySubjectMapping ───────────────────────────────────────────────────

/// In-memory `SubjectMapping` backed by a `HashMap`, for use in tests.
pub struct InMemorySubjectMapping {
    // Store both directions for O(1) lookup
    journey_to_subject: Mutex<HashMap<String, Uuid>>,
    subject_to_journeys: Mutex<HashMap<Uuid, Vec<String>>>,
}

impl InMemorySubjectMapping {
    #[must_use]
    pub fn new() -> Self {
        Self {
            journey_to_subject: Mutex::new(HashMap::new()),
            subject_to_journeys: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemorySubjectMapping {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SubjectMapping for InMemorySubjectMapping {
    async fn associate(&self, aggregate_id: &str, subject_id: &Uuid) -> Result<(), MappingError> {
        let mut j2s = self.journey_to_subject.lock().unwrap();
        let mut s2j = self.subject_to_journeys.lock().unwrap();

        // Idempotent: if already mapped, do nothing.
        if j2s.contains_key(aggregate_id) {
            return Ok(());
        }

        j2s.insert(aggregate_id.to_string(), *subject_id);

        let journeys = s2j.entry(*subject_id).or_default();
        if !journeys.contains(&aggregate_id.to_string()) {
            journeys.push(aggregate_id.to_string());
        }

        Ok(())
    }

    async fn get_subject(&self, aggregate_id: &str) -> Result<Option<Uuid>, MappingError> {
        let j2s = self.journey_to_subject.lock().unwrap();
        Ok(j2s.get(aggregate_id).copied())
    }

    async fn get_journeys(&self, subject_id: &Uuid) -> Result<Vec<String>, MappingError> {
        let s2j = self.subject_to_journeys.lock().unwrap();
        Ok(s2j.get(subject_id).cloned().unwrap_or_default())
    }
}

// ── PostgresSubjectMapping ───────────────────────────────────────────────────

pub struct PostgresSubjectMapping {
    pool: sqlx::Pool<sqlx::Postgres>,
}

impl PostgresSubjectMapping {
    #[must_use]
    pub fn new(pool: sqlx::Pool<sqlx::Postgres>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SubjectMapping for PostgresSubjectMapping {
    async fn associate(&self, aggregate_id: &str, subject_id: &Uuid) -> Result<(), MappingError> {
        sqlx::query(
            "INSERT INTO journey_subject_mapping (aggregate_id, subject_id) \
             VALUES ($1, $2) ON CONFLICT (aggregate_id) DO NOTHING",
        )
        .bind(aggregate_id)
        .bind(subject_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_subject(&self, aggregate_id: &str) -> Result<Option<Uuid>, MappingError> {
        use sqlx::Row;

        let row =
            sqlx::query("SELECT subject_id FROM journey_subject_mapping WHERE aggregate_id = $1")
                .bind(aggregate_id)
                .fetch_optional(&self.pool)
                .await?;

        Ok(row.map(|r| r.get::<Uuid, _>("subject_id")))
    }

    async fn get_journeys(&self, subject_id: &Uuid) -> Result<Vec<String>, MappingError> {
        use sqlx::Row;

        let rows =
            sqlx::query("SELECT aggregate_id FROM journey_subject_mapping WHERE subject_id = $1")
                .bind(subject_id)
                .fetch_all(&self.pool)
                .await?;

        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("aggregate_id"))
            .collect())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Unit tests (InMemorySubjectMapping) ──────────────────────────────────

    #[tokio::test]
    async fn test_get_subject_returns_none_for_unmapped_journey() {
        let mapping = InMemorySubjectMapping::new();
        let result = mapping.get_subject("unknown-aggregate").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_associate_and_get_subject() {
        let mapping = InMemorySubjectMapping::new();
        let aggregate_id = "journey-001";
        let subject_id = Uuid::new_v4();

        mapping.associate(aggregate_id, &subject_id).await.unwrap();

        let result = mapping.get_subject(aggregate_id).await.unwrap();
        assert_eq!(result, Some(subject_id));
    }

    #[tokio::test]
    async fn test_associate_is_idempotent() {
        let mapping = InMemorySubjectMapping::new();
        let aggregate_id = "journey-idempotent";
        let subject_id = Uuid::new_v4();

        mapping.associate(aggregate_id, &subject_id).await.unwrap();
        mapping.associate(aggregate_id, &subject_id).await.unwrap();

        // get_subject still returns the correct UUID
        let result = mapping.get_subject(aggregate_id).await.unwrap();
        assert_eq!(result, Some(subject_id));

        // get_journeys does not contain a duplicate
        let journeys = mapping.get_journeys(&subject_id).await.unwrap();
        assert_eq!(journeys.len(), 1);
        assert_eq!(journeys[0], aggregate_id);
    }

    #[tokio::test]
    async fn test_get_journeys_returns_empty_for_unknown_subject() {
        let mapping = InMemorySubjectMapping::new();
        let subject_id = Uuid::new_v4();

        let journeys = mapping.get_journeys(&subject_id).await.unwrap();
        assert!(journeys.is_empty());
    }

    #[tokio::test]
    async fn test_get_journeys_returns_single_journey() {
        let mapping = InMemorySubjectMapping::new();
        let aggregate_id = "journey-single";
        let subject_id = Uuid::new_v4();

        mapping.associate(aggregate_id, &subject_id).await.unwrap();

        let journeys = mapping.get_journeys(&subject_id).await.unwrap();
        assert_eq!(journeys, vec![aggregate_id.to_string()]);
    }

    #[tokio::test]
    async fn test_get_journeys_returns_multiple_journeys() {
        let mapping = InMemorySubjectMapping::new();
        let subject_id = Uuid::new_v4();
        let first_journey = "journey-multi-1";
        let second_journey = "journey-multi-2";

        mapping.associate(first_journey, &subject_id).await.unwrap();
        mapping
            .associate(second_journey, &subject_id)
            .await
            .unwrap();

        let mut journeys = mapping.get_journeys(&subject_id).await.unwrap();
        journeys.sort(); // order doesn't matter

        assert_eq!(journeys.len(), 2);
        assert!(journeys.contains(&first_journey.to_string()));
        assert!(journeys.contains(&second_journey.to_string()));
    }

    #[tokio::test]
    async fn test_different_subjects_are_independent() {
        let mapping = InMemorySubjectMapping::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let alice_journey = "journey-subject-alice";
        let bob_journey = "journey-subject-bob";

        mapping.associate(alice_journey, &alice).await.unwrap();
        mapping.associate(bob_journey, &bob).await.unwrap();

        let alice_journeys = mapping.get_journeys(&alice).await.unwrap();
        assert_eq!(alice_journeys, vec![alice_journey.to_string()]);

        let bob_journeys = mapping.get_journeys(&bob).await.unwrap();
        assert_eq!(bob_journeys, vec![bob_journey.to_string()]);
    }

    // ── Integration tests (PostgresSubjectMapping) ───────────────────────────

    async fn setup_test_db() -> sqlx::Pool<sqlx::Postgres> {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:postgres@localhost:5432/journey_dynamics".to_string()
        });
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .expect("Failed to connect to database")
    }

    #[tokio::test]
    async fn test_postgres_associate_and_get_subject() {
        let pool = setup_test_db().await;
        let mapping = PostgresSubjectMapping::new(pool.clone());

        let aggregate_id = format!("test-journey-{}", Uuid::new_v4());
        let subject_id = Uuid::new_v4();

        mapping.associate(&aggregate_id, &subject_id).await.unwrap();

        let result = mapping.get_subject(&aggregate_id).await.unwrap();
        assert_eq!(result, Some(subject_id));

        // Cleanup
        sqlx::query("DELETE FROM journey_subject_mapping WHERE aggregate_id = $1")
            .bind(&aggregate_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_postgres_get_journeys_for_subject() {
        let pool = setup_test_db().await;
        let mapping = PostgresSubjectMapping::new(pool.clone());

        let subject_id = Uuid::new_v4();
        let first_aggregate = format!("test-journey-{}", Uuid::new_v4());
        let second_aggregate = format!("test-journey-{}", Uuid::new_v4());

        mapping
            .associate(&first_aggregate, &subject_id)
            .await
            .unwrap();
        mapping
            .associate(&second_aggregate, &subject_id)
            .await
            .unwrap();

        let mut journeys = mapping.get_journeys(&subject_id).await.unwrap();
        journeys.sort();

        assert_eq!(journeys.len(), 2);
        assert!(journeys.contains(&first_aggregate));
        assert!(journeys.contains(&second_aggregate));

        // Cleanup
        sqlx::query("DELETE FROM journey_subject_mapping WHERE subject_id = $1")
            .bind(subject_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_postgres_associate_is_idempotent() {
        let pool = setup_test_db().await;
        let mapping = PostgresSubjectMapping::new(pool.clone());

        let aggregate_id = format!("test-journey-{}", Uuid::new_v4());
        let subject_id = Uuid::new_v4();

        mapping.associate(&aggregate_id, &subject_id).await.unwrap();
        mapping.associate(&aggregate_id, &subject_id).await.unwrap();

        let journeys = mapping.get_journeys(&subject_id).await.unwrap();
        assert_eq!(journeys.len(), 1);
        assert_eq!(journeys[0], aggregate_id);

        // Cleanup
        sqlx::query("DELETE FROM journey_subject_mapping WHERE aggregate_id = $1")
            .bind(&aggregate_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
