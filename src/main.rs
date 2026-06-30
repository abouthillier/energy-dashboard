mod api;
mod config;
mod db;
mod import;
mod models;
mod parsers;
mod weather;

use std::net::SocketAddr;

use anyhow::Context;
use axum::Router;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::{api::AppState, config::Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "energy_service=info,tower_http=info,axum=info".into()
        }))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    let state = AppState { pool, config: config.clone() };
    let app = Router::new()
        .nest("/", api::router(state))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .context("invalid ENERGY_HOST/ENERGY_PORT bind address")?;

    tracing::info!("energy service listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
