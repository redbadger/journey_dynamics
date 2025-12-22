use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct DataMerger {
    /// The current merged state of all captured data
    merged_data: Value,
    /// History of all data capture operations for audit purposes
    capture_history: Vec<(String, Value)>,
}

impl Default for DataMerger {
    fn default() -> Self {
        Self {
            merged_data: serde_json::json!({}),
            capture_history: Vec::new(),
        }
    }
}

impl DataMerger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge new form data into the existing data using deep merge semantics
    pub fn merge_form_data(&mut self, key: &str, data: &Value) -> Result<(), DataMergerError> {
        // Store the operation in history
        self.capture_history.push((key.to_string(), data.clone()));

        // Deep merge the data based on the key
        match key {
            "capturedData" => {
                // Direct merge into the root captured data
                self.deep_merge_into_root(data)?;
            }
            _ => {
                // Step-specific or other data - merge as a top-level field or deep merge if it exists
                self.merge_field_data(key, data)?;
            }
        }

        Ok(())
    }

    /// Get the current merged data state
    pub fn get_merged_data(&self) -> &Value {
        &self.merged_data
    }

    /// Get the capture history
    pub fn get_capture_history(&self) -> &[(String, Value)] {
        &self.capture_history
    }

    /// Deep merge data directly into the root level
    fn deep_merge_into_root(&mut self, data: &Value) -> Result<(), DataMergerError> {
        if let Value::Object(new_data) = data {
            if let Value::Object(ref mut current_data) = self.merged_data {
                for (key, value) in new_data {
                    Self::deep_merge_value(current_data, key, value);
                }
            } else {
                return Err(DataMergerError::InvalidStructure(
                    "Root data must be an object".to_string(),
                ));
            }
        } else {
            return Err(DataMergerError::InvalidStructure(
                "Data to merge must be an object".to_string(),
            ));
        }
        Ok(())
    }

    /// Merge field-specific data
    fn merge_field_data(&mut self, field_key: &str, data: &Value) -> Result<(), DataMergerError> {
        if let Value::Object(ref mut current_data) = self.merged_data {
            // For step data (arbitrary keys), merge the object contents directly into root
            if let Value::Object(new_obj) = data {
                for (key, value) in new_obj {
                    Self::deep_merge_value(current_data, key, value);
                }
            } else {
                // For non-object data, store it under the field key
                current_data.insert(field_key.to_string(), data.clone());
            }
        } else {
            return Err(DataMergerError::InvalidStructure(
                "Root data must be an object".to_string(),
            ));
        }
        Ok(())
    }

    /// Recursively deep merge a value into an existing object
    fn deep_merge_value(target: &mut serde_json::Map<String, Value>, key: &str, new_value: &Value) {
        match (target.get_mut(key), new_value) {
            // Both are objects - merge recursively
            (Some(Value::Object(existing)), Value::Object(new_obj)) => {
                for (nested_key, nested_value) in new_obj {
                    Self::deep_merge_value(existing, nested_key, nested_value);
                }
            }
            // Arrays - replace entirely (could implement array merging strategies if needed)
            (_, Value::Array(_)) => {
                target.insert(key.to_string(), new_value.clone());
            }
            // All other cases - replace the value
            _ => {
                target.insert(key.to_string(), new_value.clone());
            }
        }
    }

    /// Get a specific field from the merged data using dot notation
    pub fn get_field(&self, field_path: &str) -> Option<&Value> {
        let parts: Vec<&str> = field_path.split('.').collect();
        let mut current = &self.merged_data;

        for part in parts {
            current = current.get(part)?;
        }

        Some(current)
    }

    /// Create a flattened view of all field paths and their values
    pub fn flatten_fields(&self) -> HashMap<String, &Value> {
        let mut result = HashMap::new();
        self.flatten_recursive("", &self.merged_data, &mut result);
        result
    }

    /// Recursively flatten JSON structure into dot-notation paths
    fn flatten_recursive<'a>(
        &self,
        prefix: &str,
        value: &'a Value,
        result: &mut HashMap<String, &'a Value>,
    ) {
        match value {
            Value::Object(obj) => {
                for (key, val) in obj {
                    let new_prefix = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{}.{}", prefix, key)
                    };
                    result.insert(new_prefix.clone(), val);
                    self.flatten_recursive(&new_prefix, val, result);
                }
            }
            Value::Array(arr) => {
                for (index, val) in arr.iter().enumerate() {
                    let new_prefix = format!("{}[{}]", prefix, index);
                    result.insert(new_prefix.clone(), val);
                    self.flatten_recursive(&new_prefix, val, result);
                }
            }
            _ => {
                // Leaf values are already inserted by the parent call
            }
        }
    }

    /// Reset the merger to initial state
    pub fn reset(&mut self) {
        self.merged_data = serde_json::json!({});
        self.capture_history.clear();
    }

    /// Get the number of merge operations performed
    pub fn operation_count(&self) -> usize {
        self.capture_history.len()
    }

    /// Check if a specific field path exists in the merged data
    pub fn has_field(&self, field_path: &str) -> bool {
        self.get_field(field_path).is_some()
    }

    /// Get all top-level keys in the merged data
    pub fn get_top_level_keys(&self) -> Vec<String> {
        if let Value::Object(obj) = &self.merged_data {
            obj.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    /// Create a JSON merge patch representation
    /// This creates a patch that would transform an empty object into the current state
    pub fn to_merge_patch(&self) -> &Value {
        &self.merged_data
    }

    /// Apply a merge patch to the current data
    /// This follows RFC 7386 JSON Merge Patch semantics
    pub fn apply_merge_patch(&mut self, patch: &Value) -> Result<(), DataMergerError> {
        self.merge_form_data("merge_patch", patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DataMergerError {
    #[error("Invalid data structure: {0}")]
    InvalidStructure(String),

    #[error("Field not found: {0}")]
    FieldNotFound(String),

    #[error("Merge operation failed: {0}")]
    MergeError(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_merge_simple_objects() {
        let mut merger = DataMerger::new();

        let data1 = json!({
            "name": "John",
            "age": 30
        });

        let data2 = json!({
            "email": "john@example.com",
            "age": 31
        });

        merger.merge_form_data("step1", &data1).unwrap();
        merger.merge_form_data("step2", &data2).unwrap();

        assert_eq!(merger.get_field("name"), Some(&json!("John")));
        assert_eq!(merger.get_field("age"), Some(&json!(31))); // Should be updated
        assert_eq!(merger.get_field("email"), Some(&json!("john@example.com")));
    }

    #[test]
    fn test_deep_merge_nested_objects() {
        let mut merger = DataMerger::new();

        let data1 = json!({
            "user": {
                "name": "John",
                "address": {
                    "city": "London"
                }
            }
        });

        let data2 = json!({
            "user": {
                "email": "john@example.com",
                "address": {
                    "country": "UK"
                }
            }
        });

        merger.merge_form_data("capturedData", &data1).unwrap();
        merger.merge_form_data("capturedData", &data2).unwrap();

        assert_eq!(merger.get_field("user.name"), Some(&json!("John")));
        assert_eq!(
            merger.get_field("user.email"),
            Some(&json!("john@example.com"))
        );
        assert_eq!(
            merger.get_field("user.address.city"),
            Some(&json!("London"))
        );
        assert_eq!(merger.get_field("user.address.country"), Some(&json!("UK")));
    }

    #[test]
    fn test_array_replacement() {
        let mut merger = DataMerger::new();

        let data1 = json!({
            "items": [1, 2, 3]
        });

        let data2 = json!({
            "items": [4, 5]
        });

        merger.merge_form_data("capturedData", &data1).unwrap();
        merger.merge_form_data("capturedData", &data2).unwrap();

        assert_eq!(merger.get_field("items"), Some(&json!([4, 5])));
    }

    #[test]
    fn test_field_path_access() {
        let mut merger = DataMerger::new();

        let data = json!({
            "level1": {
                "level2": {
                    "level3": "deep_value"
                }
            }
        });

        merger.merge_form_data("test", &data).unwrap();

        assert!(merger.has_field("level1"));
        assert!(merger.has_field("level1.level2"));
        assert!(merger.has_field("level1.level2.level3"));
        assert!(!merger.has_field("level1.level2.level4"));

        assert_eq!(
            merger.get_field("level1.level2.level3"),
            Some(&json!("deep_value"))
        );
    }

    #[test]
    fn test_flatten_fields() {
        let mut merger = DataMerger::new();

        let data = json!({
            "user": {
                "name": "John",
                "contact": {
                    "email": "john@example.com"
                }
            },
            "items": [1, 2]
        });

        merger.merge_form_data("test", &data).unwrap();
        let flattened = merger.flatten_fields();

        assert!(flattened.contains_key("user"));
        assert!(flattened.contains_key("user.name"));
        assert!(flattened.contains_key("user.contact"));
        assert!(flattened.contains_key("user.contact.email"));
        assert!(flattened.contains_key("items"));
        assert!(flattened.contains_key("items[0]"));
        assert!(flattened.contains_key("items[1]"));
    }

    #[test]
    fn test_capture_history() {
        let mut merger = DataMerger::new();

        merger.merge_form_data("step1", &json!({"a": 1})).unwrap();
        merger.merge_form_data("step2", &json!({"b": 2})).unwrap();

        let history = merger.get_capture_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].0, "step1");
        assert_eq!(history[1].0, "step2");
        assert_eq!(merger.operation_count(), 2);
    }

    #[test]
    fn test_reset() {
        let mut merger = DataMerger::new();

        merger
            .merge_form_data("test", &json!({"key": "value"}))
            .unwrap();
        assert_eq!(merger.operation_count(), 1);

        merger.reset();
        assert_eq!(merger.operation_count(), 0);
        assert_eq!(merger.get_merged_data(), &json!({}));
    }

    #[test]
    fn test_top_level_keys() {
        let mut merger = DataMerger::new();

        merger
            .merge_form_data("step1", &json!({"field1": "value1"}))
            .unwrap();
        merger
            .merge_form_data("step2", &json!({"field2": "value2"}))
            .unwrap();

        let keys = merger.get_top_level_keys();
        assert!(keys.contains(&"field1".to_string()));
        assert!(keys.contains(&"field2".to_string()));
        assert_eq!(keys.len(), 2);
    }
}
