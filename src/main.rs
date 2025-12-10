use std::net::SocketAddr;

use axum::{Router, routing::get};
use journey_dynamics::{
    route_handler::{command_handler, query_handler},
    state::new_application_state,
};

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let state = new_application_state().await;
    // Configure the Axum routes and services.
    // For this example a single logical endpoint is used and the HTTP method
    // distinguishes whether the call is a command or a query.
    let router = Router::new()
        .route(
            "/journey/{journey_id}",
            get(query_handler).post(command_handler),
        )
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3030));

    axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), router)
        .await
        .unwrap();
}
