use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlightBooking {
    pub trip_type: TripType,
    pub origin: Option<AirportCode>,
    pub destination: Option<AirportCode>,
    pub departure_date: Option<String>, // ISO 8601 date format
    pub return_date: Option<String>,    // ISO 8601 date format
    pub passengers: Passengers,
    pub selected_outbound_flight: Option<FlightSelection>,
    pub selected_return_flight: Option<FlightSelection>,
    pub search_results: Option<SearchResults>,
    pub pricing: Option<Pricing>,
    pub insurance: Option<Insurance>,
    pub payment: Option<Payment>,
    pub booking_reference: Option<String>,
    pub status: BookingStatus,
    pub is_international: Option<bool>,
    pub requires_visa: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TripType {
    OneWay,
    RoundTrip,
    MultiCity,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AirportCode(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Passengers {
    pub total: u32,
    pub adults: u32,
    pub children: u32,
    pub infants: u32,
    pub details: Option<Vec<PassengerDetail>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PassengerDetail {
    pub first_name: String,
    pub last_name: String,
    pub date_of_birth: String, // ISO 8601 date format
    pub passport_number: Option<String>,
    pub nationality: Option<String>, // ISO 3166-1 alpha-2
    pub passenger_type: PassengerType,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PassengerType {
    Adult,
    Child,
    Infant,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlightSelection {
    pub flight_id: String,
    pub airline: String,
    pub flight_number: Option<String>,
    pub aircraft: Option<String>,
    pub price: f64,
    pub departure: String,        // HH:MM format
    pub arrival: String,          // HH:MM format
    pub duration: Option<String>, // ISO 8601 duration format
    pub stops: Option<u32>,
    pub cabin_class: Option<CabinClass>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CabinClass {
    Economy,
    PremiumEconomy,
    Business,
    First,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub outbound: Option<Vec<FlightOption>>,
    pub return_flights: Option<Vec<FlightOption>>,
    pub total_results: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlightOption {
    #[serde(flatten)]
    pub flight: FlightSelection,
    pub available: bool,
    pub seats_remaining: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    pub base_price: f64,
    pub taxes: f64,
    pub total_price: f64,
    pub currency: String, // ISO 4217 currency code
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Insurance {
    pub selected: bool,
    pub insurance_type: Option<InsuranceType>,
    pub price: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum InsuranceType {
    Basic,
    Comprehensive,
    Premium,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Payment {
    pub status: PaymentStatus,
    pub method: Option<PaymentMethod>,
    pub transaction_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PaymentStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Refunded,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaymentMethod {
    CreditCard,
    DebitCard,
    Paypal,
    BankTransfer,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BookingStatus {
    SearchCriteria,
    FlightSearchResults,
    OutboundFlightSelection,
    ReturnFlightSelection,
    PassengerDetails,
    InsuranceSelection,
    PaymentDetails,
    BookingConfirmation,
    Completed,
    Cancelled,
}

impl Default for FlightBooking {
    fn default() -> Self {
        Self {
            trip_type: TripType::OneWay,
            origin: None,
            destination: None,
            departure_date: None,
            return_date: None,
            passengers: Passengers {
                total: 1,
                adults: 1,
                children: 0,
                infants: 0,
                details: None,
            },
            selected_outbound_flight: None,
            selected_return_flight: None,
            search_results: None,
            pricing: None,
            insurance: None,
            payment: None,
            booking_reference: None,
            status: BookingStatus::SearchCriteria,
            is_international: None,
            requires_visa: None,
        }
    }
}

impl FlightBooking {
    /// Validate that passenger counts match the details provided
    pub fn validate_passenger_consistency(&self) -> Result<(), String> {
        if let Some(details) = &self.passengers.details {
            if details.len() as u32 != self.passengers.total {
                return Err(format!(
                    "Passenger count mismatch: total={} but {} detail records provided",
                    self.passengers.total,
                    details.len()
                ));
            }

            let mut adult_count = 0;
            let mut child_count = 0;
            let mut infant_count = 0;

            for detail in details {
                match detail.passenger_type {
                    PassengerType::Adult => adult_count += 1,
                    PassengerType::Child => child_count += 1,
                    PassengerType::Infant => infant_count += 1,
                }
            }

            if adult_count != self.passengers.adults
                || child_count != self.passengers.children
                || infant_count != self.passengers.infants
            {
                return Err(format!(
                    "Passenger type counts don't match: adults {}/{}, children {}/{}, infants {}/{}",
                    adult_count,
                    self.passengers.adults,
                    child_count,
                    self.passengers.children,
                    infant_count,
                    self.passengers.infants
                ));
            }
        }
        Ok(())
    }

    /// Check if required fields for search criteria are present
    pub fn has_required_search_fields(&self) -> bool {
        self.origin.is_some()
            && self.destination.is_some()
            && self.departure_date.is_some()
            && self.passengers.total > 0
    }

    /// Check if passenger details are complete for the number of passengers
    pub fn has_complete_passenger_details(&self) -> bool {
        if let Some(details) = &self.passengers.details {
            details.len() as u32 == self.passengers.total
                && details.iter().all(|p| {
                    !p.first_name.is_empty()
                        && !p.last_name.is_empty()
                        && !p.date_of_birth.is_empty()
                })
        } else {
            false
        }
    }

    /// Generate JSON schema for the flight booking structure
    pub fn schema() -> schemars::schema::RootSchema {
        schemars::schema_for!(FlightBooking)
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn test_default_flight_booking() {
        let booking = FlightBooking::default();
        assert_eq!(booking.passengers.total, 1);
        assert_eq!(booking.passengers.adults, 1);
        assert_eq!(booking.status, BookingStatus::SearchCriteria);
    }

    #[test]
    fn test_passenger_consistency_validation() {
        let mut booking = FlightBooking::default();
        booking.passengers = Passengers {
            total: 2,
            adults: 2,
            children: 0,
            infants: 0,
            details: Some(vec![
                PassengerDetail {
                    first_name: "John".to_string(),
                    last_name: "Doe".to_string(),
                    date_of_birth: "1985-03-15".to_string(),
                    passport_number: None,
                    nationality: None,
                    passenger_type: PassengerType::Adult,
                },
                PassengerDetail {
                    first_name: "Jane".to_string(),
                    last_name: "Doe".to_string(),
                    date_of_birth: "1987-07-22".to_string(),
                    passport_number: None,
                    nationality: None,
                    passenger_type: PassengerType::Adult,
                },
            ]),
        };

        assert!(booking.validate_passenger_consistency().is_ok());
    }

    #[test]
    fn test_passenger_count_mismatch() {
        let mut booking = FlightBooking::default();
        booking.passengers = Passengers {
            total: 2,
            adults: 2,
            children: 0,
            infants: 0,
            details: Some(vec![PassengerDetail {
                first_name: "John".to_string(),
                last_name: "Doe".to_string(),
                date_of_birth: "1985-03-15".to_string(),
                passport_number: None,
                nationality: None,
                passenger_type: PassengerType::Adult,
            }]),
        };

        assert!(booking.validate_passenger_consistency().is_err());
    }

    #[test]
    fn test_required_search_fields() {
        let mut booking = FlightBooking::default();
        assert!(!booking.has_required_search_fields());

        booking.origin = Some(AirportCode("LHR".to_string()));
        booking.destination = Some(AirportCode("JFK".to_string()));
        booking.departure_date = Some("2024-06-15".to_string());

        assert!(booking.has_required_search_fields());
    }

    #[test]
    fn test_schema_generation() {
        let schema = FlightBooking::schema();
        // Just verify that schema generation works and produces a non-empty schema
        assert!(!serde_json::to_string(&schema).unwrap().is_empty());
    }
}

#[cfg(test)]
mod integration_tests;
