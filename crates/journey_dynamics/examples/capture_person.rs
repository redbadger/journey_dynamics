//! Example demonstrating how to register and bind a subject in a journey.
//!
//! This example shows:
//! - Starting a journey
//! - Registering a data subject and binding them to a role path using
//!   `RegisterAndBindSubject`
//! - Setting person attributes with `SetAttributes`
//!
//! Run with: `cargo run -p journey_dynamics --example capture_person`

use std::collections::BTreeMap;
use std::sync::Arc;

use cqrs_es::{CqrsFramework, EventStore, mem_store::MemStore};
use journey_dynamics::{
    SimpleLoggingQuery,
    domain::{
        AttributeSchema,
        commands::JourneyCommand,
        journey::{Journey, JourneyServices},
    },
    services::{decision_engine::SimpleDecisionEngine, schema_validator::NoOpValidator},
};
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Journey Subject Registration Example ===\n");

    // Setup event store and decision engine
    let event_store = MemStore::<Journey>::default();
    let query = SimpleLoggingQuery {};
    let decision_engine = Arc::new(SimpleDecisionEngine);
    let schema_validator = Arc::new(NoOpValidator);
    let services = JourneyServices::new(
        decision_engine,
        schema_validator,
        Arc::new(AttributeSchema::permissive()),
    );
    let cqrs = CqrsFramework::new(event_store.clone(), vec![Box::new(query)], services);

    // Create a new journey
    let journey_id = Uuid::new_v4();
    println!("Starting journey: {journey_id}");

    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::Start { id: journey_id },
    )
    .await?;

    println!("Journey started successfully\n");

    // Register the lead booker and bind them to the persons/lead_booker role.
    let lead_booker_id = Uuid::new_v4();
    println!("Registering lead booker...");
    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::RegisterAndBindSubject {
            role_path: "/persons/lead_booker".parse()?,
            subject_id: lead_booker_id,
            email: "alice.johnson@example.com".to_string(),
        },
    )
    .await?;

    println!("Lead booker registered: alice.johnson@example.com\n");

    // Set the lead booker's attributes via SetAttributes.
    let mut changes = BTreeMap::new();
    changes.insert("/persons/lead_booker/firstName".parse()?, json!("Alice"));
    changes.insert("/persons/lead_booker/lastName".parse()?, json!("Johnson"));
    changes.insert("/persons/lead_booker/phone".parse()?, json!("+1-555-0123"));

    println!("Setting lead booker attributes...");
    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::SetAttributes { changes },
    )
    .await?;

    println!("Attributes set for Alice Johnson\n");

    // Register a second passenger on the same journey.
    let passenger_id = Uuid::new_v4();
    println!("Registering passenger_0...");
    cqrs.execute(
        &journey_id.to_string(),
        JourneyCommand::RegisterAndBindSubject {
            role_path: "/persons/passenger_0".parse()?,
            subject_id: passenger_id,
            email: "bob.smith@example.com".to_string(),
        },
    )
    .await?;

    println!("passenger_0 registered: bob.smith@example.com\n");

    // Complete the journey.
    cqrs.execute(&journey_id.to_string(), JourneyCommand::Complete)
        .await?;

    println!("Journey completed\n");

    // Display the event history.
    let events = event_store.load_events(&journey_id.to_string()).await?;
    println!("=== Journey {journey_id} Event History ===");
    for (i, event) in events.iter().enumerate() {
        println!("Event {}: {:?}", i + 1, event.payload);
    }

    println!("\n=== Example Complete ===");

    Ok(())
}
