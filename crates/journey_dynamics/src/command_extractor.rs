use axum::{
    body::{Body, Bytes},
    extract::FromRequest,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use std::collections::HashMap;

use crate::domain::commands::JourneyCommand;

// This is a custom Axum extension that builds metadata from the inbound request
// and parses and deserializes the body as the command payload.
pub struct CommandExtractor(pub HashMap<String, String>, pub JourneyCommand);

const USER_AGENT_HDR: &str = "User-Agent";

impl<S> FromRequest<S> for CommandExtractor
where
    S: Send + Sync,
{
    type Rejection = CommandExtractionError;

    async fn from_request(req: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        let uri_path = req.uri().path().to_string();

        // Here we are including the current date/time, the uri that was called and the user-agent
        // in a HashMap that we will submit as metadata with the command.
        let mut metadata = HashMap::default();
        metadata.insert("time".to_string(), chrono::Utc::now().to_rfc3339());
        metadata.insert("uri".to_string(), req.uri().to_string());
        if let Some(user_agent) = req.headers().get(USER_AGENT_HDR)
            && let Ok(value) = user_agent.to_str()
        {
            metadata.insert(USER_AGENT_HDR.to_string(), value.to_string());
        }

        // Parse and deserialize the request body as the command payload.
        let body = Bytes::from_request(req, state).await?;
        let command: JourneyCommand = if body.is_empty() {
            // Only generate a Start command for journey creation (POST /journeys)
            // If posting to a specific journey (POST /journeys/{id}), empty body is invalid

            if uri_path == "/journeys" {
                let id = uuid::Uuid::new_v4();
                JourneyCommand::Start { id }
            } else {
                return Err(CommandExtractionError);
            }
        } else {
            serde_json::from_slice(&body)?
        };

        Ok(Self(metadata, command))
    }
}

pub struct CommandExtractionError;

impl IntoResponse for CommandExtractionError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            "command could not be read".to_string(),
        )
            .into_response()
    }
}

impl From<axum::extract::rejection::BytesRejection> for CommandExtractionError {
    fn from(_: axum::extract::rejection::BytesRejection) -> Self {
        Self
    }
}

impl From<serde_json::Error> for CommandExtractionError {
    fn from(_: serde_json::Error) -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::domain::{AttributePath, commands::JourneyCommand};

    /// The HTTP route deserialises the request body as [`JourneyCommand`] via
    /// serde. Verify that the `SetAttributes` variant round-trips correctly so
    /// that a POST body of the form
    ///
    /// ```json
    /// { "SetAttributes": { "changes": { "search/origin": "LHR" } } }
    /// ```
    ///
    /// is accepted at the existing `/journeys/{id}` endpoint without any change
    /// to the extractor or routing code.
    #[test]
    fn set_attributes_deserializes_from_http_body() {
        let body =
            r#"{"SetAttributes":{"changes":{"search/origin":"LHR","search/destination":"JFK"}}}"#;
        let cmd: JourneyCommand = serde_json::from_str(body).unwrap();

        let JourneyCommand::SetAttributes { changes } = cmd else {
            panic!("expected SetAttributes, got something else");
        };

        assert_eq!(changes.len(), 2);
        let origin: AttributePath = "search/origin".parse().unwrap();
        let dest: AttributePath = "search/destination".parse().unwrap();
        assert_eq!(changes[&origin], json!("LHR"));
        assert_eq!(changes[&dest], json!("JFK"));
    }

    /// Legacy `Capture` command must still deserialise so existing clients
    /// are not broken.
    #[allow(deprecated)]
    #[test]
    fn legacy_capture_still_deserializes() {
        let body = r#"{"Capture":{"step":"search","data":{"origin":"LHR"}}}"#;
        let cmd: JourneyCommand = serde_json::from_str(body).unwrap();
        assert!(matches!(cmd, JourneyCommand::Capture { .. }));
    }
}
