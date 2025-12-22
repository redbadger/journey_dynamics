// use schemars::JsonSchema;
// use serde_json::Value;
// use std::collections::HashMap;

// #[derive(Debug, Clone)]
// pub struct SchemaValidator {
//     schemas: HashMap<String, schemars::schema::RootSchema>,
// }

// impl Default for SchemaValidator {
//     fn default() -> Self {
//         Self {
//             schemas: HashMap::new(),
//         }
//     }
// }

// impl SchemaValidator {
//     pub fn new() -> Self {
//         Self::default()
//     }

//     /// Register a new schema for validation
//     pub fn register_schema<T: JsonSchema>(&mut self, name: &str) {
//         let schema = schemars::schema_for!(T);
//         self.schemas.insert(name.to_string(), schema);
//     }

//     /// Register a schema from a JSON schema value
//     pub fn register_schema_from_value(
//         &mut self,
//         name: &str,
//         schema: Value,
//     ) -> Result<(), ValidationError> {
//         let root_schema: RootSchema = serde_json::from_value(schema)
//             .map_err(|e| ValidationError::SchemaError(e.to_string()))?;
//         self.schemas.insert(name.to_string(), root_schema);
//         Ok(())
//     }

//     /// Validate JSON data against a registered schema
//     pub fn validate(&self, schema_name: &str, data: &Value) -> Result<(), ValidationError> {
//         let schema = self
//             .schemas
//             .get(schema_name)
//             .ok_or_else(|| ValidationError::SchemaNotFound(schema_name.to_string()))?;

//         // Convert schemars schema to a format we can validate against
//         let schema_value = serde_json::to_value(schema)
//             .map_err(|e| ValidationError::SchemaError(e.to_string()))?;

//         // Basic validation - check required fields and types
//         self.validate_recursive(data, &schema_value, "")
//     }

//     /// Check if data conforms to a partial schema (useful for progressive validation)
//     pub fn validate_partial(
//         &self,
//         schema_name: &str,
//         data: &Value,
//         required_fields: &[&str],
//     ) -> Result<(), ValidationError> {
//         // First run full validation
//         self.validate(schema_name, data)?;

//         // Check if required fields are present
//         for field in required_fields {
//             if !self.has_field(data, field) {
//                 return Err(ValidationError::MissingRequiredField(field.to_string()));
//             }
//         }

//         Ok(())
//     }

//     /// Extract and validate a specific field from data
//     pub fn validate_field<'a>(
//         &self,
//         schema_name: &str,
//         data: &'a Value,
//         field_path: &str,
//     ) -> Result<&'a Value, ValidationError> {
//         let field_value = self
//             .get_field(data, field_path)
//             .ok_or_else(|| ValidationError::FieldNotFound(field_path.to_string()))?;

//         // For now, we assume the field is valid if it exists and the parent validates
//         // More sophisticated field-level validation could be added here
//         self.validate(schema_name, data)?;

//         Ok(field_value)
//     }

//     /// Get validation errors without failing
//     pub fn get_validation_errors(&self, schema_name: &str, data: &Value) -> Vec<ValidationError> {
//         match self.validate(schema_name, data) {
//             Ok(_) => vec![],
//             Err(e) => vec![e],
//         }
//     }

//     /// Check if a field exists in the data using dot notation
//     fn has_field(&self, data: &Value, field_path: &str) -> bool {
//         self.get_field(data, field_path).is_some()
//     }

//     /// Get a field from data using dot notation
//     fn get_field<'a>(&self, data: &'a Value, field_path: &str) -> Option<&'a Value> {
//         let parts: Vec<&str> = field_path.split('.').collect();
//         let mut current = data;

//         for part in parts {
//             // Handle array indices like [0], [1], etc.
//             if part.starts_with('[') && part.ends_with(']') {
//                 let index_str = &part[1..part.len() - 1];
//                 let index: usize = index_str.parse().ok()?;
//                 current = current.get(index)?;
//             } else if let Some(bracket_pos) = part.find('[') {
//                 // Handle cases like "items[0]" where field name and array index are in same token
//                 let field_name = &part[..bracket_pos];
//                 let index_part = &part[bracket_pos..];

//                 if index_part.starts_with('[') && index_part.ends_with(']') {
//                     // First access the field
//                     current = current.get(field_name)?;
//                     // Then access the array index
//                     let index_str = &index_part[1..index_part.len() - 1];
//                     let index: usize = index_str.parse().ok()?;
//                     current = current.get(index)?;
//                 } else {
//                     current = current.get(part)?;
//                 }
//             } else {
//                 current = current.get(part)?;
//             }
//         }

//         Some(current)
//     }

//     /// Recursively validate JSON data against a schema
//     fn validate_recursive(
//         &self,
//         data: &Value,
//         schema: &Value,
//         path: &str,
//     ) -> Result<(), ValidationError> {
//         match (data, schema) {
//             (Value::Object(data_obj), Value::Object(schema_obj)) => {
//                 // Check if this is a schema definition
//                 if let Some(properties) = schema_obj.get("properties") {
//                     if let Value::Object(props) = properties {
//                         // Validate each property in the data
//                         for (key, value) in data_obj {
//                             if let Some(prop_schema) = props.get(key) {
//                                 let new_path = if path.is_empty() {
//                                     key.clone()
//                                 } else {
//                                     format!("{}.{}", path, key)
//                                 };
//                                 self.validate_recursive(value, prop_schema, &new_path)?;
//                             }
//                         }

//                         // Check for required fields
//                         if let Some(Value::Array(required)) = schema_obj.get("required") {
//                             for req_field in required {
//                                 if let Value::String(field_name) = req_field {
//                                     if !data_obj.contains_key(field_name) {
//                                         let full_path = if path.is_empty() {
//                                             field_name.clone()
//                                         } else {
//                                             format!("{}.{}", path, field_name)
//                                         };
//                                         return Err(ValidationError::MissingRequiredField(
//                                             full_path,
//                                         ));
//                                     }
//                                 }
//                             }
//                         }
//                     }
//                 }

//                 // Handle type validation
//                 if let Some(Value::String(expected_type)) = schema_obj.get("type") {
//                     let actual_type = match data {
//                         Value::Object(_) => "object",
//                         Value::Array(_) => "array",
//                         Value::String(_) => "string",
//                         Value::Number(n) => {
//                             // Check if it's an integer when expected_type is "integer"
//                             if expected_type == "integer" && n.is_f64() {
//                                 let float_val = n.as_f64().unwrap();
//                                 if float_val.fract() == 0.0 {
//                                     "integer"
//                                 } else {
//                                     "number"
//                                 }
//                             } else if expected_type == "integer" && (n.is_i64() || n.is_u64()) {
//                                 "integer"
//                             } else {
//                                 "number"
//                             }
//                         }
//                         Value::Bool(_) => "boolean",
//                         Value::Null => "null",
//                     };

//                     // Accept number for integer and vice versa for JSON compatibility
//                     let types_match = expected_type == actual_type
//                         || (expected_type == "integer" && actual_type == "number")
//                         || (expected_type == "number" && actual_type == "integer");

//                     if !types_match {
//                         return Err(ValidationError::TypeMismatch {
//                             path: path.to_string(),
//                             expected: expected_type.clone(),
//                             actual: actual_type.to_string(),
//                         });
//                     }
//                 }
//             }
//             (Value::Array(data_arr), Value::Object(schema_obj)) => {
//                 // Handle array validation
//                 if let Some(items_schema) = schema_obj.get("items") {
//                     for (index, item) in data_arr.iter().enumerate() {
//                         let new_path = if path.is_empty() {
//                             format!("[{}]", index)
//                         } else {
//                             format!("{}[{}]", path, index)
//                         };
//                         self.validate_recursive(item, items_schema, &new_path)?;
//                     }
//                 }
//             }
//             _ => {
//                 // For primitive types, basic type checking
//                 if let Value::Object(schema_obj) = schema {
//                     if let Some(Value::String(expected_type)) = schema_obj.get("type") {
//                         let actual_type = match data {
//                             Value::String(_) => "string",
//                             Value::Number(n) => {
//                                 if expected_type == "integer" && n.is_f64() {
//                                     let float_val = n.as_f64().unwrap();
//                                     if float_val.fract() == 0.0 {
//                                         "integer"
//                                     } else {
//                                         "number"
//                                     }
//                                 } else if expected_type == "integer" && (n.is_i64() || n.is_u64()) {
//                                     "integer"
//                                 } else {
//                                     "number"
//                                 }
//                             }
//                             Value::Bool(_) => "boolean",
//                             Value::Null => "null",
//                             _ => "unknown",
//                         };

//                         // Accept number for integer and vice versa for JSON compatibility
//                         let types_match = expected_type == actual_type
//                             || (expected_type == "integer" && actual_type == "number")
//                             || (expected_type == "number" && actual_type == "integer");

//                         if !types_match {
//                             return Err(ValidationError::TypeMismatch {
//                                 path: path.to_string(),
//                                 expected: expected_type.clone(),
//                                 actual: actual_type.to_string(),
//                             });
//                         }
//                     }
//                 }
//             }
//         }

//         Ok(())
//     }

//     /// Get list of available schemas
//     pub fn available_schemas(&self) -> Vec<String> {
//         self.schemas.keys().cloned().collect()
//     }

//     /// Get the raw schema for inspection
//     pub fn get_schema(&self, schema_name: &str) -> Option<&schemars::schema::RootSchema> {
//         self.schemas.get(schema_name)
//     }

//     /// Remove a schema
//     pub fn remove_schema(&mut self, schema_name: &str) -> bool {
//         self.schemas.remove(schema_name).is_some()
//     }

//     /// Clear all schemas
//     pub fn clear_schemas(&mut self) {
//         self.schemas.clear();
//     }
// }

// #[derive(Debug, thiserror::Error)]
// pub enum ValidationError {
//     #[error("Schema not found: {0}")]
//     SchemaNotFound(String),

//     #[error("Schema error: {0}")]
//     SchemaError(String),

//     #[error("Missing required field: {0}")]
//     MissingRequiredField(String),

//     #[error("Field not found: {0}")]
//     FieldNotFound(String),

//     #[error("Type mismatch at {path}: expected {expected}, got {actual}")]
//     TypeMismatch {
//         path: String,
//         expected: String,
//         actual: String,
//     },

//     #[error("Validation failed: {0}")]
//     ValidationFailed(String),
// }

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use schemars::JsonSchema;
//     use serde::{Deserialize, Serialize};
//     use serde_json::json;

//     #[derive(JsonSchema, Serialize, Deserialize)]
//     struct TestUser {
//         name: String,
//         age: u32,
//         email: Option<String>,
//     }

//     #[test]
//     fn test_validator_creation() {
//         let validator = SchemaValidator::new();
//         assert!(validator.available_schemas().is_empty());
//     }

//     #[test]
//     fn test_schema_registration() {
//         let mut validator = SchemaValidator::new();
//         validator.register_schema::<TestUser>("user");

//         assert_eq!(validator.available_schemas().len(), 1);
//         assert!(validator.available_schemas().contains(&"user".to_string()));
//     }

//     #[test]
//     fn test_valid_data_validation() {
//         let mut validator = SchemaValidator::new();
//         validator.register_schema::<TestUser>("user");

//         let valid_data = json!({
//             "name": "John Doe",
//             "age": 30,
//             "email": "john@example.com"
//         });

//         assert!(validator.validate("user", &valid_data).is_ok());
//     }

//     #[test]
//     fn test_missing_required_field() {
//         let mut validator = SchemaValidator::new();
//         validator.register_schema::<TestUser>("user");

//         let invalid_data = json!({
//             "name": "John Doe"
//             // Missing required 'age' field
//         });

//         assert!(validator.validate("user", &invalid_data).is_err());
//     }

//     #[test]
//     fn test_partial_validation() {
//         let mut validator = SchemaValidator::new();
//         validator.register_schema::<TestUser>("user");

//         let partial_data = json!({
//             "name": "John Doe",
//             "age": 30
//         });

//         let required_fields = ["name", "age"];
//         assert!(
//             validator
//                 .validate_partial("user", &partial_data, &required_fields)
//                 .is_ok()
//         );
//     }

//     #[test]
//     fn test_field_access() {
//         let validator = SchemaValidator::new();

//         let data = json!({
//             "user": {
//                 "profile": {
//                     "name": "John"
//                 }
//             },
//             "items": [1, 2, 3]
//         });

//         assert_eq!(
//             validator.get_field(&data, "user.profile.name"),
//             Some(&json!("John"))
//         );
//         assert_eq!(validator.get_field(&data, "items[0]"), Some(&json!(1)));
//         assert!(validator.get_field(&data, "nonexistent").is_none());
//     }

//     #[test]
//     fn test_schema_removal() {
//         let mut validator = SchemaValidator::new();
//         validator.register_schema::<TestUser>("user");

//         assert_eq!(validator.available_schemas().len(), 1);
//         assert!(validator.remove_schema("user"));
//         assert_eq!(validator.available_schemas().len(), 0);
//         assert!(!validator.remove_schema("nonexistent"));
//     }
// }
