use std::fs;
use std::path::Path;

use flight_booking::FlightBookingSchema;
use schemars::schema_for;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = schema_for!(FlightBookingSchema);

    // Ensure the schemas directory exists
    let schemas_dir = Path::new("./schemas");
    if !schemas_dir.exists() {
        fs::create_dir_all(schemas_dir)?;
    }

    // Write the schema to file
    let schema_path = schemas_dir.join("flight-booking-schema.json");
    fs::write(&schema_path, serde_json::to_string_pretty(&schema)?)?;

    println!(
        "Generated flight booking schema at: {}",
        schema_path.display()
    );

    Ok(())
}
