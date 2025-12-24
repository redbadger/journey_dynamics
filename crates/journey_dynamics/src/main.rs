use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    routing::{get, post},
};
use journey_dynamics::{
    route_handler::{command_handler, query_handler},
    state::new_application_state,
};

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let state = Arc::new(new_application_state().await);
    // Configure the Axum routes and services.
    // For this example a single logical endpoint is used and the HTTP method
    // distinguishes whether the call is a command or a query.
    let router = Router::new()
        .route("/journeys", post(command_handler))
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
