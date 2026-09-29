use crate::bandwidth::{TransferGovernor, TransferLimits};
use crate::notifications::{NotificationConfig, NotificationHub};
use crate::operations::OperationalServices;
use crate::traffic::TrafficRecorder;
use crate::{Readiness, RequestLimits};
use server_account::PostgresAccountService;
use server_postgres::PostgresDatabase;
use server_postgres::{AssetApplication, PostgresSyncRepository};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

struct AccountThumbnailLimit {
    uploads: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

pub(crate) struct ThumbnailUploadPermit {
    _account_upload: OwnedSemaphorePermit,
    _account_bytes: OwnedSemaphorePermit,
    _global_upload: OwnedSemaphorePermit,
    _global_bytes: OwnedSemaphorePermit,
    _account: Arc<AccountThumbnailLimit>,
}

pub(crate) struct ThumbnailUploadGate {
    uploads: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    per_account: usize,
    bytes_per_account: usize,
    accounts: Mutex<HashMap<String, Weak<AccountThumbnailLimit>>>,
}

impl ThumbnailUploadGate {
    fn new(global: usize, per_account: usize, global_bytes: usize, bytes_per_account: usize) -> Self {
        Self { uploads: Arc::new(Semaphore::new(global)), bytes: Arc::new(Semaphore::new(global_bytes)), per_account, bytes_per_account, accounts: Mutex::new(HashMap::new()) }
    }

    pub(crate) fn acquire(&self, user_id: &str, declared_bytes: u64) -> Option<ThumbnailUploadPermit> {
        let byte_permits = u32::try_from(declared_bytes.max(1)).ok()?;
        let account = {
            let mut accounts = self.accounts.lock().ok()?;
            accounts.retain(|_, account| account.strong_count() > 0);
            match accounts.get(user_id).and_then(Weak::upgrade) {
                Some(account) => account,
                None => {
                    let account = Arc::new(AccountThumbnailLimit { uploads: Arc::new(Semaphore::new(self.per_account)), bytes: Arc::new(Semaphore::new(self.bytes_per_account)) });
                    accounts.insert(user_id.to_string(), Arc::downgrade(&account));
                    account
                }
            }
        };
        let account_upload = account.uploads.clone().try_acquire_owned().ok()?;
        let account_bytes = account.bytes.clone().try_acquire_many_owned(byte_permits).ok()?;
        let global_upload = self.uploads.clone().try_acquire_owned().ok()?;
        let global_bytes = self.bytes.clone().try_acquire_many_owned(byte_permits).ok()?;
        Some(ThumbnailUploadPermit { _account_upload: account_upload, _account_bytes: account_bytes, _global_upload: global_upload, _global_bytes: global_bytes, _account: account })
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) playback: Arc<crate::playback::Grants>,
    pub(crate) database: PostgresDatabase,
    pub(crate) account: Arc<PostgresAccountService>,
    pub(crate) sync: Arc<PostgresSyncRepository>,
    pub(crate) assets: Arc<AssetApplication>,
    pub(crate) readiness: Arc<Readiness>,
    pub(crate) transfer_governor: TransferGovernor,
    pub(crate) thumbnail_uploads: Arc<ThumbnailUploadGate>,
    pub(crate) notifications: NotificationHub,
    pub(crate) traffic: TrafficRecorder,
    pub(crate) operational_services: OperationalServices,
}

impl AppState {
    pub(super) fn new(
        database: PostgresDatabase, account: Arc<PostgresAccountService>, sync: Arc<PostgresSyncRepository>, assets: Arc<AssetApplication>, readiness: Arc<Readiness>, transfer_limits: TransferLimits, request_limits: RequestLimits,
        notification_config: NotificationConfig, operational_services: OperationalServices, traffic: TrafficRecorder,
    ) -> Self {
        Self {
            database,
            playback: Arc::new(crate::playback::Grants::default()),
            account,
            sync,
            assets,
            readiness,
            transfer_governor: TransferGovernor::new(transfer_limits),
            thumbnail_uploads: Arc::new(ThumbnailUploadGate::new(
                request_limits.max_thumbnail_uploads,
                request_limits.max_thumbnail_uploads_per_account,
                request_limits.max_in_flight_thumbnail_bytes,
                request_limits.max_in_flight_thumbnail_bytes_per_account,
            )),
            notifications: NotificationHub::new(notification_config),
            traffic,
            operational_services,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ThumbnailUploadGate;

    #[test]
    fn thumbnail_upload_capacity_is_per_account_and_global_and_releases_on_drop() {
        let gate = ThumbnailUploadGate::new(2, 1, 10, 6);
        let alice = gate.acquire("alice", 6).unwrap();
        assert!(gate.acquire("alice", 1).is_none());
        let bob = gate.acquire("bob", 4).unwrap();
        assert!(gate.acquire("carol", 1).is_none());
        drop(alice);
        let carol = gate.acquire("carol", 6).unwrap();
        drop((bob, carol));
        assert!(gate.acquire("alice", 6).is_some());
    }

    #[test]
    fn thumbnail_upload_capacity_is_weighted_by_declared_bytes() {
        let gate = ThumbnailUploadGate::new(10, 10, 10, 6);
        let alice = gate.acquire("alice", 4).unwrap();
        assert!(gate.acquire("alice", 3).is_none(), "per-account byte budget must apply even when count permits remain");
        let bob = gate.acquire("bob", 6).unwrap();
        assert!(gate.acquire("carol", 1).is_none(), "global byte budget must apply even when count permits remain");
        drop(alice);
        assert!(gate.acquire("carol", 4).is_some());
        drop(bob);
    }
}
