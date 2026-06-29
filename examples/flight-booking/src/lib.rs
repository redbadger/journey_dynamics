use std::collections::BTreeMap;

use journey_dynamics::domain::{attribute_schema::AttributeSchemaConfig, AttributeSchema};
use journey_dynamics::queries::JourneyView;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Main schema with optional top-level groups
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlightBookingSchema {
    pub search: Option<SearchCriteria>,
    pub search_results: Option<SearchResults>,
    pub booking: Option<BookingData>,
    /// Per-passenger details, keyed by passenger ref (e.g. `passenger_0`).
    /// Each entry is a crypto subject `/persons/{ref}`; the PII fields within
    /// (marked `x-subject`) are encrypted under that passenger's DEK.
    pub persons: Option<BTreeMap<String, PassengerDetail>>,
}

/// Reconstruct a [`FlightBookingSchema`] from the plaintext path-keyed bag
/// stored in `shared_data`. PII fields (`firstName`, `passportNumber`, etc.)
/// are encrypted separately and are not included.
///
/// # Errors
///
/// Returns a [`serde_json::Error`] if `shared_data` cannot be deserialised
/// into the schema types.
impl TryFrom<&JourneyView> for FlightBookingSchema {
    type Error = serde_json::Error;

    fn try_from(view: &JourneyView) -> Result<Self, Self::Error> {
        serde_json::from_value(view.shared_data.clone())
    }
}

/// The attribute-classification config for the flight-booking example,
/// **derived from [`FlightBookingSchema`]** via the `x-subject` markers — the
/// single source of truth. Yields:
/// - `/search`, `/searchResults`, `/booking` → plaintext prefixes (no PII);
/// - `/persons/{ref}` → a namespace where the `x-subject`-marked fields
///   (`firstName`, `lastName`, `dateOfBirth`, `passportNumber`, `nationality`)
///   are secret under `persons/{ref}` and `passengerType` is plaintext.
///
/// Serialise this to the JSON file loaded by `JOURNEY_ATTRIBUTE_SCHEMA_PATH`.
///
/// # Panics
/// Panics if `FlightBookingSchema` fails to serialise, or if a secret field's
/// `x-subject` annotation is malformed — both are build-time bugs in the schema
/// types above.
#[must_use]
pub fn attribute_schema_config() -> AttributeSchemaConfig {
    let schema = serde_json::to_value(schemars::schema_for!(FlightBookingSchema))
        .expect("schema serialises");
    AttributeSchemaConfig::from_annotated_schema(&schema)
}

/// The runtime [`AttributeSchema`] for the flight-booking example, built from
/// the schema-derived [`attribute_schema_config()`].
#[must_use]
pub fn attribute_schema() -> AttributeSchema {
    AttributeSchema::from(attribute_schema_config())
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

// Booking data group - contains all booking-related information.
//
// NOTE: per-passenger PII (names, dates of birth, passport numbers, nationality) is
// intentionally absent from this struct.  That data flows through `SetAttributes`
// under `persons/<ref>/<field>` and is encrypted at rest under each passenger's
// Data Encryption Key.  Only non-PII workflow signals belong here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BookingData {
    pub selected_outbound_flight: Option<FlightSelection>,
    pub selected_return_flight: Option<FlightSelection>,
    pub selected_seats: Option<SelectedSeats>,
    pub seat_upgrade_total: Option<f64>,
    pub pricing: Option<Pricing>,
    pub insurance: Option<Insurance>,
    pub payment: Option<Payment>,
    pub booking_reference: Option<String>,
    pub terms_accepted: Option<bool>,
    pub payment_status: Option<PaymentStatus>,
    pub is_international: Option<bool>,
    pub requires_visa: Option<bool>,
}

/// Seat assignments for outbound and (optionally) return legs.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SelectedSeats {
    pub outbound: Option<Vec<String>>,
    /// Renamed to avoid collision with the `return` keyword.
    #[serde(rename = "return")]
    pub return_seats: Option<Vec<String>>,
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
    pub adults: u32,
    pub children: u32,
    pub infants: u32,
}

/// Per-passenger details, captured progressively (all optional).
///
/// The PII fields carry `x-subject = "/persons/*"`, marking them secret under
/// the passenger's own subject (`/persons/{ref}`); `passengerType` is plaintext
/// (an operational signal the workflow reads directly).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PassengerDetail {
    #[schemars(extend("x-subject" = "/persons/*"), length(min = 1))]
    pub first_name: Option<String>,
    #[schemars(extend("x-subject" = "/persons/*"), length(min = 1))]
    pub last_name: Option<String>,
    #[schemars(extend("x-subject" = "/persons/*", "format" = "date"))]
    pub date_of_birth: Option<String>, // ISO 8601 date format
    #[schemars(extend("x-subject" = "/persons/*"))]
    pub passport_number: Option<String>,
    #[schemars(extend("x-subject" = "/persons/*"))]
    pub nationality: Option<String>, // ISO 3166-1 alpha-2
    pub passenger_type: Option<PassengerType>,
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
