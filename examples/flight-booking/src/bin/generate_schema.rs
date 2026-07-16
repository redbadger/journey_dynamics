use std::fs;
use std::path::Path;

use flight_booking::{attribute_schema_config, FlightBookingSchema};
use schemars::schema_for;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure the schemas directory exists
    let schemas_dir = Path::new("./schemas");
    if !schemas_dir.exists() {
        fs::create_dir_all(schemas_dir)?;
    }

    // Generate the flight-booking data schema from Rust types.
    let schema = schema_for!(FlightBookingSchema);
    let schema_path = schemas_dir.join("flight-booking-schema.json");
    fs::write(&schema_path, serde_json::to_string_pretty(&schema)?)?;
    println!(
        "Generated flight booking schema at: {}",
        schema_path.display()
    );

    // Generate the attribute (PII classification) schema. It is *derived* from
    // the same `FlightBookingSchema` via its `x-subject` markers — the single
    // source of truth. Edit the schema types in lib.rs, then re-run
    // `just generate` to keep both JSON files in sync.
    let attr_config = attribute_schema_config();
    let attr_path = schemas_dir.join("attribute-schema.json");
    fs::write(&attr_path, serde_json::to_string_pretty(&attr_config)?)?;
    println!("Generated attribute schema at: {}", attr_path.display());

    Ok(())
}
