//! Owner-committed pages for the storage-independent sync engine.
use library_database::DatabaseError;
use sync_common::{MutationId, PullStateResponse, StateCell, SyncCursor};
use sync_engine::{StoreError, StoreFuture};

/// Storage commands for a complete native exchange or repair pass. The local
/// library is authoritative, so commands never validate a stored account
/// binding: switching accounts leaves sync state untouched.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct AccountStore<'a> {
    pub sync: &'a super::LibrarySync,
}

#[cfg(not(target_arch = "wasm32"))]
impl AccountStore<'_> {
    /// One commit connection per command, opened inline from the library
    /// path stored on the sync handle. The store never receives storage
    /// handles, so every future below captures an owned outcome and stays
    /// `Send` without sharing primitives.
    fn database(&self) -> Result<library_database::Database, StoreError> {
        self.sync.database().map_err(|error| Box::new(library_database::DatabaseError::operation(error.to_string())) as StoreError)
    }
    /// One owner transaction: acknowledge the completed outbox mutations and
    /// apply the incoming page atomically. Everything runs synchronously so
    /// the caller's future captures an owned outcome and stays Send.
    ///
    /// A remote-originated directory rename queues the same durable work a
    /// local move does, but the replay can't happen inline here: `SyncStore`
    /// futures must stay `Send`, and holding `&self` across an `.await`
    /// breaks that, since the connection isn't `Sync`. The caller
    /// that drives this exchange, outside this trait's `Send` bound, drains
    /// pending directory work once the page settles instead.
    fn commit_response(&self, ids: &[MutationId], page: &PullStateResponse) -> Result<bool, StoreError> {
        self.database()?.sync_commit_pull_response(ids, page).map_err(store_error)
    }
    /// Batching policy belongs to the orchestrator: one owner transaction
    /// per bounded chunk.
    fn acknowledge(&self, ids: &[MutationId]) -> Result<(), StoreError> {
        let database = self.database()?;
        ids.chunks(256).try_for_each(|chunk| database.sync_acknowledge_mutations(chunk)).map_err(store_error)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl sync_engine::SyncStore for AccountStore<'_> {
    fn cursor_recovery_pending(&self) -> StoreFuture<'_, bool> {
        let pending = self.database().and_then(|database| database.sync_cursor_recovery_pending().map_err(store_error));
        Box::pin(async move { pending })
    }
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()> {
        let begun = self.database().and_then(|database| database.sync_begin_cursor_recovery().map_err(store_error));
        Box::pin(async move { begun })
    }
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize> {
        // Bounded transactions per page, driven synchronously so the returned
        // future captures no database reference and stays Send. No cooperative
        // yield: there is no storage queue to yield to anymore.
        let mut after = None;
        let mut inserted = 0;
        let recovered: Result<usize, DatabaseError> = loop {
            let database = match self.database() {
                Ok(database) => database,
                Err(error) => break Err(DatabaseError::operation(error.to_string())),
            };
            match database.sync_recover_state_page(after.as_ref()) {
                Ok((next, count)) => {
                    inserted += count;
                    if next.is_none() {
                        break Ok(inserted);
                    }
                    after = next;
                }
                Err(error) => break Err(error),
            }
        };
        Box::pin(async move { recovered.map_err(store_error) })
    }
    fn pull_cursor(&self) -> StoreFuture<'_, SyncCursor> {
        let cursor = self.database().and_then(|database| database.sync_pull_cursor().map_err(store_error));
        Box::pin(async move { cursor })
    }
    fn inventory_page(&self, after: Option<StateCell>) -> StoreFuture<'_, Vec<StateCell>> {
        let page = self.database().and_then(|database| database.sync_inventory_page(after.as_ref()).map_err(store_error));
        Box::pin(async move { page })
    }
    fn inventory_checkpoint(&self) -> StoreFuture<'_, Option<StateCell>> {
        let checkpoint = self.database().and_then(|database| database.sync_inventory_checkpoint().map_err(store_error));
        Box::pin(async move { checkpoint })
    }
    fn save_inventory_checkpoint(&self, after: Option<StateCell>) -> StoreFuture<'_, ()> {
        let saved = self.database().and_then(|database| database.sync_save_inventory_checkpoint(after).map_err(store_error));
        Box::pin(async move { saved })
    }
    fn enqueue_missing_state_cells(&self, cells: &[StateCell]) -> StoreFuture<'_, usize> {
        let enqueued = self.database().and_then(|database| database.sync_enqueue_missing_state_cells(cells).map_err(store_error));
        Box::pin(async move { enqueued })
    }
    fn pending_mutations(&self) -> StoreFuture<'_, Vec<library_replica::StateMutation>> {
        let mutations = self.database().and_then(|database| database.sync_publishable_mutations().map_err(store_error));
        Box::pin(async move { mutations })
    }
    fn acknowledge_mutations(&self, ids: &[MutationId]) -> StoreFuture<'_, ()> {
        // The work runs synchronously here so the returned future captures
        // no database reference and stays Send.
        let committed = self.acknowledge(ids);
        Box::pin(async move { committed })
    }
    fn apply_pull_page(&self, page: &PullStateResponse) -> StoreFuture<'_, ()> {
        let result = self.commit_response(&[], page).map(|changed| {
            if changed {
                self.sync.notify_remote_page_applied();
            }
        });
        Box::pin(async move { result })
    }
    fn apply_exchange_response(&self, ids: &[MutationId], page: &PullStateResponse) -> StoreFuture<'_, ()> {
        if ids.is_empty() {
            return self.apply_pull_page(page);
        }
        // Keep the same bound as ordinary acknowledgment commands.
        if ids.len() > 256 {
            let result = self.acknowledge(ids).and_then(|()| {
                self.commit_response(&[], page).map(|changed| {
                    if changed {
                        self.sync.notify_remote_page_applied();
                    }
                })
            });
            return Box::pin(async move { result });
        }
        let result = match self.commit_response(ids, page) {
            Ok(changed) => {
                if changed {
                    self.sync.notify_remote_page_applied();
                }
                Ok(())
            }
            Err(error) => {
                // Failed application/commit rolled back the whole command.
                // Re-acknowledge under a fresh transaction, retaining the
                // prior semantics without partially applying pull.
                self.acknowledge(ids).and(Err(error))
            }
        };
        Box::pin(async move { result })
    }
}

fn store_error(error: DatabaseError) -> StoreError {
    Box::new(error)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod account_tests;
