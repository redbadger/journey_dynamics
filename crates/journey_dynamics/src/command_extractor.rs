use axum::{
    body::{Body, Bytes},
    extract::FromRequest,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use std::collections::HashMap;

use crate::domain::{commands::JourneyCommand, flatten};

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
            let mut raw: serde_json::Value = serde_json::from_slice(&body)?;
            normalize_set_attributes(&mut raw);
            serde_json::from_value(raw)?
        };

        Ok(Self(metadata, command))
    }
}

/// Normalise the nested-form sugar for `SetAttributes` into the canonical
/// `{ "changes": { ... } }` form before deserialisation.
///
/// The canonical form is:
/// ```json
/// { "SetAttributes": { "changes": { "search/origin": "LHR" } } }
/// ```
///
/// The sugar form uses a nested JSON object whose keys are the top-level path
/// segments:
/// ```json
/// { "SetAttributes": { "search": { "origin": "LHR" } } }
/// ```
///
/// **Detection rule:** if the inner object has exactly one key named
/// `"changes"` it is already in canonical form and is left untouched. Any
/// other shape is treated as the nested sugar form and flattened via
/// [`flatten`].
fn normalize_set_attributes(value: &mut serde_json::Value) {
    // Only act on objects that carry a "SetAttributes" key.
    let Some(inner) = value
        .as_object()
        .and_then(|obj| obj.get("SetAttributes"))
        .cloned()
    else {
        return;
    };

    // Only transform if the inner value is itself an object.
    let Some(inner_obj) = inner.as_object() else {
        return;
    };

    // Canonical form: exactly one key named "changes" — leave as-is.
    if inner_obj.len() == 1 && inner_obj.contains_key("changes") {
        return;
    }

    // Sugar form: flatten the nested object into a path-keyed changes map.
    let flat = flatten(&inner);
    let changes: serde_json::Map<String, serde_json::Value> = flat
        .into_iter()
        .map(|(path, val)| (path.as_str().to_owned(), val))
        .collect();

    // Rewrite to canonical form in place.
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "SetAttributes".to_owned(),
            serde_json::json!({ "changes": serde_json::Value::Object(changes) }),
        );
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

    use super::normalize_set_attributes;

    // ── canonical form ────────────────────────────────────────────────────────

    /// The explicit `{ "changes": { ... } }` form must deserialise correctly.
    ///
    /// ```json
    /// { "SetAttributes": { "changes": { "search/origin": "LHR" } } }
    /// ```
    #[test]
    fn set_attributes_canonical_form_deserializes() {
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

    /// The normaliser must leave the canonical form untouched.
    #[test]
    fn normalize_leaves_canonical_form_unchanged() {
        let mut raw = json!({
            "SetAttributes": {
                "changes": { "search/origin": "LHR" }
            }
        });
        normalize_set_attributes(&mut raw);
        assert_eq!(
            raw,
            json!({ "SetAttributes": { "changes": { "search/origin": "LHR" } } })
        );
    }

    // ── nested-form sugar ─────────────────────────────────────────────────────

    /// A shallow nested sugar form is flattened to path-keyed changes.
    ///
    /// ```json
    /// { "SetAttributes": { "search": { "origin": "LHR" } } }
    /// ```
    /// becomes
    /// ```json
    /// { "SetAttributes": { "changes": { "search/origin": "LHR" } } }
    /// ```
    #[test]
    fn normalize_flattens_shallow_nested_sugar() {
        let mut raw = json!({
            "SetAttributes": { "search": { "origin": "LHR", "destination": "JFK" } }
        });
        normalize_set_attributes(&mut raw);
        assert_eq!(
            raw,
            json!({
                "SetAttributes": {
                    "changes": {
                        "search/destination": "JFK",
                        "search/origin": "LHR"
                    }
                }
            })
        );
    }

    /// The nested sugar form deserialises into the same `SetAttributes` command
    /// as the canonical form.
    #[test]
    fn set_attributes_nested_sugar_deserializes_via_normaliser() {
        let mut raw = json!({
            "SetAttributes": { "search": { "origin": "LHR", "destination": "JFK" } }
        });
        normalize_set_attributes(&mut raw);
        let cmd: JourneyCommand = serde_json::from_value(raw).unwrap();

        let JourneyCommand::SetAttributes { changes } = cmd else {
            panic!("expected SetAttributes, got something else");
        };

        assert_eq!(changes.len(), 2);
        let origin: AttributePath = "search/origin".parse().unwrap();
        let dest: AttributePath = "search/destination".parse().unwrap();
        assert_eq!(changes[&origin], json!("LHR"));
        assert_eq!(changes[&dest], json!("JFK"));
    }

    /// Multiple top-level sections in one sugar-form body are all flattened.
    #[test]
    fn normalize_flattens_multiple_sections() {
        let mut raw = json!({
            "SetAttributes": {
                "search": { "origin": "LHR" },
                "booking": { "class": "economy" }
            }
        });
        normalize_set_attributes(&mut raw);
        let cmd: JourneyCommand = serde_json::from_value(raw).unwrap();

        let JourneyCommand::SetAttributes { changes } = cmd else {
            panic!("expected SetAttributes");
        };
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[&"search/origin".parse::<AttributePath>().unwrap()],
            json!("LHR")
        );
        assert_eq!(
            changes[&"booking/class".parse::<AttributePath>().unwrap()],
            json!("economy")
        );
    }

    /// Deep nesting (e.g. persons array) is also flattened correctly.
    #[test]
    fn normalize_flattens_deep_nested_persons() {
        let mut raw = json!({
            "SetAttributes": {
                "persons": [
                    { "firstName": "Alice", "passportNumber": "X123" }
                ]
            }
        });
        normalize_set_attributes(&mut raw);
        let cmd: JourneyCommand = serde_json::from_value(raw).unwrap();

        let JourneyCommand::SetAttributes { changes } = cmd else {
            panic!("expected SetAttributes");
        };
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[&"persons/0/firstName".parse::<AttributePath>().unwrap()],
            json!("Alice")
        );
        assert_eq!(
            changes[&"persons/0/passportNumber".parse::<AttributePath>().unwrap()],
            json!("X123")
        );
    }

    // ── non-SetAttributes commands ────────────────────────────────────────────────

    /// The normaliser is a no-op for non-SetAttributes commands.
    #[test]
    fn normalize_is_noop_for_other_commands() {
        let original = json!({ "Complete": null });
        let mut raw = original.clone();
        normalize_set_attributes(&mut raw);
        assert_eq!(raw, original);
    }
}
