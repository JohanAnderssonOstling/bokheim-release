use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use taxonomy_server::{app, TaxonomyService};
use tracing_subscriber::{fmt, EnvFilter};

const DEFAULT_BIND: &str = "127.0.0.1:8093";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();
    let database = std::env::var_os("BOKHEIM_TAXONOMY_DATABASE").map(PathBuf::from).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_TAXONOMY_DATABASE is required"))?;
    let bind: SocketAddr = std::env::var("BOKHEIM_TAXONOMY_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned()).parse().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid BOKHEIM_TAXONOMY_BIND: {error}")))?;
    if !bind.ip().is_loopback() && std::env::var("BOKHEIM_TAXONOMY_ALLOW_PUBLIC_HTTP").as_deref() != Ok("true") {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "non-loopback plain HTTP requires BOKHEIM_TAXONOMY_ALLOW_PUBLIC_HTTP=true; use a TLS reverse proxy in production").into());
    }
    let service = TaxonomyService::open(database)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(address = %listener.local_addr()?, "public unauthenticated taxonomy server listening");
    axum::serve(listener, app(service)).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
