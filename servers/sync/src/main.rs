mod resend;

use server_account::{AccountTokenKey, PostgresAccountService};
use std::io;
use std::path::Path;
use std::sync::Arc;
use sync_server::transport::ServerConfig;
use tracing_subscriber::{fmt, EnvFilter};

const TEST_USER_QUOTA_BYTES: i64 = 1_000 * 1_000 * 1_000 * 1_000;

fn secret_from_environment_or_systemd_credential(environment_name: &str, credential_name: &str) -> Result<Option<String>, io::Error> {
    let environment = std::env::var(environment_name).ok().filter(|value| !value.is_empty());
    let credential = if let Some(directory) = std::env::var_os("CREDENTIALS_DIRECTORY") {
        match std::fs::read_to_string(Path::new(&directory).join(credential_name)) {
            Ok(value) => Some(value.trim_end_matches(['\r', '\n']).to_owned()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    match (environment, credential.filter(|value| !value.is_empty())) {
        (Some(_), Some(_)) => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("configure only one of {environment_name} or the {credential_name} systemd credential"))),
        (environment, credential) => Ok(environment.or(credential)),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let mut arguments = std::env::args_os().skip(1);
    match arguments.next() {
        None => run_server().await,
        Some(command) if command == "migrate" && arguments.next().is_none() => run_migrate().await,
        Some(command) if command == "verify-schema" && arguments.next().is_none() => run_verify_schema().await,
        Some(command) => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unknown sync-server command: {}; expected no command, 'migrate', or 'verify-schema'", command.to_string_lossy())).into()),
    }
}

/// Applies the initial schema to an empty database. The server
/// never applies schema implicitly on startup.
async fn run_migrate() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = secret_from_environment_or_systemd_credential("DATABASE_URL", "database_url")?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DATABASE_URL or the database_url systemd credential is required"))?;
    let database = server_postgres::connect(&database_url).await?;
    server_postgres::migrate(&database).await?;
    Ok(())
}

async fn run_verify_schema() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = secret_from_environment_or_systemd_credential("DATABASE_URL", "database_url")?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DATABASE_URL or the database_url systemd credential is required"))?;
    let database = server_postgres::connect(&database_url).await?;
    server_postgres::verify_schema(&database).await?;
    Ok(())
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let storage = sync_server::storage_config::StoragePaths::from_env()?;
    let database_url = secret_from_environment_or_systemd_credential("DATABASE_URL", "database_url")?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DATABASE_URL or the database_url systemd credential is required"))?;
    let database_pool = server_postgres::DatabasePoolConfig::from_env().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let pool = server_postgres::connect_configured(&database_url, database_pool).await?;
    server_postgres::verify_schema(&pool).await?;

    let account_token_key = secret_from_environment_or_systemd_credential("BOKHEIM_ACCOUNT_TOKEN_KEY", "account_token_key")?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_ACCOUNT_TOKEN_KEY or the account_token_key systemd credential is required"))?;
    let account_token_key = AccountTokenKey::from_base64(&account_token_key).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let auth = Arc::new(PostgresAccountService::new(pool.clone(), account_token_key));
    let test_user_email = std::env::var("BOKHEIM_TEST_USER_EMAIL").ok().filter(|value| !value.is_empty());
    let test_user_password = secret_from_environment_or_systemd_credential("BOKHEIM_TEST_USER_PASSWORD", "test_user_password")?;
    match (test_user_email, test_user_password) {
        (Some(email), Some(password)) => {
            let user = auth.ensure_configured_user(&email, &password).await.map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            server_postgres::configure_user_storage_quota(&pool, &user.user_id, TEST_USER_QUOTA_BYTES).await?;
            tracing::info!(user_id = %user.user_id, quota_bytes = TEST_USER_QUOTA_BYTES, "configured test account");
        }
        (None, None) => {}
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_TEST_USER_EMAIL and BOKHEIM_TEST_USER_PASSWORD/test_user_password credential must be configured together").into()),
    }
    let admin_email = std::env::var("BOKHEIM_ADMIN_EMAIL").ok().filter(|value| !value.is_empty());
    let admin_password = secret_from_environment_or_systemd_credential("BOKHEIM_ADMIN_PASSWORD", "admin_password")?;
    match (admin_email, admin_password) {
        (Some(email), Some(password)) => {
            let administrator = auth.ensure_configured_admin(&email, &password).await.map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            tracing::info!(user_id = %administrator.user_id, "configured administrator account");
        }
        (None, None) => {}
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "BOKHEIM_ADMIN_EMAIL and BOKHEIM_ADMIN_PASSWORD/admin_password credential must be configured together").into()),
    }
    let resend_api_key = secret_from_environment_or_systemd_credential("BOKHEIM_RESEND_API_KEY", "resend_api_key")?;
    if let Some(sender) = resend::ResendEmailSender::from_config(resend_api_key, std::env::var("BOKHEIM_EMAIL_FROM").ok())? {
        tokio::spawn(resend::run_email_worker(auth.clone(), sender));
    } else {
        tracing::warn!("email delivery is not configured; account emails will remain queued");
    }

    let server = ServerConfig::from_env()?;
    let transfer_limits = server_http::TransferLimits::from_env().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let request_limits = server_http::RequestLimits::from_env().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let notification_config = server_http::NotificationConfig::from_env().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let operational_services = server_http::OperationalServices::from_env().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let upload_limits = server_asset_store::UploadLimits::from_env().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let (books, thumbnails) = storage.into_parts();
    let app =
        sync_server::app(pool, auth, sync_server::AppConfig { books, thumbnails, client_address_source: server.client_address_source(), transfer_limits, request_limits, upload_limits, notification_config, operational_services }).await?;
    sync_server::transport::serve(server, app).await?;
    Ok(())
}
