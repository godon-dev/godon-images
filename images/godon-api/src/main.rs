mod config;
mod handlers;
mod types;
mod windmill_adapter;

use axum::{
    routing::{delete, get, post, put},
    Router,
};
use std::net::SocketAddr;
use tower_http::cors::{Any, CorsLayer};
use log::info;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .init();

    let cfg = config::Config::from_env();
    let addr: SocketAddr = format!("0.0.0.0:{}", cfg.port).parse().unwrap();

    let app = Router::new()
        .route("/", get(handlers::root))
        .route("/health", get(handlers::health))
        .route("/breeders", get(handlers::list_breeders))
        .route("/breeders", post(handlers::create_breeder))
        .route("/breeders/{id}", get(handlers::get_breeder))
        .route("/breeders/{id}", put(handlers::update_breeder))
        .route("/breeders/{id}", delete(handlers::delete_breeder))
        .route("/breeders/{id}/stop", post(handlers::stop_breeder))
        .route("/breeders/{id}/start", post(handlers::start_breeder))
        .route("/credentials", get(handlers::list_credentials))
        .route("/credentials", post(handlers::create_credential))
        .route("/credentials/{id}", get(handlers::get_credential))
        .route("/credentials/{id}", delete(handlers::delete_credential))
        .route("/targets", get(handlers::list_targets))
        .route("/targets", post(handlers::create_target))
        .route("/targets/{id}", get(handlers::get_target))
        .route("/targets/{id}", delete(handlers::delete_target))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(cfg.clone());

    info!("Starting Godon API server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
