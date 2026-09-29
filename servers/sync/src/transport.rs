use axum::Router;
use server_account::ClientAddressSource;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

const DEFAULT_ADDRESS: &str = "127.0.0.1:8080";
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const REMOVED_TLS_SETTINGS: &[&str] = &["SYNC_TLS_MODE", "SYNC_TLS_CERT_PATH", "SYNC_TLS_KEY_PATH", "SYNC_ACME_DOMAINS", "SYNC_ACME_EMAIL", "SYNC_ACME_CACHE_DIR", "SYNC_ACME_ENVIRONMENT"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerConfig {
    transport: Transport,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Transport {
    Tcp(SocketAddr),
    #[cfg(unix)]
    Unix(PathBuf),
}

#[derive(Debug, Eq, PartialEq)]
pub struct ConfigError(String);

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ConfigError {}

impl ServerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_values(|name| std::env::var(name).ok())
    }

    pub fn client_address_source(&self) -> ClientAddressSource {
        match self.transport {
            Transport::Tcp(_) => ClientAddressSource::DirectPeer,
            #[cfg(unix)]
            Transport::Unix(_) => ClientAddressSource::TrustedForwardedFor,
        }
    }

    fn from_values(mut value: impl FnMut(&str) -> Option<String>) -> Result<Self, ConfigError> {
        reject_removed_tls_settings(&mut value)?;
        let socket = value("SYNC_SERVER_SOCKET");
        let address = value("SYNC_SERVER_ADDR");
        if socket.is_some() && address.is_some() {
            return Err(ConfigError::new("SYNC_SERVER_SOCKET and SYNC_SERVER_ADDR are mutually exclusive"));
        }

        if let Some(socket) = socket {
            if value("SYNC_ALLOW_INSECURE_HTTP").is_some() {
                return Err(ConfigError::new("SYNC_ALLOW_INSECURE_HTTP is not valid with SYNC_SERVER_SOCKET"));
            }
            let socket = PathBuf::from(socket);
            if !socket.is_absolute() {
                return Err(ConfigError::new("SYNC_SERVER_SOCKET must be an absolute path"));
            }
            #[cfg(unix)]
            return Ok(Self { transport: Transport::Unix(socket) });
            #[cfg(not(unix))]
            return Err(ConfigError::new("SYNC_SERVER_SOCKET is only supported on Unix platforms"));
        }

        let address = address.unwrap_or_else(|| DEFAULT_ADDRESS.to_owned()).parse::<SocketAddr>().map_err(|error| ConfigError::new(format!("invalid SYNC_SERVER_ADDR: {error}")))?;
        let allow_insecure = parse_bool(value("SYNC_ALLOW_INSECURE_HTTP"), "SYNC_ALLOW_INSECURE_HTTP")?.unwrap_or(false);
        if !address.ip().is_loopback() && !allow_insecure {
            return Err(ConfigError::new("plain TCP HTTP may only bind to loopback; use SYNC_SERVER_SOCKET behind Caddy or explicitly set SYNC_ALLOW_INSECURE_HTTP=true"));
        }
        Ok(Self { transport: Transport::Tcp(address) })
    }
}

pub async fn serve(config: ServerConfig, app: Router) -> Result<(), Box<dyn Error>> {
    match config.transport {
        Transport::Tcp(address) => {
            tracing::warn!(%address, "serving plain HTTP on TCP for local development or an explicitly accepted proxy deployment");
            let listener = tokio::net::TcpListener::bind(address).await?;
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown_signal()).await?;
        }
        #[cfg(unix)]
        Transport::Unix(path) => {
            let listener = bind_unix_socket(&path)?;
            tracing::info!(socket = %path.display(), "serving HTTP through a trusted Unix-socket proxy boundary");
            let handle = axum_server::Handle::new();
            let shutdown_handle = handle.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                tracing::info!(timeout_seconds = GRACEFUL_SHUTDOWN_TIMEOUT.as_secs(), "stopping new connections and draining active requests");
                shutdown_handle.graceful_shutdown(Some(GRACEFUL_SHUTDOWN_TIMEOUT));
            });
            axum_server::from_unix(listener)?.handle(handle).serve(app.into_make_service()).await?;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl-C shutdown handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM shutdown handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(unix)]
fn bind_unix_socket(path: &Path) -> Result<std::os::unix::net::UnixListener, std::io::Error> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Unix socket must have a parent directory"))?;
    if !parent.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("Unix socket parent directory does not exist: {}", parent.display())));
    }

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("refusing to replace non-socket path: {}", path.display())));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let listener = std::os::unix::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn reject_removed_tls_settings(value: &mut impl FnMut(&str) -> Option<String>) -> Result<(), ConfigError> {
    if let Some(name) = REMOVED_TLS_SETTINGS.iter().find(|name| value(name).is_some()) {
        return Err(ConfigError::new(format!("{name} is no longer accepted; terminate public HTTPS in Caddy and configure SYNC_SERVER_SOCKET")));
    }
    Ok(())
}

fn parse_bool(value: Option<String>, name: &str) -> Result<Option<bool>, ConfigError> {
    value
        .map(|value| match value.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            other => Err(ConfigError::new(format!("invalid {name} {other:?}; expected true or false"))),
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use std::collections::HashMap;
    #[cfg(unix)]
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn config(values: &[(&str, &str)]) -> Result<ServerConfig, ConfigError> {
        let values: HashMap<_, _> = values.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect();
        ServerConfig::from_values(|name| values.get(name).cloned())
    }

    #[test]
    fn defaults_to_loopback_http_and_direct_peer_addresses() {
        let config = config(&[]).unwrap();
        assert_eq!(config, ServerConfig { transport: Transport::Tcp("127.0.0.1:8080".parse().unwrap()) });
        assert_eq!(config.client_address_source(), ClientAddressSource::DirectPeer);
    }

    #[test]
    fn refuses_public_plain_http_by_default() {
        let error = config(&[("SYNC_SERVER_ADDR", "0.0.0.0:8080")]).unwrap_err();
        assert!(error.to_string().contains("plain TCP HTTP may only bind to loopback"));
    }

    #[test]
    fn public_plain_http_requires_an_explicit_escape_hatch() {
        assert!(config(&[("SYNC_SERVER_ADDR", "0.0.0.0:8080"), ("SYNC_ALLOW_INSECURE_HTTP", "true")]).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_uses_the_trusted_proxy_address_source() {
        let config = config(&[("SYNC_SERVER_SOCKET", "/run/bokheim/sync.sock")]).unwrap();
        assert_eq!(config.client_address_source(), ClientAddressSource::TrustedForwardedFor);
    }

    #[test]
    fn socket_and_tcp_address_are_mutually_exclusive() {
        let error = config(&[("SYNC_SERVER_SOCKET", "/run/bokheim/sync.sock"), ("SYNC_SERVER_ADDR", "127.0.0.1:8080")]).unwrap_err();
        assert!(error.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn socket_path_must_be_absolute() {
        let error = config(&[("SYNC_SERVER_SOCKET", "sync.sock")]).unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }

    #[test]
    fn removed_embedded_tls_settings_fail_loudly() {
        let error = config(&[("SYNC_TLS_MODE", "acme")]).unwrap_err();
        assert!(error.to_string().contains("terminate public HTTPS in Caddy"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_is_group_accessible_but_not_world_accessible() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("sync.sock");
        let listener = bind_unix_socket(&socket).unwrap();
        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);
        drop(listener);
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_replaces_only_a_stale_socket() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("sync.sock");
        drop(bind_unix_socket(&socket).unwrap());
        assert!(bind_unix_socket(&socket).is_ok());

        let ordinary_file = directory.path().join("do-not-delete");
        std::fs::write(&ordinary_file, b"important").unwrap();
        let error = bind_unix_socket(&ordinary_file).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(ordinary_file).unwrap(), b"important");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_transport_serves_http_without_a_tcp_listener() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("sync.sock");
        let config = ServerConfig { transport: Transport::Unix(socket.clone()) };
        let app = Router::new().route("/api/health", get(|| async { "ok" }));
        let server = tokio::spawn(async move { serve(config, app).await.map_err(|error| error.to_string()) });

        let mut stream = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match tokio::net::UnixStream::connect(&socket).await {
                    Ok(stream) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound || error.kind() == std::io::ErrorKind::ConnectionRefused => tokio::task::yield_now().await,
                    Err(error) => panic!("failed to connect to test socket: {error}"),
                }
            }
        })
        .await
        .expect("Unix listener did not start");
        stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"), "unexpected response: {response}");
        assert!(response.ends_with("\r\n\r\nok"), "unexpected response: {response}");

        server.abort();
    }
}
