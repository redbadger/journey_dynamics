use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Main schema with optional top-level groups
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlightBookingSchema {
    pub search: Option<SearchCriteria>,
    pub search_results: Option<SearchResults>,
    pub booking: Option<BookingData>,
}

// Search criteria group - when present, core fields are required
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchCriteria {
    pub trip_type: TripType,
    pub origin: AirportCode,
    pub destination: AirportCode,
    pub departure_date: String,      // ISO 8601 date format
    pub return_date: Option<String>, // ISO 8601 date format, required for round-trip
    pub passengers: PassengerCounts,
}

// Booking data group - contains all booking-related information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookingData {
    pub selected_outbound_flight: Option<FlightSelection>,
    pub selected_return_flight: Option<FlightSelection>,
    pub passenger_details: Option<Vec<PassengerDetail>>,
    pub pricing: Option<Pricing>,
    pub insurance: Option<Insurance>,
    pub payment: Option<Payment>,
    pub booking_reference: Option<String>,
    pub terms_accepted: Option<bool>,
    pub payment_status: Option<PaymentStatus>,
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
pub struct PassengerCounts {
    pub total: u32,
    pub adults: u32,
    pub children: u32,
    pub infants: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PassengerDetail {
    #[schemars(length(min = 1))]
    pub first_name: String,
    #[schemars(length(min = 1))]
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

#[cfg(test)]
mod tests;
