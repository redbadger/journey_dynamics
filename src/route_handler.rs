use crate::command_extractor::CommandExtractor;
use crate::state::ApplicationState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

// Serves as our query endpoint to respond with the materialized `JourneyView`
// for the requested journey.
pub async fn query_handler(
    Path(journey_id): Path<String>,
    State(state): State<ApplicationState>,
) -> Response {
    // Parse the journey_id as UUID
    let Ok(uuid) = Uuid::parse_str(&journey_id) else {
        return (StatusCode::BAD_REQUEST, "Invalid journey ID format").into_response();
    };

    match state.journey_query.load(&uuid).await {
        Ok(Some(journey_view)) => (StatusCode::OK, Json(journey_view)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            println!("Error: {err:#?}\n");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

// Serves as our command endpoint to make changes in a `Journey` aggregate.
pub async fn command_handler(
    Path(journey_id): Path<String>,
    State(state): State<ApplicationState>,
    CommandExtractor(metadata, command): CommandExtractor,
) -> Response {
    match state
        .cqrs
        .execute_with_metadata(&journey_id, command, metadata)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            println!("Error: {err:#?}\n");
            (StatusCode::BAD_REQUEST, err.to_string()).into_response()
        }
    }
}
