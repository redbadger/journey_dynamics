use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    routing::{delete, get, post},
};
use journey_dynamics::{
    route_handler::{command_handler, query_handler, shred_subject, shred_subjects_by_email},
    state::new_application_state,
};

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let state = Arc::new(new_application_state().await);
    let router = Router::new()
        .route("/journeys", post(command_handler))
        .route(
            "/journeys/{journey_id}",
            get(query_handler).post(command_handler),
        )
        .route("/subjects/by-email", delete(shred_subjects_by_email))
        .route("/subjects/{subject_id}", delete(shred_subject))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3030));

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("Listening on {listener:?}");
    axum::serve(listener, router).await.unwrap();
}
