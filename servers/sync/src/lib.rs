pub mod storage_config;
pub mod transport;

use axum::Router;
use server_account::{ClientAddressSource, PostgresAccountService};
use server_asset_store::{AssetError, FsAssetStore};
use server_postgres::{AssetApplication, PostgresDatabase, PostgresSyncRepository};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const READINESS_CACHE_TTL: Duration = Duration::from_secs(5);

pub struct AppConfig {
    pub books: PathBuf,
    pub thumbnails: PathBuf,
    pub client_address_source: ClientAddressSource,
    pub transfer_limits: server_http::TransferLimits,
    pub request_limits: server_http::RequestLimits,
    pub upload_limits: server_asset_store::UploadLimits,
    pub notification_config: server_http::NotificationConfig,
    pub operational_services: server_http::OperationalServices,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            books: PathBuf::from("./books"),
            thumbnails: PathBuf::from("./thumbnails"),
            client_address_source: ClientAddressSource::DirectPeer,
            transfer_limits: server_http::TransferLimits::default(),
            request_limits: server_http::RequestLimits::default(),
            upload_limits: server_asset_store::UploadLimits::default(),
            notification_config: server_http::NotificationConfig::default(),
            operational_services: server_http::OperationalServices::default(),
        }
    }
}

pub async fn app(database: PostgresDatabase, account: Arc<PostgresAccountService>, config: AppConfig) -> Result<Router, AssetError> {
    let AppConfig { books, thumbnails, client_address_source, transfer_limits, request_limits, upload_limits, notification_config, operational_services } = config;

    let assets = Arc::new(FsAssetStore::with_upload_limits(books, thumbnails, upload_limits));
    assets.initialize().await?;

    let sync = Arc::new(PostgresSyncRepository::new(database.clone()));
    let asset_service = Arc::new(AssetApplication::new(database.clone(), assets.clone()));
    let readiness = Arc::new(server_http::Readiness::new(database.clone(), assets.clone(), READINESS_CACHE_TTL));
    let account_routes = server_account::http::router(account.clone(), client_address_source);
    Ok(server_http::router_with_operational_limits(database, account_routes, account, sync, asset_service, readiness, transfer_limits, request_limits, notification_config, operational_services, client_address_source))
}
