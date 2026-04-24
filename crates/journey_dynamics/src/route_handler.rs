use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};

use uuid::Uuid;

use crate::{
    command_extractor::CommandExtractor, domain::commands::JourneyCommand, state::ApplicationState,
};

// Handles GDPR right-to-erasure requests by crypto-shredding the subject's DEK,
// which permanently renders all encrypted PII irrecoverable, then emits a
// `SubjectForgotten` audit event on every affected journey.
pub async fn shred_subject(
    Path(subject_id): Path<Uuid>,
    State(state): State<Arc<ApplicationState>>,
) -> Response {
    // 1. Find all journeys that reference this subject by scanning the event store.
    //    PersonCaptured and PersonDetailsUpdated events both carry subject_id in plaintext,
    //    so no separate mapping table is needed.
    let journeys = match state
        .journey_query
        .find_journeys_by_subject(&subject_id)
        .await
    {
        Ok(j) => j,
        Err(err) => {
            eprintln!("Error fetching journeys for subject {subject_id}: {err:#?}");
            return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
        }
    };

    // 2. Crypto-shred: delete the DEK — all ciphertext is now permanently unreadable.
    if let Err(err) = state.key_store.delete_key(&subject_id).await {
        eprintln!("Error deleting key for subject {subject_id}: {err:#?}");
        return (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response();
    }

    // 3. For each affected journey, emit a SubjectForgotten audit event via the
    //    ForgetSubject command. The view-repository handler for SubjectForgotten nulls
    //    out the person slot and sets forgotten = TRUE.
    for aggregate_id in &journeys {
        if let Err(err) = state
            .cqrs
            .execute(aggregate_id, JourneyCommand::ForgetSubject { subject_id })
            .await
        {
            // The key is already gone so the shredding is complete. Log and continue
            // so that we still attempt to clean up the remaining journeys.
            eprintln!("Error emitting SubjectForgotten for journey {aggregate_id}: {err:#?}");
        }
    }

    StatusCode::NO_CONTENT.into_response()
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
            println!("Error: {err:#?}\n");
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
            println!("Error: {err:#?}\n");
            (StatusCode::BAD_REQUEST, err.to_string()).into_response()
        }
    }
}
