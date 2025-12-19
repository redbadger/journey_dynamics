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
