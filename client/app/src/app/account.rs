use futures_util::{stream, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use crate::LibraryId;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AccountStatus {
    signed_in: bool,
    server_url: Option<String>,
    email: Option<String>,
}

impl AccountStatus {
    pub fn signed_out() -> Self {
        Self::default()
    }

    pub fn authenticated(server_url: impl Into<String>, email: impl Into<String>) -> Self {
        Self { signed_in: true, server_url: Some(server_url.into()), email: Some(email.into()) }
    }

    pub const fn signed_in(&self) -> bool {
        self.signed_in
    }

    pub fn server_url(&self) -> Option<&str> {
        self.server_url.as_deref()
    }

    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct AccountStorageUsage {
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub quota_bytes: u64,
}

impl AccountStorageUsage {
    pub const fn available_bytes(self) -> u64 {
        self.quota_bytes.saturating_sub(self.used_bytes.saturating_add(self.reserved_bytes))
    }
}

impl From<account_contract::StorageUsageResponse> for AccountStorageUsage {
    fn from(value: account_contract::StorageUsageResponse) -> Self {
        Self { used_bytes: value.used_bytes, reserved_bytes: value.reserved_bytes, quota_bytes: value.quota_bytes }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ServerLibraryStorageUsage {
    pub library_id: LibraryId,
    pub library_name: String,
    pub used_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct LocalLibraryStorageUsage {
    pub library_id: LibraryId,
    pub library_name: String,
    pub used_bytes: u64,
}

struct RemoteAccountDetails {
    state: LibraryListState,
    storage_usage: AccountStorageUsage,
    server_storage_usage: Vec<ServerLibraryStorageUsage>,
}

use super::core::*;

#[path = "account_tasks.rs"]
pub(super) mod tasks;

impl AppBackend {
    pub(crate) fn account_status(&self) -> Result<AccountStatus, BackendError> {
        let mut account = self.current_account()?;
        if account.is_none() {
            account = self.state.account.restore_if_empty().map_err(BackendError::operation)?;
            if account.is_some() {
                self.state.account_scheduler.request_remote_refresh();
            }
        }
        Ok(account.as_ref().map(|session| AccountStatus::authenticated(session.server_url().as_str(), session.email())).unwrap_or_else(AccountStatus::signed_out))
    }

    pub(crate) async fn login(&self, email: &str, password: &str) -> Result<(), BackendError> {
        let session = account_client::login(&crate::api_server_url(), email, password).await.map_err(BackendError::operation)?;
        self.adopt_session(session)
    }

    pub(crate) fn adopt_session(&self, session: account_client::Session) -> Result<(), BackendError> {
        self.state.account.set(session.clone()).map_err(BackendError::operation)?;
        self.state.account_scheduler.request_remote_refresh();
        Ok(())
    }

    pub(crate) async fn verify_email(&self, email: &str, pin: &str) -> Result<(), BackendError> {
        let session = account_client::verify_email(&crate::api_server_url(), email, pin).await.map_err(BackendError::operation)?;
        self.adopt_session(session)
    }

    pub(crate) fn logout(&self) -> Result<(), BackendError> {
        self.logout_with_notification(|session| async move {
            match crate::executor::timeout(std::time::Duration::from_secs(8), account_client::logout(&session)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::warn!("Failed to notify server about logout: {error}"),
                Err(_) => log::warn!("Server logout notification timed out"),
            }
        })
    }

    pub(super) fn logout_with_notification<F: crate::executor::BackendFuture<()>>(&self, notify: impl FnOnce(account_client::Session) -> F) -> Result<(), BackendError> {
        let session = self.state.account.take().map_err(BackendError::operation)?;
        self.state.account_scheduler.wake();
        if let Some(session) = session {
            // Revoke only the captured session. A subsequent login must not
            // wait for or be affected by this best-effort notification.
            self.state.executor.spawn_detached(notify(session));
        }
        Ok(())
    }

    pub(crate) async fn reconcile_libraries(&self) -> Result<LibraryListState, BackendError> {
        let Some(_) = self.current_account()? else {
            return self.library_list_state(None, None).await;
        };
        // Failed deletions stay durable; the worker discovers them on its next check.
        self.flush_pending_library_deletions().await?;
        self.refresh_remote_account().await
    }

    pub(crate) async fn refresh_remote_account(&self) -> Result<LibraryListState, BackendError> {
        Ok(self.refresh_remote_account_details().await?.state)
    }

    /// Fetches account facts only for an explicit caller. Nothing from this
    /// result is retained in AppState; the UI owns any presentation cache.
    async fn refresh_remote_account_details(&self) -> Result<RemoteAccountDetails, BackendError> {
        let (snapshot, policies) = tokio::try_join!(
            async {
                let started = web_time::Instant::now();
                let result = self.state.account.request(|session| Box::pin(account_client::account_snapshot(session))).await.map_err(BackendError::operation);
                log::debug!("account snapshot request took {:?}", started.elapsed());
                result
            },
            async {
                let started = web_time::Instant::now();
                let result = self.state.account.request(|session| Box::pin(account_client::cloud_storage(session))).await.map_err(BackendError::operation);
                log::debug!("account cloud storage request took {:?}", started.elapsed());
                result
            },
        )?;
        self.apply_remote_library_deletions(&snapshot.deleted_library_ids).await?;
        let session = self.require_account()?;
        // Settings is read-only and must not flush queued deletions. Keep those
        // libraries detached until the synchronization worker publishes the
        // deletion instead of resurrecting them from the remote snapshot.
        let pending_deletions = self.state.registry.pending_server_deletions().map_err(BackendError::operation)?.into_iter().collect::<HashSet<_>>();
        let libraries = snapshot.libraries.into_iter().filter(|library| !pending_deletions.contains(&library.library_id)).collect::<Vec<_>>();
        crate::platform::reserve_database_capacity(libraries.len()).await.map_err(BackendError::message)?;
        let library_storage_usage = snapshot
            .library_storage
            .into_iter()
            .map(|entry| {
                let library_id = entry.library_id.parse().map_err(BackendError::operation)?;
                let library_name = libraries.iter().find(|library| library.library_id == library_id).map(|library| library.library_name.clone()).unwrap_or_else(|| "Remote library".to_owned());
                Ok(ServerLibraryStorageUsage { library_id, library_name, used_bytes: entry.used_bytes })
            })
            .collect::<Result<Vec<_>, BackendError>>()?;
        let storage_usage = snapshot.storage.into();
        // Publish remote knowledge before discovery emits library-list events.
        let remote_ids = self.attach_remote_libraries(&libraries)?;
        let account = self.account_identity()?;
        self.check_account_identity(&account)?;
        self.apply_cloud_storage(&policies)?;
        Ok(RemoteAccountDetails { state: self.library_list_state(Some(&session), Some(&remote_ids)).await?, storage_usage, server_storage_usage: library_storage_usage })
    }

    pub(super) async fn flush_pending_library_deletions(&self) -> Result<bool, BackendError> {
        let mut all_flushed = true;
        for library_id in self.state.registry.pending_server_deletions().map_err(BackendError::operation)? {
            match self.state.account.request(|session| Box::pin(async move { account_client::delete_library(session, &library_id).await })).await {
                Ok(_) => {
                    if let Err(error) = self.state.registry.complete_server_deletion(&library_id) {
                        all_flushed = false;
                        log::warn!("server deleted library {library_id}, but its pending tombstone could not be cleared locally: {error}");
                    }
                }
                Err(error) => {
                    all_flushed = false;
                    log::warn!("library {library_id} remains queued for server deletion: {error}");
                }
            }
        }
        Ok(all_flushed)
    }

    async fn apply_remote_library_deletions(&self, deleted: &[LibraryId]) -> Result<(), BackendError> {
        let local_ids = self.state.registry.entries().map_err(BackendError::operation)?.into_iter().map(|entry| *entry.library_id()).collect::<std::collections::HashSet<_>>();
        for library_id in deleted.iter().filter(|library_id| local_ids.contains(library_id)) {
            self.remove_library_from_device(library_id).await?;
            self.state.registry.complete_server_deletion(library_id).map_err(BackendError::operation)?;
        }
        Ok(())
    }

    pub(super) fn attach_remote_libraries(&self, remote: &[sync_common::api::libraries::LibrarySummary]) -> Result<std::collections::HashSet<LibraryId>, BackendError> {
        let mut remote_ids = std::collections::HashSet::with_capacity(remote.len());
        let mut needing_open = Vec::new();
        let mut changed = false;
        for library in remote {
            let _guard = self.state.library_directory.lock_lifecycle()?;
            let existing = self.state.registry.entry(&library.library_id).map_err(BackendError::operation)?;
            remote_ids.insert(library.library_id.clone());
            if self.state.registry.is_detached(&library.library_id).map_err(BackendError::operation)? {
                continue;
            }
            // A stale server snapshot must not undo an offline rename.
            if self.state.registry.read_value(&format!("pending-library-rename/{}", library.library_id)).map_err(BackendError::operation)?.is_some() {
                continue;
            }
            let renamed = existing.as_ref().map(|entry| entry.library_name()) != Some(library.library_name.as_str());
            if renamed {
                changed = true;
            }
            // Library-list reconciliation updates registry metadata only. A live
            // Library owns expensive shared resources (pool, scanner, sync manager,
            // event forwarding) and must survive metadata refreshes unchanged.
            self.state.registry.attach_remote(&library.library_id, &library.library_name).map_err(BackendError::operation)?;
            if renamed {
                if let Ok(handle) = self.state.library_directory.get(&library.library_id) {
                    handle.set_display_name(library.library_name.clone());
                }
            }
            if self.state.library_directory.try_get(&library.library_id)?.is_none() {
                needing_open.push(library.library_id);
            }
        }
        // Discovery retries registered libraries that failed to open during
        // startup, after browser database capacity has been reserved. A
        // persistently inaccessible library remains listed but unavailable.
        for library_id in needing_open {
            if let Err(error) = self.open_registered_library(&library_id) {
                log::warn!("Could not open registered library {library_id} during discovery: {error}");
            }
        }
        if changed {
            self.send_library_list_changed();
        }
        Ok(remote_ids)
    }

    pub(super) async fn library_list_state(&self, session: Option<&account_client::Session>, remote: Option<&std::collections::HashSet<LibraryId>>) -> Result<LibraryListState, BackendError> {
        let libraries = self.libraries()?;
        let started = web_time::Instant::now();
        let sync_statuses = stream::iter(libraries.iter().cloned().map(|library| async move { self.library_sync_status(&library, session, remote).await }))
            .buffered(4)
            .try_collect()
            .await?;
        log::debug!("account library statuses took {:?}", started.elapsed());
        Ok(LibraryListState { libraries, sync_statuses })
    }

    pub(super) async fn library_sync_status(&self, library: &LibraryEntry, session: Option<&account_client::Session>, remote: Option<&HashSet<LibraryId>>) -> Result<LibrarySyncStatus, BackendError> {
        if let Some(handle) = self.state.library_directory.try_get(library.library_id())? {
            match handle.sync_status(session.cloned(), remote.map(|remote| remote.contains(library.library_id()))).await {
                Ok(status) => return Ok(status),
                Err(error) => log::warn!("Could not read sync status for library {}: {error}", library.library_id()),
            }
        }
        let mut status = LibrarySyncStatus::placeholder(*library.library_id(), LibrarySyncState::Unavailable);
        status.asset_storage_enabled = self.library_asset_storage_enabled(library.library_id())?;
        Ok(status)
    }

    /// Direct account-and-library snapshot for a foreground view. Expensive
    /// account discovery happens only because that view asked for it.
    pub(crate) async fn account_snapshot(&self) -> Result<crate::AccountSnapshot, BackendError> {
        let started = web_time::Instant::now();
        let account = self.account_status()?;
        let (state, storage_usage, server_storage_usage) = if self.current_account()?.is_some() {
            let remote = self.refresh_remote_account_details().await?;
            (remote.state, Some(remote.storage_usage), Some(remote.server_storage_usage))
        } else {
            (self.library_list_state(None, None).await?, None, None)
        };
        log::debug!("account snapshot took {:?}", started.elapsed());
        Ok(crate::AccountSnapshot { account, storage_usage, sync_statuses: state.sync_statuses, server_storage_usage })
    }

    pub(crate) async fn local_storage_usage(&self) -> Result<Vec<LocalLibraryStorageUsage>, BackendError> {
        let libraries = self.libraries()?;
        let started = web_time::Instant::now();
        let entries: Vec<Option<LocalLibraryStorageUsage>> = stream::iter(libraries.iter().map(|entry| async move {
            let Ok(library) = self.state.library_directory.get(entry.library_id()) else {
                return Ok::<_, BackendError>(None);
            };
            Ok(library.local_storage_usage().await?.map(|used_bytes| LocalLibraryStorageUsage {
                library_id: *entry.library_id(),
                library_name: entry.library_name().to_owned(),
                used_bytes,
            }))
        }))
        .buffered(4)
        .try_collect()
        .await?;
        let mut usage: Vec<_> = entries.into_iter().flatten().collect();
        usage.sort_by(|left, right| left.library_name.cmp(&right.library_name));
        log::debug!("account local storage usage took {:?}", started.elapsed());
        Ok(usage)
    }

    pub(super) fn require_account(&self) -> Result<account_client::Session, BackendError> {
        self.state.account.require().map_err(BackendError::operation)
    }

    pub(super) fn current_account(&self) -> Result<Option<account_client::Session>, BackendError> {
        self.state.account.current().map_err(BackendError::operation)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod unavailable_library_tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_library_does_not_block_account_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let library = backend.create_library_with_asset_storage("Unavailable".to_owned(), false).unwrap();
        let id = *library.library_id();
        backend.state.library_directory.retire(&id).unwrap();

        let snapshot = backend.account_snapshot().await.unwrap();
        let status = snapshot.sync_statuses.iter().find(|status| status.library_id == id).unwrap();
        assert_eq!(status.state, LibrarySyncState::Unavailable);
        assert!(!status.asset_storage_enabled);
        assert!(backend.local_storage_usage().await.unwrap().iter().all(|usage| usage.library_id != id));
    }

    #[tokio::test]
    async fn discovery_reopens_an_existing_registered_library() {
        let root = tempfile::tempdir().unwrap();
        let backend = AppBackend::new(BackendContext::initialize(AppDataLocation::native_path(root.path())).unwrap()).unwrap();
        let library = backend.create_library("Existing".to_owned(), None).unwrap();
        let id = *library.library_id();
        backend.state.library_directory.retire(&id).unwrap();

        let remote = [sync_common::api::libraries::LibrarySummary { library_id: id, library_name: "Existing".to_owned() }];
        assert!(backend.attach_remote_libraries(&remote).unwrap().contains(&id));
        assert!(backend.state.library_directory.try_get(&id).unwrap().is_some());
    }
}
