use flight_booking::FlightBooking;
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Generate the JSON schema from the Rust types
    let schema = FlightBooking::schema();

    // Convert to pretty JSON
    let schema_json = serde_json::to_string_pretty(&schema)?;

    // Ensure the schemas directory exists
    let schemas_dir = Path::new("../../schemas");
    if !schemas_dir.exists() {
        fs::create_dir_all(schemas_dir)?;
    }

    // Write the schema to file
    let schema_path = schemas_dir.join("flight-booking.json");
    fs::write(&schema_path, schema_json)?;

    println!(
        "Generated flight booking schema at: {}",
        schema_path.display()
    );

    // Also write it to the jdm-models directory for easy access
    let jdm_schemas_dir = Path::new("jdm-models/schemas");
    if !jdm_schemas_dir.exists() {
        fs::create_dir_all(jdm_schemas_dir)?;
    }

    let jdm_schema_path = jdm_schemas_dir.join("flight-booking.json");
    let schema_json_copy = serde_json::to_string_pretty(&schema)?;
    fs::write(&jdm_schema_path, schema_json_copy)?;

    println!("Also copied schema to: {}", jdm_schema_path.display());

    Ok(())
}
