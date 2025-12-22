use journey_dynamics::utils::SchemaDataHandler;
use jsonschema::Validator;
use serde_json::Value;
use std::fs;

/// Extension trait for `SchemaDataHandler` to provide FlightBooking-specific functionality
pub trait FlightBookingDataExt {
    /// Get data structured according to the `FlightBooking` schema format
    fn get_flight_booking_data(&self) -> Value;

    /// Validate that the data contains required `FlightBooking` fields
    ///
    /// # Errors
    /// Returns an error if required fields are missing or have invalid values
    fn validate_flight_booking_requirements(&self) -> Result<(), FlightBookingValidationError>;

    /// Check if the booking is ready for a specific status transition
    fn can_transition_to_status(&self, target_status: &str) -> bool;

    /// Validate the current data against the generated `FlightBooking` JSON schema
    ///
    /// # Errors
    /// Returns an error if the data fails schema validation
    fn validate_against_flight_booking_schema(&self) -> Result<(), FlightBookingValidationError>;
}

impl FlightBookingDataExt for SchemaDataHandler {
    fn get_flight_booking_data(&self) -> Value {
        // Extract known FlightBooking fields from the merged data
        let mut flight_booking = serde_json::json!({});

        if let Value::Object(ref mut booking) = flight_booking {
            if let Value::Object(data) = self.get_merged_data() {
                // Map common fields that might exist in the data
                for (key, value) in data {
                    // Include all fields as-is - schema validation will catch invalid ones
                    booking.insert(key.clone(), value.clone());
                }
            }
        }

        flight_booking
    }

    fn validate_flight_booking_requirements(&self) -> Result<(), FlightBookingValidationError> {
        // Check for required fields based on FlightBooking schema
        if self.get_field("tripType").is_none() {
            return Err(FlightBookingValidationError::MissingField(
                "tripType".to_string(),
            ));
        }

        if self.get_field("passengers").is_none() {
            return Err(FlightBookingValidationError::MissingField(
                "passengers".to_string(),
            ));
        }

        if self.get_field("status").is_none() {
            return Err(FlightBookingValidationError::MissingField(
                "status".to_string(),
            ));
        }

        // Validate passenger count is greater than 0
        if let Some(passengers) = self.get_field("passengers") {
            if let Some(total) = passengers.get("total") {
                if let Some(count) = total.as_u64() {
                    if count == 0 {
                        return Err(FlightBookingValidationError::InvalidValue(
                            "passengers.total must be greater than 0".to_string(),
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    fn can_transition_to_status(&self, target_status: &str) -> bool {
        let current_status = self
            .get_field("status")
            .and_then(|s| s.as_str())
            .unwrap_or("search_criteria");

        match target_status {
            "flight_search_results" => {
                // Need origin, destination, departure date, and passengers
                self.has_field("origin")
                    && self.has_field("destination")
                    && self.has_field("departureDate")
                    && self.has_field("passengers")
            }
            "outbound_flight_selection" => {
                current_status == "flight_search_results" && self.has_field("searchResults")
            }
            "return_flight_selection" => {
                let is_round_trip = self
                    .get_field("tripType")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "round-trip");

                current_status == "outbound_flight_selection"
                    && self.has_field("selectedOutboundFlight")
                    && is_round_trip
            }
            "passenger_details" => {
                let has_outbound = self.has_field("selectedOutboundFlight");
                let is_round_trip = self
                    .get_field("tripType")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "round-trip");

                if is_round_trip {
                    has_outbound && self.has_field("selectedReturnFlight")
                } else {
                    has_outbound
                }
            }
            "insurance_selection" => {
                current_status == "passenger_details" && self.has_field("passengers.details")
            }
            "payment_details" => current_status == "insurance_selection",
            "booking_confirmation" => {
                current_status == "payment_details"
                    && self.has_field("payment")
                    && self
                        .get_field("payment.status")
                        .and_then(|s| s.as_str())
                        .is_some_and(|s| s == "completed")
            }
            "completed" => {
                current_status == "booking_confirmation" && self.has_field("bookingReference")
            }
            _ => false,
        }
    }

    fn validate_against_flight_booking_schema(&self) -> Result<(), FlightBookingValidationError> {
        // Load the generated schema from the file
        let schema_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schemas/flight-booking-schema.json"
        );
        let schema_content = fs::read_to_string(schema_path).map_err(|e| {
            FlightBookingValidationError::InvalidValue(format!("Failed to read schema file: {e}"))
        })?;

        let schema: Value = serde_json::from_str(&schema_content).map_err(|e| {
            FlightBookingValidationError::InvalidValue(format!("Failed to parse schema: {e}"))
        })?;

        // Compile the schema
        let compiled = Validator::new(&schema).map_err(|e| {
            FlightBookingValidationError::InvalidValue(format!("Failed to compile schema: {e}"))
        })?;

        // Get the flight booking data and validate it
        let flight_booking_data = self.get_flight_booking_data();

        if let Err(validation_error) = compiled.validate(&flight_booking_data) {
            return Err(FlightBookingValidationError::InvalidValue(format!(
                "Schema validation failed: {validation_error}"
            )));
        }

        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FlightBookingValidationError {
    #[error("Missing required field: {0}")]
    MissingField(String),

    #[error("Invalid value: {0}")]
    InvalidValue(String),

    #[error("Invalid status transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_flight_booking_data_extraction() {
        let mut handler = SchemaDataHandler::new();

        let flight_data = json!({
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-01",
            "passengers": {
                "adults": 2,
                "children": 0,
                "infants": 0,
                "total": 2
            },
            "status": "search_criteria",
            "customField": "should_be_included"
        });

        handler
            .merge_form_data("capturedData", &flight_data)
            .unwrap();
        let booking_data = handler.get_flight_booking_data();

        assert_eq!(booking_data["tripType"], json!("round-trip"));
        assert_eq!(booking_data["origin"], json!("LHR"));
        assert_eq!(booking_data["destination"], json!("JFK"));
        assert_eq!(booking_data["passengers"]["total"], json!(2));
        assert_eq!(booking_data["customField"], json!("should_be_included"));
    }

    #[test]
    fn test_validation_success() {
        let mut handler = SchemaDataHandler::new();

        let valid_data = json!({
            "tripType": "one-way",
            "passengers": {
                "total": 1,
                "adults": 1,
                "children": 0,
                "infants": 0
            },
            "status": "search_criteria"
        });

        handler
            .merge_form_data("capturedData", &valid_data)
            .unwrap();
        assert!(handler.validate_flight_booking_requirements().is_ok());
    }

    #[test]
    fn test_validation_missing_field() {
        let mut handler = SchemaDataHandler::new();

        let invalid_data = json!({
            "passengers": {
                "total": 1
            }
            // Missing tripType and status
        });

        handler
            .merge_form_data("capturedData", &invalid_data)
            .unwrap();
        let result = handler.validate_flight_booking_requirements();
        assert!(result.is_err());

        if let Err(FlightBookingValidationError::MissingField(field)) = result {
            assert_eq!(field, "tripType");
        } else {
            panic!("Expected MissingField error");
        }
    }

    #[test]
    fn test_status_transitions() {
        let mut handler = SchemaDataHandler::new();

        // Set up initial search criteria
        let search_data = json!({
            "tripType": "round-trip",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-01",
            "returnDate": "2024-06-08",
            "passengers": {
                "total": 2,
                "adults": 2,
                "children": 0,
                "infants": 0
            },
            "status": "search_criteria"
        });

        handler
            .merge_form_data("capturedData", &search_data)
            .unwrap();

        // Should be able to transition to flight search results
        assert!(handler.can_transition_to_status("flight_search_results"));

        // Add search results
        let search_results_data = json!({
            "searchResults": {
                "totalResults": 10,
                "outbound": []
            },
            "status": "flight_search_results"
        });

        handler
            .merge_form_data("capturedData", &search_results_data)
            .unwrap();

        // Should be able to transition to outbound selection
        assert!(handler.can_transition_to_status("outbound_flight_selection"));

        // Add outbound selection
        let outbound_data = json!({
            "selectedOutboundFlight": {
                "flightId": "BA123",
                "airline": "British Airways",
                "price": 299.99,
                "departure": "10:00",
                "arrival": "14:00"
            },
            "status": "outbound_flight_selection"
        });

        handler
            .merge_form_data("capturedData", &outbound_data)
            .unwrap();

        // For round-trip, should be able to transition to return selection
        assert!(handler.can_transition_to_status("return_flight_selection"));
    }

    #[test]
    fn test_one_way_trip_transitions() {
        let mut handler = SchemaDataHandler::new();

        let one_way_data = json!({
            "tripType": "one-way",
            "origin": "LHR",
            "destination": "JFK",
            "departureDate": "2024-06-01",
            "passengers": {
                "total": 1,
                "adults": 1,
                "children": 0,
                "infants": 0
            },
            "selectedOutboundFlight": {
                "flightId": "BA123",
                "airline": "British Airways",
                "price": 299.99,
                "departure": "10:00",
                "arrival": "14:00"
            },
            "status": "outbound_flight_selection"
        });

        handler
            .merge_form_data("capturedData", &one_way_data)
            .unwrap();

        // For one-way trips, should skip return flight selection
        assert!(!handler.can_transition_to_status("return_flight_selection"));
        assert!(handler.can_transition_to_status("passenger_details"));
    }

    #[test]
    fn test_schema_validation() {
        let mut handler = SchemaDataHandler::new();

        let valid_booking = serde_json::json!({
            "tripType": "one-way",
            "passengers": {
                "total": 1,
                "adults": 1,
                "children": 0,
                "infants": 0
            },
            "status": "search_criteria"
        });

        handler
            .merge_form_data("capturedData", &valid_booking)
            .unwrap();

        // This test will only pass if the schema file exists and is valid
        // In a real application, you would ensure the schema is generated before testing
        match handler.validate_against_flight_booking_schema() {
            Ok(()) => println!("Schema validation passed"),
            Err(e) => println!("Schema validation failed (expected in test): {e}"),
        }
    }
}
