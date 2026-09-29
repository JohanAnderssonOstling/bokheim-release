//! Direct sync-engine execution and native serialization of sync passes.
use super::*;

pub(super) async fn lock(sync: &LibrarySync) -> tokio::sync::MutexGuard<'_, ()> {
    sync.sync_gate.lock().await
}

impl LibrarySync {
    #[cfg(test)]
    pub(super) fn state_engine(&self) -> SyncResult<sync_engine::SyncEngine<'_, crate::sync::state_sync::AccountStore<'_>>> {
        let (user, credentials) = self.sync_identity()?;
        self.engine_for(user, credentials)
    }

    fn engine_for(&self, _user: Option<String>, credentials: Option<sync_transport::SyncCredentials>) -> SyncResult<sync_engine::SyncEngine<'_, crate::sync::state_sync::AccountStore<'_>>> {
        Ok(sync_engine::SyncEngine::new(sync_engine::SyncEngineConfig {
            store: crate::sync::state_sync::AccountStore { sync: self },
            http_client: &self.http_client,
            library_id: self.library_id,
            replica_id: self.replica_id,
            credentials,
            inventory_checked: &self.inventory_checked,
        }))
    }
}

pub(super) async fn exchange(sync: &LibrarySync, user: &Option<String>, credentials: Option<sync_transport::SyncCredentials>, mode: SyncMode) -> SyncResult<Option<SyncExchange>> {
    let engine = sync.engine_for(user.clone(), credentials)?;
    let outcome = match mode {
        SyncMode::Full => engine.synchronize().await?,
        SyncMode::Outbox => engine.drain_outbox().await?,
        SyncMode::Repair => {
            engine.repair_inventory().await?;
            SyncOutcome::default()
        }
    };
    Ok(Some(SyncExchange { outcome, inventory_checked: engine.inventory_checked() }))
}
