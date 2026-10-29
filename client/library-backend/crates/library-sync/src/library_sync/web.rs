//! Browser coordinator owns serialization across tabs and worker lifetimes.
use super::*;

pub(super) async fn exchange(sync: &LibrarySync, user: &str, credentials: SyncCredentials, outbox_only: bool) -> SyncResult<SyncOutcome> {
    Ok(crate::synchronize(sync.database.clone(), sync.events.clone(), sync.library_id, sync.replica_id, user, &credentials, outbox_only).await?)
}
