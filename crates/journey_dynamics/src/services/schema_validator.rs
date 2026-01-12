use serde_json::Value;
use thiserror::Error;

/// Trait for validating data against schemas
pub trait SchemaValidator: Send + Sync {
    /// Validate data against a schema
    ///
    /// # Errors
    /// Returns an error if the data fails schema validation
    fn validate(&self, data: &Value) -> Result<(), SchemaValidationError>;
}

/// Error types for schema validation
#[derive(Debug, Error)]
pub enum SchemaValidationError {
    #[error("Schema validation failed: {0}")]
    ValidationFailed(String),

    #[error("Schema not found: {0}")]
    SchemaNotFound(String),

    #[error("Invalid schema: {0}")]
    InvalidSchema(String),

    #[error("JSON processing error: {0}")]
    JsonError(String),
}

/// No-op validator that accepts all data
pub struct NoOpValidator;

impl SchemaValidator for NoOpValidator {
    fn validate(&self, _data: &Value) -> Result<(), SchemaValidationError> {
        Ok(())
    }
}

/// JSON Schema validator using the jsonschema crate
#[derive(Debug)]
pub struct JsonSchemaValidator {
    validator: jsonschema::Validator,
}

impl JsonSchemaValidator {
    /// Create a new validator from a JSON schema
    ///
    /// # Errors
    /// Returns an error if the schema cannot be compiled
    pub fn new(schema: &Value) -> Result<Self, SchemaValidationError> {
        let validator = jsonschema::validator_for(schema)
            .map_err(|e| SchemaValidationError::InvalidSchema(e.to_string()))?;

        println!("{:?}", validator);
        Ok(Self { validator })
    }

    /// Create a new validator from a JSON schema string
    ///
    /// # Errors
    /// Returns an error if the schema cannot be parsed or compiled
    pub fn from_json_str(schema_str: &str) -> Result<Self, SchemaValidationError> {
        let schema: Value = serde_json::from_str(schema_str)
            .map_err(|e| SchemaValidationError::JsonError(e.to_string()))?;
        Self::new(&schema)
    }
}

impl SchemaValidator for JsonSchemaValidator {
    fn validate(&self, data: &Value) -> Result<(), SchemaValidationError> {
        println!("{:?}", self);
        // Use iter_errors to get an iterator over validation errors
        let errors: Vec<String> = self
            .validator
            .iter_errors(data)
            .map(|error| error.to_string())
            .collect();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(SchemaValidationError::ValidationFailed(errors.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_no_op_validator() {
        let validator = NoOpValidator;
        let data = json!({"anything": "goes"});

        assert!(validator.validate(&data).is_ok());
    }

    #[test]
    fn test_json_schema_validator_basic() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string"
                },
                "age": {
                    "type": "number",
                    "minimum": 0
                }
            },
            "required": ["name", "age"]
        });

        let validator = JsonSchemaValidator::new(&schema).unwrap();

        let valid_data = json!({
            "name": "John",
            "age": 30
        });

        let invalid_data_missing_field = json!({
            "name": "John"
        });

        let invalid_data_wrong_type = json!({
            "name": "John",
            "age": "thirty"
        });

        let invalid_data_constraint = json!({
            "name": "John",
            "age": -5
        });

        assert!(validator.validate(&valid_data).is_ok());
        assert!(validator.validate(&invalid_data_missing_field).is_err());
        assert!(validator.validate(&invalid_data_wrong_type).is_err());
        assert!(validator.validate(&invalid_data_constraint).is_err());
    }

    #[test]
    fn test_json_schema_validator_with_refs() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "tripType": {
                    "$ref": "#/$defs/TripType"
                },
                "status": {
                    "$ref": "#/$defs/BookingStatus"
                }
            },
            "required": ["tripType", "status"],
            "$defs": {
                "TripType": {
                    "type": "string",
                    "enum": ["one-way", "round-trip", "multi-city"]
                },
                "BookingStatus": {
                    "type": "string",
                    "enum": ["search_criteria", "completed"]
                }
            }
        });

        let validator = JsonSchemaValidator::new(&schema).unwrap();

        let valid_data = json!({
            "tripType": "round-trip",
            "status": "search_criteria"
        });

        let invalid_enum_data = json!({
            "tripType": "invalid-trip-type",
            "status": "search_criteria"
        });

        assert!(validator.validate(&valid_data).is_ok());
        assert!(validator.validate(&invalid_enum_data).is_err());
    }
}
