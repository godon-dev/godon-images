mod causal;
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
        .route("/systemtenders", get(handlers::list_systemtenders))
        .route("/systemtenders", post(handlers::create_systemtender))
        .route("/systemtenders/{id}", get(handlers::get_systemtender))
        .route("/systemtenders/{id}", put(handlers::update_systemtender))
        .route("/systemtenders/{id}", delete(handlers::delete_systemtender))
        .route("/systemtenders/{id}/stop", post(handlers::stop_systemtender))
        .route("/systemtenders/{id}/start", post(handlers::start_systemtender))
        .route("/credentials", get(handlers::list_credentials))
        .route("/credentials", post(handlers::create_credential))
        .route("/credentials/{id}", get(handlers::get_credential))
        .route("/credentials/{id}", delete(handlers::delete_credential))
        .route("/targets", get(handlers::list_targets))
        .route("/targets", post(handlers::create_target))
        .route("/targets/{id}", get(handlers::get_target))
        .route("/targets/{id}", delete(handlers::delete_target))
        .route("/steerwishes", get(handlers::list_steerwishes))
        .route("/steerwishes", post(handlers::declare_steerwish))
        .route("/steerwishes/{id}", get(handlers::get_steerwish))
        .route("/steerwishes/{id}/close", post(handlers::close_steerwish))
        .route("/steerwishes/{id}/update", post(handlers::update_steerwish))
        .route("/steerwishes/{id}", delete(handlers::delete_steerwish))
        .route("/connectome", get(handlers::get_connectome))
        .route("/connectome/artifact", get(handlers::get_connectome_artifact))
        .route("/connectome/curves", get(handlers::get_connectome_curves))
        .route("/connectome/predict", post(handlers::connectome_predict))
        .route("/connectome/predict/multihop", post(handlers::connectome_predict_multihop))
        .route("/connectome/impact/{systemtender_id}", get(handlers::connectome_impact))
        .route("/connectome/causes/{systemtender_id}", get(handlers::connectome_causes))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(cfg.clone());

    info!("Starting Godon API server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
