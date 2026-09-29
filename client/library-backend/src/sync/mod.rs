//! Metadata synchronization and binary asset transfer.

mod asset_sync;
mod error;
mod library_sync;
pub(crate) mod recovery;
mod scheduler;
mod state_sync;
#[cfg(target_arch = "wasm32")]
mod web_coordinator;

pub use library_runtime::scheduler::{BackgroundTiming, SyncRequest, SyncScheduler};
pub use library_sync::{LibrarySync, LibrarySyncConfig};
#[cfg(target_arch = "wasm32")]
pub use scheduler::run_timed;
#[cfg(target_arch = "wasm32")]
pub use web_coordinator::run;
#[cfg(target_arch = "wasm32")]
pub(crate) use web_coordinator::synchronize;

#[cfg(test)]
pub(crate) fn fixture_book_creations(changes: &[sync_common::ServerMutation]) -> Vec<sync_common::ServerMutation> {
    use library_replica::{BookLifecycleState, MutationBody};
    let mut result = std::collections::BTreeMap::new();
    for change in changes {
        let Ok(Some(body)) = MutationBody::from_wire(&change.mutation) else { continue };
        let Some(hash) = body.upload_dependency() else { continue };
        let declaration = if matches!(body, MutationBody::BookLifecycle { .. }) {
            change.clone()
        } else {
            sync_common::ServerMutation {
                mutation: MutationBody::BookLifecycle { content_hash: hash, value: BookLifecycleState::Present }
                    .to_wire(sync_common::MutationId::parse(&uuid::Uuid::from_u128(1).to_string()).unwrap(), 0, sync_common::ReplicaSeq::new(1).unwrap())
                    .unwrap(),
                replica_id: uuid::Uuid::from_u128(1),
                revision: sync_common::LibraryRevision::new(1).unwrap(),
            }
        };
        if matches!(body, MutationBody::BookLifecycle { .. }) {
            result.insert(hash.to_string(), declaration);
        } else {
            result.entry(hash.to_string()).or_insert(declaration);
        }
    }
    result.into_values().collect()
}
