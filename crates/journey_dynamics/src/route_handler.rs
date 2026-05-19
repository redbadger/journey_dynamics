use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    command_extractor::CommandExtractor, domain::commands::JourneyCommand, state::ApplicationState,
};

/// Request body for `DELETE /subjects/by-email`.
#[derive(Debug, Deserialize)]
pub struct EraseByEmailBody {
    pub email: String,
}

// Handles GDPR right-to-erasure requests by crypto-shredding the subject's DEK,
// which permanently renders all encrypted PII irrecoverable, then emits a
// `SubjectForgotten` audit event on every affected journey.
pub async fn shred_subject(
    Path(subject_id): Path<Uuid>,
    State(state): State<Arc<ApplicationState>>,
) -> Response {
    if let Err(response) = shred_one_subject(&state, subject_id).await {
        return response;
    }
    StatusCode::NO_CONTENT.into_response()
}

// Handles GDPR right-to-erasure requests by email address. Resolves every
// non-forgotten subject_id stored against the supplied email (case-insensitively),
// then runs the same crypto-shredding flow as `shred_subject` for each one.
// This is robust to the caller's subject-ID derivation scheme and works even
// after an email address has changed, provided the original address was stored
// in `journey_person` at booking time.
pub async fn shred_subjects_by_email(
    State(state): State<Arc<ApplicationState>>,
    Json(body): Json<EraseByEmailBody>,
) -> Response {
    // 1. Resolve all non-forgotten subject_ids linked to this email.
    //    The query uses a pre-lowercased column (email_lower) compared against lower($1).
    let subject_ids = match state
        .journey_query
        .find_subjects_by_email(&body.email)
        .await
    {
        Ok(ids) => ids,
        Err(err) => {
            eprintln!(
                "Error looking up subjects for email {}: {err:#?}",
                body.email
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };

    // 2. For each resolved subject_id, run the same shredding flow.
    for subject_id in subject_ids {
        if let Err(response) = shred_one_subject(&state, subject_id).await {
            return response;
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

/// Core shredding logic for a single subject.
///
/// 1. Finds all journeys that reference the subject in the event store.
/// 2. Atomically deletes the DEK and the `subject_lookup` row in one transaction.
///    After this commit, GDPR erasure is complete: all ciphertext is permanently
///    unreadable and the email address is removed from the lookup table.
/// 3. Emits a `SubjectForgotten` audit event on each affected journey (best-effort).
///    A failure here is logged but does not abort — the PII is already gone.
///
/// Returns `Err(Response)` on any hard failure so the caller can short-circuit.
async fn shred_one_subject(state: &ApplicationState, subject_id: Uuid) -> Result<(), Response> {
    // Step 1 — find affected journeys.
    let journeys = state
        .journey_query
        .find_journeys_by_subject(&subject_id)
        .await
        .map_err(|err| {
            eprintln!("Error fetching journeys for subject {subject_id}: {err:#?}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;

    // Step 2 — atomically delete the DEK and the email → subject_id lookup entry.
    //
    // Both rows are PII: the DEK renders all ciphertext permanently unreadable;
    // the subject_lookup row holds the plaintext email address. Committing them
    // together ensures neither survives a mid-shred crash.
    let mut tx = state.pool.begin().await.map_err(|err| {
        eprintln!("Error starting shred transaction for subject {subject_id}: {err:#?}");
        (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;

    state
        .key_store
        .delete_key_in_tx(&mut tx, &subject_id)
        .await
        .map_err(|err| {
            eprintln!("Error deleting key for subject {subject_id}: {err:#?}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;

    state
        .journey_query
        .delete_subject_lookup_in_tx(&mut tx, &subject_id)
        .await
        .map_err(|err| {
            eprintln!("Error removing subject_lookup for {subject_id}: {err:#?}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        })?;

    tx.commit().await.map_err(|err| {
        eprintln!("Error committing shred transaction for subject {subject_id}: {err:#?}");
        (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
    })?;

    // Step 3 — emit SubjectForgotten audit events (best-effort).
    for aggregate_id in &journeys {
        if let Err(err) = state
            .cqrs
            .execute(aggregate_id, JourneyCommand::ForgetSubject { subject_id })
            .await
        {
            // PII is already gone; log and continue so we still attempt all journeys.
            eprintln!("Error emitting SubjectForgotten for journey {aggregate_id}: {err:#?}");
        }
    }

    Ok(())
}

// Serves as our query endpoint to respond with the materialized `JourneyView`
// for the requested journey.
pub async fn query_handler(
    Path(journey_id): Path<Uuid>,
    State(state): State<Arc<ApplicationState>>,
) -> Response {
    match state.journey_query.load(&journey_id).await {
        Ok(Some(journey_view)) => (StatusCode::OK, Json(journey_view)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            eprintln!("Error: {err:#?}");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

// Serves as our command endpoint to make changes in a `Journey` aggregate.
// Handles both journey creation (no journey_id in path) and modification (with journey_id).
pub async fn command_handler(
    path: Option<Path<Uuid>>,
    State(state): State<Arc<ApplicationState>>,
    CommandExtractor(metadata, command): CommandExtractor,
) -> Response {
    // Determine the journey_id and creation status based on path and command
    let (journey_id, is_creating) = match path {
        Some(Path(id)) => {
            // Path parameter provided - check if it's a Start command
            let is_creating = matches!(command, JourneyCommand::Start { .. });
            (id, is_creating)
        }
        None => {
            // No path parameter - this must be journey creation
            match &command {
                JourneyCommand::Start { id } => (*id, true),
                _ => {
                    // No path parameter and not a Start command - invalid
                    return (
                        StatusCode::BAD_REQUEST,
                        "Journey creation requires a Start command",
                    )
                        .into_response();
                }
            }
        }
    };

    match state
        .cqrs
        .execute_with_metadata(&journey_id.to_string(), command, metadata)
        .await
    {
        Ok(()) => {
            if is_creating {
                let mut headers = HeaderMap::new();

                let location = format!("/journeys/{journey_id}");
                let Ok(header_value) = HeaderValue::from_str(&location) else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to create location header",
                    )
                        .into_response();
                };
                headers.insert(header::LOCATION, header_value);

                (StatusCode::CREATED, headers).into_response()
            } else {
                StatusCode::NO_CONTENT.into_response()
            }
        }
        Err(err) => {
            eprintln!("Error: {err:#?}");
            (StatusCode::BAD_REQUEST, err.to_string()).into_response()
        }
    }
}
