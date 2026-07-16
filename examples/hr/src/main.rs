//! Runnable demo: hire a person, read their record, then exercise GDPR
//! right-to-erasure and watch a single key deletion shred their PII across
//! **both** the `Person` and `Employment` aggregates.
//!
//! Run with `cargo run` from `examples/hr/`.

use serde_json::json;
use uuid::Uuid;

use es_capture::aggregate::CaptureCommand;

use hr::{
    EMPLOYMENT_ROLE, EmploymentConfig, HrApp, PERSON_ROLE, PersonConfig, attrs, ptr, read_state,
};

#[tokio::main]
async fn main() {
    let app = HrApp::build();

    let subject = Uuid::new_v4();
    let person_id = Uuid::new_v4();
    let employment_id = Uuid::new_v4();
    let email = "ada@example.com".to_string();

    println!("HR example — es-capture, two aggregates, shared data subject\n");
    println!("subject id ...... {subject}");
    println!("person id ....... {person_id}");
    println!("employment id ... {employment_id}\n");

    // ── Hire ────────────────────────────────────────────────────────────────
    let pid = person_id.to_string();
    let eid = employment_id.to_string();
    hire_person(&app, &pid, person_id, subject, &email).await;
    hire_employment(&app, &eid, employment_id, person_id, subject, &email).await;

    // ── Read before erasure ───────────────────────────────────────────────────
    println!("── After hiring ──────────────────────────────────────────────");
    dump(
        "Person",
        &read_state::<PersonConfig>(&app.reader, person_id).await,
    );
    dump(
        "Employment",
        &read_state::<EmploymentConfig>(&app.reader, employment_id).await,
    );

    // ── Right-to-erasure: one key deletion covers both aggregates ─────────────
    println!("\n>> Right-to-erasure for subject {subject}");
    println!(">> Deleting the data encryption key once …");
    app.key_store
        .delete_key(&subject)
        .await
        .expect("delete key");
    // Record the audit event in each aggregate's history.
    exec(
        &app.person,
        &pid,
        CaptureCommand::ForgetSubject {
            subject_id: subject,
        },
    )
    .await;
    exec(
        &app.employment,
        &eid,
        CaptureCommand::ForgetSubject {
            subject_id: subject,
        },
    )
    .await;

    // ── Read after erasure ────────────────────────────────────────────────────
    println!("\n── After erasure (PII unreadable in BOTH; plaintext intact) ──");
    dump(
        "Person",
        &read_state::<PersonConfig>(&app.reader, person_id).await,
    );
    dump(
        "Employment",
        &read_state::<EmploymentConfig>(&app.reader, employment_id).await,
    );
}

async fn hire_person(app: &HrApp, pid: &str, person_id: Uuid, subject: Uuid, email: &str) {
    exec(&app.person, pid, CaptureCommand::Start { id: person_id }).await;
    exec(
        &app.person,
        pid,
        CaptureCommand::RegisterAndBindSubject {
            role_path: ptr(PERSON_ROLE),
            subject_id: subject,
            email: email.to_string(),
        },
    )
    .await;
    exec(
        &app.person,
        pid,
        CaptureCommand::SetAttributes {
            changes: attrs(vec![
                ("/self/firstName", json!("Ada")),
                ("/self/lastName", json!("Lovelace")),
                ("/self/dateOfBirth", json!("1815-12-10")),
                ("/self/nationalInsuranceNumber", json!("QQ123456C")),
                ("/self/country", json!("UK")),
            ]),
        },
    )
    .await;
}

async fn hire_employment(
    app: &HrApp,
    eid: &str,
    employment_id: Uuid,
    person_id: Uuid,
    subject: Uuid,
    email: &str,
) {
    exec(
        &app.employment,
        eid,
        CaptureCommand::Start { id: employment_id },
    )
    .await;
    exec(
        &app.employment,
        eid,
        CaptureCommand::RegisterAndBindSubject {
            role_path: ptr(EMPLOYMENT_ROLE),
            subject_id: subject,
            email: email.to_string(),
        },
    )
    .await;
    exec(
        &app.employment,
        eid,
        CaptureCommand::SetAttributes {
            changes: attrs(vec![
                ("/employment/personId", json!(person_id.to_string())),
                ("/employment/jobTitle", json!("Principal Engineer")),
                ("/employment/department", json!("R&D")),
                ("/employment/startDate", json!("2024-01-15")),
                ("/employee/salary", json!(145_000)),
                ("/employee/bankAccountNumber", json!("12345678")),
                ("/employee/bankSortCode", json!("01-02-03")),
            ]),
        },
    )
    .await;
}

async fn exec<C: es_capture::aggregate::CaptureConfig>(
    cqrs: &hr::HrCqrs<C>,
    aggregate_id: &str,
    command: CaptureCommand,
) {
    cqrs.execute(aggregate_id, command)
        .await
        .expect("command should succeed");
}

fn dump(label: &str, state: &serde_json::Value) {
    let pretty = serde_json::to_string_pretty(state).unwrap_or_else(|_| state.to_string());
    println!("\n{label}:\n{pretty}");
}
