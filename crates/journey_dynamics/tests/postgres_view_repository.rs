#![allow(clippy::doc_markdown)]
//! Integration tests for [`StructuredJourneyViewRepository`].
//!
//! These tests require a live PostgreSQL database.  The connection URL is read
//! from the `DATABASE_URL` environment variable, falling back to
//! `postgres://postgres:postgres@localhost:5432/journey_dynamics`.
//!
//! Run with:
//!   `cargo nextest run --test postgres_view_repository`
//!
//! They are deliberately kept out of `--lib` runs so that
//! `cargo nextest run --lib` succeeds without a database being present.

use cqrs_es::{EventEnvelope, Query};
use journey_dynamics::{
    domain::events::JourneyEvent, queries::JourneyState,
    view_repository::StructuredJourneyViewRepository,
};
use serde_json::json;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};
use uuid::Uuid;

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

async fn cleanup_subject_lookup(pool: &Pool<Postgres>, subject_id: &Uuid) {
    let _ = sqlx::query("DELETE FROM subject_lookup WHERE subject_id = $1")
        .bind(subject_id)
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

// ── find_subjects_by_email ───────────────────────────────────────────────

#[tokio::test]
async fn test_find_subjects_by_email_case_insensitive() {
    // Email stored in mixed case; query with lowercase — must still match.
    // find_subjects_by_email reads subject_lookup only, so no journey setup needed.
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_id = Uuid::new_v4();
    let stored_email = format!("Alice+{}@Example.COM", Uuid::new_v4());

    sqlx::query("INSERT INTO subject_lookup (subject_id, email_lower) VALUES ($1, lower($2))")
        .bind(subject_id)
        .bind(&stored_email)
        .execute(&pool)
        .await
        .unwrap();

    let subjects = repo
        .find_subjects_by_email(&stored_email.to_lowercase())
        .await
        .unwrap();
    assert_eq!(subjects.len(), 1);
    assert_eq!(subjects[0], subject_id);

    cleanup_subject_lookup(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_find_subjects_by_email_deduplicates_across_journeys() {
    // subject_lookup has subject_id as PK — one row regardless of how many
    // journeys a subject appears in.  The query must return exactly one result.
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_id = Uuid::new_v4();
    let unique_email = format!("repeat+{}@example.com", Uuid::new_v4());

    sqlx::query("INSERT INTO subject_lookup (subject_id, email_lower) VALUES ($1, lower($2))")
        .bind(subject_id)
        .bind(&unique_email)
        .execute(&pool)
        .await
        .unwrap();

    let subjects = repo.find_subjects_by_email(&unique_email).await.unwrap();
    assert_eq!(
        subjects.len(),
        1,
        "same subject across two journeys must be deduplicated"
    );
    assert_eq!(subjects[0], subject_id);

    cleanup_subject_lookup(&pool, &subject_id).await;
}

#[tokio::test]
async fn test_find_subjects_by_email_excludes_shredded() {
    // After shredding, the subject_lookup row is deleted by the route handler.
    // Verify the email lookup returns nothing once that deletion has occurred.
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_id = Uuid::new_v4();
    let unique_email = format!("gone+{}@example.com", Uuid::new_v4());

    sqlx::query("INSERT INTO subject_lookup (subject_id, email_lower) VALUES ($1, lower($2))")
        .bind(subject_id)
        .bind(&unique_email)
        .execute(&pool)
        .await
        .unwrap();

    // Simulate shredding via the actual method the route handler calls.
    repo.delete_subject_lookup(&subject_id).await.unwrap();

    let subjects = repo.find_subjects_by_email(&unique_email).await.unwrap();
    assert!(
        subjects.is_empty(),
        "shredded subject must not be returned by email lookup"
    );
}

#[tokio::test]
async fn test_find_subjects_by_email_unknown_returns_empty() {
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let unknown = format!("nobody+{}@example.com", Uuid::new_v4());

    let subjects = repo.find_subjects_by_email(&unknown).await.unwrap();
    assert!(subjects.is_empty(), "unknown email must return empty vec");
}

#[tokio::test]
async fn test_find_subjects_by_email_returns_multiple_subjects() {
    // Two distinct subjects sharing the same email address (e.g. random
    // UUID-per-slot strategy) must both be returned.
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();
    let shared_email = format!("shared+{}@example.com", Uuid::new_v4());

    for subject_id in [subject_a, subject_b] {
        sqlx::query("INSERT INTO subject_lookup (subject_id, email_lower) VALUES ($1, lower($2))")
            .bind(subject_id)
            .bind(&shared_email)
            .execute(&pool)
            .await
            .unwrap();
    }

    let mut subjects = repo.find_subjects_by_email(&shared_email).await.unwrap();
    subjects.sort();
    let mut expected = vec![subject_a, subject_b];
    expected.sort();
    assert_eq!(subjects, expected, "both subjects must be returned");

    cleanup_subject_lookup(&pool, &subject_a).await;
    cleanup_subject_lookup(&pool, &subject_b).await;
}

// ── delete_subject_lookup ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_subject_lookup_removes_row() {
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_id = Uuid::new_v4();

    sqlx::query("INSERT INTO subject_lookup (subject_id, email_lower) VALUES ($1, lower($2))")
        .bind(subject_id)
        .bind("test@example.com")
        .execute(&pool)
        .await
        .unwrap();

    repo.delete_subject_lookup(&subject_id).await.unwrap();

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subject_lookup WHERE subject_id = $1")
            .bind(subject_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0, "row must be gone after delete_subject_lookup");
}

#[tokio::test]
async fn test_delete_subject_lookup_is_idempotent() {
    // Calling delete on a subject_id that has no row must not error.
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_id = Uuid::new_v4();

    repo.delete_subject_lookup(&subject_id).await.unwrap();
    repo.delete_subject_lookup(&subject_id).await.unwrap();
}

#[tokio::test]
async fn test_delete_subject_lookup_does_not_affect_other_subjects() {
    let pool = setup_test_db().await;
    let repo = StructuredJourneyViewRepository::new(pool.clone());
    let subject_a = Uuid::new_v4();
    let subject_b = Uuid::new_v4();

    for subject_id in [subject_a, subject_b] {
        sqlx::query("INSERT INTO subject_lookup (subject_id, email_lower) VALUES ($1, lower($2))")
            .bind(subject_id)
            .bind(format!("test+{subject_id}@example.com"))
            .execute(&pool)
            .await
            .unwrap();
    }

    repo.delete_subject_lookup(&subject_a).await.unwrap();

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subject_lookup WHERE subject_id = $1")
            .bind(subject_b)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "subject_b must be unaffected by subject_a deletion"
    );

    cleanup_subject_lookup(&pool, &subject_b).await;
}
