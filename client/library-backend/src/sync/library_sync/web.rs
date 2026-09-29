//! Browser coordinator owns serialization across tabs and worker lifetimes.
use super::*;

pub(super) async fn lock(_sync: &LibrarySync) {}

pub(super) async fn exchange(sync: &LibrarySync, user: &Option<String>, credentials: Option<sync_transport::SyncCredentials>, mode: SyncMode) -> SyncResult<Option<SyncExchange>> {
    let Some(credentials) = credentials else { return Ok(None) };
    let (outcome, inventory_checked) = crate::sync::synchronize(
        sync.database()?,
        sync.event_tx.clone(),
        sync.library_id,
        sync.replica_id,
        user.as_deref().unwrap_or_default(),
        &credentials,
        &sync.inventory_checked,
        matches!(mode, SyncMode::Outbox),
        matches!(mode, SyncMode::Repair),
    )
    .await?;
    Ok(Some(SyncExchange { outcome, inventory_checked }))
}
