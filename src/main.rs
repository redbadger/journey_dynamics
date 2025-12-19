use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::{Path, State},
    routing::{get, post},
};
use journey_dynamics::{
    command_extractor::CommandExtractor,
    domain::commands::JourneyCommand,
    route_handler::{command_handler, query_handler},
    state::new_application_state,
};
use uuid::Uuid;

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let state = Arc::new(new_application_state().await);
    // Configure the Axum routes and services.
    // For this example a single logical endpoint is used and the HTTP method
    // distinguishes whether the call is a command or a query.
    let router = Router::new()
        .route(
            "/journeys",
            post({
                let state = Arc::clone(&state);
                move || {
                    let id = Uuid::new_v4();
                    command_handler(
                        Path(id),
                        State(state),
                        CommandExtractor(HashMap::default(), JourneyCommand::Start { id }),
                    )
                }
            }),
        )
        .route(
            "/journeys/{journey_id}",
            get(query_handler).post(command_handler),
        )
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3030));

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Listening on {listener:?}");
    axum::serve(listener, router).await.unwrap();
}
