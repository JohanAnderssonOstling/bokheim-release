//! Direct sync-engine execution against the library's storage owner.

use super::*;
use crate::engine::{SyncEngine, SyncEngineConfig};
use crate::state_sync::AccountStore;

pub(super) async fn exchange(sync: &LibrarySync, _user: &str, credentials: SyncCredentials, outbox_only: bool) -> SyncResult<SyncOutcome> {
    let engine = SyncEngine::new(SyncEngineConfig { store: AccountStore { sync }, http_client: &sync.http_client, library_id: sync.library_id, replica_id: sync.replica_id, credentials: Some(credentials) });
    Ok(if outbox_only { engine.drain_outbox().await? } else { engine.synchronize().await? })
}
