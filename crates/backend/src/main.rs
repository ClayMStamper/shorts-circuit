//! Axum OAuth broker for confidential client credentials; later the cloud runner.
//!
//! Configuration (environment):
//! - `SHORTS_BROKER_BIND` — listen address, default `127.0.0.1:8787`. Put it behind HTTPS.
//! - `TIKTOK_CLIENT_KEY`, `TIKTOK_CLIENT_SECRET` — the TikTok developer app. Without them TikTok
//!   reports as not configured.
//! - `TIKTOK_REDIRECT_URI` — `<public broker URL>/v1/tiktok/callback`, registered with TikTok.

use std::net::SocketAddr;

use shorts_backend::{BrokerConfig, router};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = BrokerConfig::from_env()?;
    let bind: SocketAddr = std::env::var("SHORTS_BROKER_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    info!(%bind, tiktok = config.tiktok.is_some(), "OAuth broker starting");
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router(config, http))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
