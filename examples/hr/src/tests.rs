//! Behavioural tests for the two aggregates plus the cross-aggregate
//! crypto-shredding integration test.

use std::collections::BTreeMap;

use cqrs_es::test::TestFramework;
use serde_json::json;
use uuid::Uuid;

use es_capture::aggregate::{CaptureCommand, CaptureEvent, SecretPartitionData};

use super::*;

type PersonTester = TestFramework<Person>;
type EmploymentTester = TestFramework<Employment>;

// ── Person behaviour ─────────────────────────────────────────────────────────

#[test]
fn person_start_emits_started() {
    let id = Uuid::new_v4();
    PersonTester::with(person_services())
        .given_no_previous_events()
        .when(CaptureCommand::Start { id })
        .then_expect_events(vec![CaptureEvent::Started { id }]);
}

#[test]
fn person_register_and_bind_emits_both_events() {
    let id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let role_path = ptr(PERSON_ROLE);

    PersonTester::with(person_services())
        .given(vec![CaptureEvent::Started { id }])
        .when(CaptureCommand::RegisterAndBindSubject {
            role_path: role_path.clone(),
            subject_id,
            email: "ada@example.com".to_string(),
        })
        .then_expect_events(vec![
            CaptureEvent::SubjectRegistered {
                subject_id,
                email: "ada@example.com".to_string(),
            },
            CaptureEvent::SubjectBound {
                role_path,
                subject_id,
            },
        ]);
}

#[test]
fn person_secret_attribute_is_partitioned_under_the_subject() {
    // A secret path resolves to the bound subject and produces one partition.
    // With no decision engine, only `AttributesSet` is emitted.
    let id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let role_path = ptr(PERSON_ROLE);

    PersonTester::with(person_services())
        .given(vec![
            CaptureEvent::Started { id },
            CaptureEvent::SubjectRegistered {
                subject_id,
                email: "ada@example.com".to_string(),
            },
            CaptureEvent::SubjectBound {
                role_path: role_path.clone(),
                subject_id,
            },
        ])
        .when(CaptureCommand::SetAttributes {
            changes: attrs(vec![("/self/firstName", json!("Ada"))]),
        })
        .then_expect_events(vec![CaptureEvent::AttributesSet {
            plaintext: BTreeMap::new(),
            secret_partitions: vec![SecretPartitionData {
                role_path,
                subject_id,
                changes: attrs(vec![("/self/firstName", json!("Ada"))]),
            }],
        }]);
}

#[test]
fn person_plaintext_attribute_stays_plaintext() {
    let id = Uuid::new_v4();
    PersonTester::with(person_services())
        .given(vec![CaptureEvent::Started { id }])
        .when(CaptureCommand::SetAttributes {
            changes: attrs(vec![("/self/country", json!("UK"))]),
        })
        .then_expect_events(vec![CaptureEvent::AttributesSet {
            plaintext: attrs(vec![("/self/country", json!("UK"))]),
            secret_partitions: vec![],
        }]);
}

#[test]
fn person_rejects_unknown_path() {
    // Explicit schema: anything not declared is rejected.
    let id = Uuid::new_v4();
    let unknown = ptr("/self/favouriteColour");
    PersonTester::with(person_services())
        .given(vec![CaptureEvent::Started { id }])
        .when(CaptureCommand::SetAttributes {
            changes: attrs(vec![("/self/favouriteColour", json!("blue"))]),
        })
        .then_expect_error(es_capture::aggregate::CaptureError::UnknownAttributePath(
            vec![unknown],
        ));
}

// ── Employment behaviour ─────────────────────────────────────────────────────

#[test]
fn employment_splits_plaintext_and_secret() {
    let id = Uuid::new_v4();
    let subject_id = Uuid::new_v4();
    let role_path = ptr(EMPLOYMENT_ROLE);

    EmploymentTester::with(employment_services())
        .given(vec![
            CaptureEvent::Started { id },
            CaptureEvent::SubjectRegistered {
                subject_id,
                email: "ada@example.com".to_string(),
            },
            CaptureEvent::SubjectBound {
                role_path: role_path.clone(),
                subject_id,
            },
        ])
        .when(CaptureCommand::SetAttributes {
            changes: attrs(vec![
                ("/employment/jobTitle", json!("Principal Engineer")),
                ("/employee/salary", json!(145_000)),
            ]),
        })
        .then_expect_events(vec![CaptureEvent::AttributesSet {
            plaintext: attrs(vec![("/employment/jobTitle", json!("Principal Engineer"))]),
            secret_partitions: vec![SecretPartitionData {
                role_path,
                subject_id,
                changes: attrs(vec![("/employee/salary", json!(145_000))]),
            }],
        }]);
}

// ── Cross-aggregate crypto-shredding ─────────────────────────────────────────

#[tokio::test]
async fn forgetting_the_subject_shreds_pii_in_both_aggregates() {
    let app = HrApp::build();
    let subject = Uuid::new_v4();
    let person_id = Uuid::new_v4();
    let employment_id = Uuid::new_v4();
    let email = "ada@example.com".to_string();

    // ── Hire: Person ──
    let pid = person_id.to_string();
    app.person
        .execute(&pid, CaptureCommand::Start { id: person_id })
        .await
        .unwrap();
    app.person
        .execute(
            &pid,
            CaptureCommand::RegisterAndBindSubject {
                role_path: ptr(PERSON_ROLE),
                subject_id: subject,
                email: email.clone(),
            },
        )
        .await
        .unwrap();
    app.person
        .execute(
            &pid,
            CaptureCommand::SetAttributes {
                changes: attrs(vec![
                    ("/self/firstName", json!("Ada")),
                    ("/self/country", json!("UK")),
                ]),
            },
        )
        .await
        .unwrap();

    // ── Hire: Employment (same subject) ──
    let eid = employment_id.to_string();
    app.employment
        .execute(&eid, CaptureCommand::Start { id: employment_id })
        .await
        .unwrap();
    app.employment
        .execute(
            &eid,
            CaptureCommand::RegisterAndBindSubject {
                role_path: ptr(EMPLOYMENT_ROLE),
                subject_id: subject,
                email: email.clone(),
            },
        )
        .await
        .unwrap();
    app.employment
        .execute(
            &eid,
            CaptureCommand::SetAttributes {
                changes: attrs(vec![
                    ("/employment/jobTitle", json!("Principal Engineer")),
                    ("/employee/salary", json!(145_000)),
                ]),
            },
        )
        .await
        .unwrap();

    // ── Before erasure: PII is readable in both ──
    let person_before = read_state::<PersonConfig>(&app.reader, person_id).await;
    assert_eq!(person_before["self"]["firstName"], json!("Ada"));
    assert_eq!(person_before["self"]["country"], json!("UK"));

    let employment_before = read_state::<EmploymentConfig>(&app.reader, employment_id).await;
    assert_eq!(employment_before["employee"]["salary"], json!(145_000));
    assert_eq!(
        employment_before["employment"]["jobTitle"],
        json!("Principal Engineer")
    );

    // ── Erase once ──
    app.key_store.delete_key(&subject).await.unwrap();

    // ── After erasure: secrets gone in BOTH aggregates, plaintext intact ──
    let person_after = read_state::<PersonConfig>(&app.reader, person_id).await;
    assert!(
        person_after["self"].get("firstName").is_none(),
        "person first name must be unrecoverable after shredding"
    );
    assert_eq!(
        person_after["self"]["country"],
        json!("UK"),
        "plaintext survives shredding"
    );
    assert_eq!(person_after["redacted"], json!(true));

    let employment_after = read_state::<EmploymentConfig>(&app.reader, employment_id).await;
    assert!(
        employment_after["employee"].get("salary").is_none(),
        "salary must be unrecoverable after shredding the same subject"
    );
    assert_eq!(
        employment_after["employment"]["jobTitle"],
        json!("Principal Engineer"),
        "plaintext survives shredding"
    );
    assert_eq!(employment_after["redacted"], json!(true));
}
