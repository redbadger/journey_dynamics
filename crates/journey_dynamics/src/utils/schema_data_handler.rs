use serde_json::Value;

/// Schema-aware data handler that uses JSON Merge Patch for data merging
/// and validates against the generated JSON schema structure
#[derive(Debug, Clone)]
pub struct SchemaDataHandler {
    /// The current merged state of all captured data
    merged_data: Value,
}

impl Default for SchemaDataHandler {
    fn default() -> Self {
        Self {
            merged_data: serde_json::json!({}),
        }
    }
}

impl SchemaDataHandler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge new form data into the existing data using JSON Merge Patch semantics
    /// This follows RFC 7386 JSON Merge Patch standard
    ///
    /// # Errors
    /// Returns an error if the data cannot be merged or patch application fails.
    pub fn merge_form_data(
        &mut self,
        key: &str,
        data: &Value,
    ) -> Result<(), SchemaDataHandlerError> {
        // Create a merge patch from the incoming data
        let patch = match key {
            "capturedData" => {
                // Direct merge into the root captured data
                data.clone()
            }
            _ => {
                // Step-specific or other data - merge as object properties
                if let Value::Object(obj) = data {
                    // Merge object properties directly into root
                    Value::Object(obj.clone())
                } else {
                    // For non-object data, store it under the field key
                    serde_json::json!({
                        key: data
                    })
                }
            }
        };

        // Apply the merge patch using json_patch crate
        self.apply_merge_patch(&patch)?;

        Ok(())
    }

    /// Apply a JSON Merge Patch to the current data
    /// This follows RFC 7386 JSON Merge Patch semantics
    ///
    /// # Errors
    /// Returns an error if the patch is invalid or cannot be applied.
    pub fn apply_merge_patch(&mut self, patch: &Value) -> Result<(), SchemaDataHandlerError> {
        // Use json_patch to merge the data
        json_patch::merge(&mut self.merged_data, patch);
        Ok(())
    }

    /// Get the current merged data state
    #[must_use]
    pub fn get_merged_data(&self) -> &Value {
        &self.merged_data
    }

    /// Get a specific field from the merged data using dot notation
    #[must_use]
    pub fn get_field(&self, field_path: &str) -> Option<&Value> {
        let parts: Vec<&str> = field_path.split('.').collect();
        let mut current = &self.merged_data;

        for part in parts {
            current = current.get(part)?;
        }

        Some(current)
    }

    /// Reset the handler to initial state
    pub fn reset(&mut self) {
        self.merged_data = serde_json::json!({});
    }

    /// Check if a specific field path exists in the merged data
    #[must_use]
    pub fn has_field(&self, field_path: &str) -> bool {
        self.get_field(field_path).is_some()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaDataHandlerError {
    #[error("JSON Patch operation failed: {0}")]
    PatchError(String),

    #[error("Schema compilation error: {0}")]
    SchemaError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Data structure error: {0}")]
    StructureError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_merge_simple_objects() {
        let mut handler = SchemaDataHandler::new();

        let data1 = json!({
            "name": "John",
            "age": 30
        });

        let data2 = json!({
            "email": "john@example.com",
            "age": 31
        });

        handler.merge_form_data("step1", &data1).unwrap();
        handler.merge_form_data("step2", &data2).unwrap();

        assert_eq!(handler.get_field("name"), Some(&json!("John")));
        assert_eq!(handler.get_field("age"), Some(&json!(31))); // Should be updated
        assert_eq!(handler.get_field("email"), Some(&json!("john@example.com")));
    }

    #[test]
    fn test_merge_patch_semantics() {
        let mut handler = SchemaDataHandler::new();

        let initial_data = json!({
            "user": {
                "name": "John",
                "address": {
                    "city": "London"
                }
            }
        });

        let patch_data = json!({
            "user": {
                "email": "john@example.com",
                "address": {
                    "country": "UK"
                }
            }
        });

        handler
            .merge_form_data("capturedData", &initial_data)
            .unwrap();
        handler
            .merge_form_data("capturedData", &patch_data)
            .unwrap();

        // JSON Merge Patch should preserve existing fields while adding new ones
        assert_eq!(handler.get_field("user.name"), Some(&json!("John")));
        assert_eq!(
            handler.get_field("user.email"),
            Some(&json!("john@example.com"))
        );
        assert_eq!(
            handler.get_field("user.address.city"),
            Some(&json!("London"))
        );
        assert_eq!(
            handler.get_field("user.address.country"),
            Some(&json!("UK"))
        );
    }

    #[test]
    fn test_field_path_access() {
        let mut handler = SchemaDataHandler::new();

        let data = json!({
            "level1": {
                "level2": {
                    "level3": "deep_value"
                }
            }
        });

        handler.merge_form_data("test", &data).unwrap();

        assert!(handler.has_field("level1"));
        assert!(handler.has_field("level1.level2"));
        assert!(handler.has_field("level1.level2.level3"));
        assert!(!handler.has_field("level1.level2.level4"));

        assert_eq!(
            handler.get_field("level1.level2.level3"),
            Some(&json!("deep_value"))
        );
    }

    #[test]
    fn test_reset_functionality() {
        let mut handler = SchemaDataHandler::new();

        handler
            .merge_form_data("test", &json!({"key": "value"}))
            .unwrap();
        assert!(handler.has_field("key"));

        handler.reset();
        assert!(!handler.has_field("key"));
        assert_eq!(handler.get_merged_data(), &json!({}));
    }

    #[test]
    fn test_direct_patch_application() {
        let mut handler = SchemaDataHandler::new();

        // Set initial data
        let initial = json!({
            "name": "John",
            "settings": {
                "theme": "dark",
                "notifications": true
            }
        });
        handler.apply_merge_patch(&initial).unwrap();

        // Apply a merge patch
        let patch = json!({
            "age": 30,
            "settings": {
                "theme": "light"
            }
        });
        handler.apply_merge_patch(&patch).unwrap();

        // Verify the merge patch semantics
        assert_eq!(handler.get_field("name"), Some(&json!("John")));
        assert_eq!(handler.get_field("age"), Some(&json!(30)));
        assert_eq!(handler.get_field("settings.theme"), Some(&json!("light")));
        assert_eq!(
            handler.get_field("settings.notifications"),
            Some(&json!(true))
        );
    }
}
