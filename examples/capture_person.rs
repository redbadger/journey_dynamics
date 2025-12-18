//! Example demonstrating how to capture person data in a journey
//!
//! This example shows:
//! - Starting a journey
//! - Capturing person information (name, email, phone) using the `CapturePerson` command
//! - How person data is projected to the structured database table
//!
//! Run with: `cargo run --example capture_person`

use cqrs_es::{CqrsFramework, EventStore, mem_store::MemStore};
use journey_dynamics::SimpleLoggingQuery;
use journey_dynamics::domain::commands::JourneyCommand;
use journey_dynamics::domain::journey::{Journey, JourneyServices};
use journey_dynamics::services::decision_engine::SimpleDecisionEngine;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Journey Person Capture Example ===\n");

    // Setup event store and decision engine
    let event_store = MemStore::<Journey>::default();
    let query = SimpleLoggingQuery {};
    let decision_engine = Arc::new(SimpleDecisionEngine);
    let services = JourneyServices::new(decision_engine);
    let cqrs = CqrsFramework::new(event_store.clone(), vec![Box::new(query)], services);

    // Create a new journey
    let journey_id = Uuid::new_v4();
    println!("Starting journey: {journey_id}");

    // Start the journey
    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::Start { id: journey_id },
    )
    .await?;

    println!("Journey started successfully\n");

    // Capture person data with all fields
    println!("Capturing person data with phone number...");
    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::CapturePerson {
            name: "Alice Johnson".to_string(),
            email: "alice.johnson@example.com".to_string(),
            phone: Some("+1-555-0123".to_string()),
        },
    )
    .await?;

    println!("Person data captured: Alice Johnson (alice.johnson@example.com, +1-555-0123)\n");

    // Update person data (optional phone)
    println!("Updating person data without phone number...");
    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::CapturePerson {
            name: "Bob Smith".to_string(),
            email: "bob.smith@example.com".to_string(),
            phone: None,
        },
    )
    .await?;

    println!("Person data updated: Bob Smith (bob.smith@example.com, no phone)\n");

    // Create another journey with different person
    let journey_id_2 = Uuid::new_v4();
    println!("Starting second journey: {journey_id_2}");

    cqrs.execute(
        &journey_id_2.to_string(),
        JourneyCommand::Start { id: journey_id_2 },
    )
    .await?;

    cqrs.execute(
        &journey_id_2.to_string(),
        JourneyCommand::CapturePerson {
            name: "Carol Williams".to_string(),
            email: "carol.williams@example.com".to_string(),
            phone: Some("+1-555-9876".to_string()),
        },
    )
    .await?;

    println!("Second journey person captured: Carol Williams\n");

    // Complete first journey
    cqrs.execute(&journey_id.to_string(), JourneyCommand::Complete)
        .await?;

    println!("First journey completed\n");

    // Load and display events for first journey
    let events = event_store.load_events(&journey_id.to_string()).await?;
    println!("=== Journey {journey_id} Event History ===");
    for (i, event) in events.iter().enumerate() {
        println!("Event {}: {:?}", i + 1, event.payload);
    }

    println!("\n=== Example Complete ===");
    println!("\nNote: In a real application with a database:");
    println!("- Person data would be stored in the 'journey_person' table");
    println!("- You could query journeys by email using find_by_email()");
    println!("- The person table would have indexes on journey_id and email");
    println!("- Data is structured with proper types (not JSON blobs)");

    Ok(())
}
