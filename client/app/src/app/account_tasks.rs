//! Account discovery, queued account writes, registration, and credential retries.
use super::super::core::*;
use sync_common::api::libraries::{CloudStorageChange, LibraryCloudStorage};

/// Account-global maintenance is owned by the account subsystem.  It holds
/// only a weak app state; library workers are never scheduled from here.
pub(in crate::app) fn start_maintenance(executor: &crate::executor::BackendExecutor, state: &std::sync::Weak<AppState>, account: (SyncScheduler, async_channel::Receiver<crate::sync::SyncRequest>)) {
    let (account_scheduler, account_requests) = account;
    let account_state = state.clone();
    executor.spawn_detached(account_scheduler.run(
        move |_| {
            let state = account_state.clone();
            async move {
                let Some(state) = state.upgrade() else { return Ok(()) };
                AppBackend { state }.run_account_maintenance().await
            }
        },
        account_requests,
        BACKGROUND_SYNC_RETRY_INTERVAL,
        std::time::Duration::from_millis(100),
    ));
}

impl AppBackend {
    pub(super) async fn publish_pending_library_names(&self) -> Result<(), BackendError> {
        if self.current_account()?.is_none() {
            return Ok(());
        }
        let mut failure = None;
        for entry in self.libraries()? {
            let id = *entry.library_id();
            let result = async {
                let key = format!("pending-library-rename/{id}");
                let Some(pending) = self.state.registry.read_value(&key).map_err(BackendError::operation)? else {
                    return Ok::<(), BackendError>(());
                };
                let name = String::from_utf8(pending.clone()).map_err(BackendError::operation)?;
                self.state.account.request(|session| Box::pin(account_client::rename_library(session, id, name.clone()))).await.map_err(BackendError::operation)?;
                let _guard = self.state.library_directory.lock_lifecycle()?;
                // A second rename during the request must remain queued.
                if self.state.registry.read_value(&key).map_err(BackendError::operation)?.as_ref() == Some(&pending) {
                    self.state.registry.remove_value(&key).map_err(BackendError::operation)?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                log::warn!("rename for library {id} remains queued: {error}");
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub(in crate::app) async fn run_account_maintenance(&self) -> Result<(), ()> {
        let result = async {
            let account = self.account_identity()?;
            if account.is_none() {
                return Ok(());
            }
            let (writes, refresh) = tokio::join!(self.flush_account_writes_cycle(), self.refresh_remote_account());
            writes?;
            refresh?;
            self.check_account_identity(&account)
        }
        .await;
        result.map_err(|error| log::warn!("account discovery will retry: {error}"))
    }

    pub(in crate::app) async fn flush_account_writes_cycle(&self) -> Result<(), BackendError> {
        let mut changed = self.state.account.subscribe();
        changed.borrow_and_update();
        let work = async {
            if self.current_account()?.is_none() {
                return Ok(());
            }
            // Both queues get an attempt even if the other fails.
            let (names, deletions, cloud) = tokio::join!(self.publish_pending_library_names(), self.flush_pending_library_deletions(), self.publish_pending_cloud_storage());
            cloud?;
            names?;
            if deletions? {
                Ok(())
            } else {
                Err(BackendError::message("library deletions remain pending"))
            }
        };
        tokio::select! {
            biased;
            _ = changed.changed() => Ok(()),
            result = work => result,
        }
    }

    pub(in crate::app) fn account_identity(&self) -> Result<Option<String>, BackendError> {
        Ok(self.current_account()?.map(|session| format!("{}|{}", session.server_url(), session.user_id())))
    }

    pub(in crate::app) fn check_account_identity(&self, account: &Option<String>) -> Result<(), BackendError> {
        if self.account_identity()? != *account {
            return Err(BackendError::message("account changed during background synchronization"));
        }
        Ok(())
    }
}

impl AppBackend {
    pub(super) async fn publish_cloud_storage(&self, id: LibraryId) -> Result<(), BackendError> {
        let key = format!("pending-cloud-storage:{id}");
        let Some(pending) = self.state.registry.read_value(&key).map_err(BackendError::operation)? else { return Ok(()) };
        let text = std::str::from_utf8(&pending).map_err(BackendError::operation)?;
        let (change_id, value) = text.split_once(':').ok_or_else(|| BackendError::message("Invalid pending cloud storage change"))?;
        let change = CloudStorageChange { change_id: change_id.parse().map_err(BackendError::operation)?, enabled: value == "1" };
        let account = self.account_identity()?;
        let result = self.state.account.request(|session| Box::pin(account_client::set_cloud_storage(session, id, change.clone()))).await.map_err(BackendError::operation)?;
        self.check_account_identity(&account)?;
        if self.state.registry.read_value(&key).map_err(BackendError::operation)?.as_ref() == Some(&pending) {
            self.state.registry.cache_cloud_storage_with_reset(&id, result.enabled).map_err(BackendError::operation)?;
            self.apply_open_cloud_storage(id, result.enabled)?;
            self.state.registry.remove_value(&key).map_err(BackendError::operation)?;
        }
        self.state.account_scheduler.request_remote_refresh();
        Ok(())
    }

    pub(super) fn apply_cloud_storage(&self, states: &[LibraryCloudStorage]) -> Result<(), BackendError> {
        for state in states {
            let id = state.library_id;
            if self.state.registry.entry(&id).map_err(BackendError::operation)?.is_none() || self.state.registry.read_value(&format!("pending-cloud-storage:{id}")).map_err(BackendError::operation)?.is_some() {
                continue;
            }
            if self.library_asset_storage_enabled(&id)? != state.enabled {
                self.state.registry.cache_cloud_storage_with_reset(&id, state.enabled).map_err(BackendError::operation)?;
                self.apply_open_cloud_storage(id, state.enabled)?;
            }
        }
        Ok(())
    }

    fn apply_open_cloud_storage(&self, id: LibraryId, enabled: bool) -> Result<(), BackendError> {
        if let Ok(library) = self.state.library_directory.get(&id) {
            library.set_asset_storage(enabled, true);
        }
        Ok(())
    }

    pub(super) async fn publish_pending_cloud_storage(&self) -> Result<(), BackendError> {
        let mut failure = None;
        for entry in self.libraries()? {
            let id = *entry.library_id();
            if self.state.registry.read_value(&format!("pending-cloud-storage:{id}")).map_err(BackendError::operation)?.is_none() {
                continue;
            }
            if let Err(error) = self.publish_cloud_storage(id).await {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}
