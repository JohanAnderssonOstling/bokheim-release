//! State-sync coordination over short, typed storage commands. HTTP uses the
//! coordinator's network worker; this WASM entry point never opens storage.
#![cfg(target_arch = "wasm32")]

use library_runtime::events::{LibraryEvent, LibraryEventSender};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use sync_common::{LibraryId, MutationId, PullStateResponse, ReplicaId, StateCell, SyncCursor};
use sync_engine::{StoreFuture, SyncStore};

#[derive(Serialize, Deserialize)]
struct SyncInput {
    library_id: LibraryId,
    replica_id: ReplicaId,
    server_url: String,
    access_token: String,
    inventory_checked: bool,
    outbox_only: bool,
    repair: bool,
}

#[derive(Serialize, Deserialize)]
struct SyncOutput {
    inventory_checked: bool,
    result: Result<Vec<sync_common::MutationIssue>, sync_engine::SyncError>,
}

#[derive(Serialize, Deserialize)]
enum StorageCommand {
    CursorRecoveryPending,
    BeginCursorRecovery,
    CompleteCursorRecovery,
    PullCursor,
    StateInventory(Option<StateCell>),
    InventoryCheckpoint,
    SaveInventoryCheckpoint(Option<StateCell>),
    EnqueueMissingStateCells(Vec<StateCell>),
    OutboxWatermark,
    PendingMutations { after: i64, through: i64 },
    AcknowledgeMutations(Vec<MutationId>),
    ApplyPullPage(PullStateResponse),
}

async fn storage_command(database: &library_database::Database, events: Option<&LibraryEventSender>, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let command: StorageCommand = rmp_serde::from_slice(bytes).map_err(|error| error.to_string())?;
    macro_rules! reply {
        ($value:expr) => {
            rmp_serde::to_vec_named(&$value.map_err(|error| error.to_string())?).map_err(|error| error.to_string())
        };
    }
    match command {
        StorageCommand::CursorRecoveryPending => reply!(database.sync_cursor_recovery_pending()),
        StorageCommand::BeginCursorRecovery => reply!(database.sync_begin_cursor_recovery()),
        StorageCommand::CompleteCursorRecovery => {
            let inserted = crate::sync::recovery::complete(database).await.map_err(|error| error.to_string())?;
            rmp_serde::to_vec_named(&inserted).map_err(|error| error.to_string())
        }
        StorageCommand::PullCursor => reply!(database.sync_pull_cursor()),
        StorageCommand::InventoryCheckpoint => reply!(database.sync_inventory_checkpoint()),
        StorageCommand::SaveInventoryCheckpoint(after) => reply!(database.sync_save_inventory_checkpoint(after)),
        StorageCommand::StateInventory(after) => reply!(database.sync_inventory_page(after.as_ref())),
        // The coordinator sends at most 256 cells in each command.
        StorageCommand::EnqueueMissingStateCells(cells) => reply!(database.sync_enqueue_missing_state_cells(&cells)),
        StorageCommand::OutboxWatermark => reply!(database.sync_outbox_watermark()),
        StorageCommand::PendingMutations { after, through } => reply!(database.sync_pending_mutations_page(after, through)),
        StorageCommand::AcknowledgeMutations(ids) => {
            // One commit per bounded batch, rather than one OPFS flush per ID.
            for chunk in ids.chunks(256) {
                database.sync_acknowledge_mutations(chunk).map_err(|error| error.to_string())?;
            }
            rmp_serde::to_vec_named(&()).map_err(|error| error.to_string())
        }
        StorageCommand::ApplyPullPage(page) => {
            // Acknowledgement and apply stay separate commands on the browser
            // protocol; empty ids apply without acknowledging.
            let changed = database.sync_commit_pull_response(&[], &page).map_err(|error| error.to_string())?;
            if changed {
                if let Some(events) = events {
                    let _ = events.try_send(LibraryEvent::ContentsChanged);
                }
            }
            rmp_serde::to_vec_named(&()).map_err(|error| error.to_string())
        }
    }
}

pub(crate) async fn synchronize(
    database: library_database::Database, events: Option<LibraryEventSender>, library_id: LibraryId, replica_id: ReplicaId, user_id: &str, credentials: &sync_transport::SyncCredentials, inventory_checked: &AtomicBool, outbox_only: bool,
    repair: bool,
) -> Result<(sync_engine::SyncOutcome, bool), sync_engine::SyncError> {
    let input = SyncInput { outbox_only, repair, library_id, replica_id, server_url: credentials.server_url().to_string(), access_token: credentials.access_token().to_owned(), inventory_checked: inventory_checked.load(Ordering::Acquire) };
    let bytes = rmp_serde::to_vec_named(&input).map_err(|error| sync_engine::SyncError::Failed(error.to_string()))?;
    let database = std::rc::Rc::new(database);
    let callback_database = database.clone();
    let storage: client_platform_web::transport::BytesOperation = std::rc::Rc::new(move |bytes| {
        let database = callback_database.clone();
        let events = events.clone();
        Box::pin(async move { storage_command(&database, events.as_ref(), &bytes).await })
    });
    let key = format!("{}|{user_id}|{library_id}|{}", credentials.server_url(), if repair { "repair" } else { "state" });
    let result = async {
        let response = client_platform_web::transport::bridge::sync_request(key, bytes, storage).map_err(|error| format!("coordinator unavailable: {error:?}"))?;
        let response = response.await?;
        rmp_serde::from_slice::<SyncOutput>(&response).map_err(|error| error.to_string())
    }
    .await
    .map_err(sync_engine::SyncError::Failed)?;
    result.result.map(|mutation_issues| (sync_engine::SyncOutcome { mutation_issues }, result.inventory_checked))
}

struct CoordinatorStore(client_platform_web::transport::BytesOperation);

impl CoordinatorStore {
    fn command<T: serde::de::DeserializeOwned + 'static>(&self, command: StorageCommand) -> StoreFuture<'_, T> {
        Box::pin(async move {
            let bytes = rmp_serde::to_vec_named(&command)?;
            let reply = (self.0)(bytes).await.map_err(std::io::Error::other)?;
            Ok(rmp_serde::from_slice(&reply)?)
        })
    }
}

impl SyncStore for CoordinatorStore {
    fn cursor_recovery_pending(&self) -> StoreFuture<'_, bool> {
        self.command(StorageCommand::CursorRecoveryPending)
    }
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::BeginCursorRecovery)
    }
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize> {
        self.command(StorageCommand::CompleteCursorRecovery)
    }
    fn pull_cursor(&self) -> StoreFuture<'_, SyncCursor> {
        self.command(StorageCommand::PullCursor)
    }
    fn inventory_page(&self, after: Option<StateCell>) -> StoreFuture<'_, Vec<StateCell>> {
        self.command(StorageCommand::StateInventory(after))
    }
    fn inventory_checkpoint(&self) -> StoreFuture<'_, Option<StateCell>> {
        self.command(StorageCommand::InventoryCheckpoint)
    }
    fn save_inventory_checkpoint(&self, after: Option<StateCell>) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::SaveInventoryCheckpoint(after))
    }
    fn enqueue_missing_state_cells(&self, cells: &[StateCell]) -> StoreFuture<'_, usize> {
        let cells = cells.to_vec();
        Box::pin(async move {
            let mut inserted = 0;
            for chunk in cells.chunks(256) {
                inserted += self.command::<usize>(StorageCommand::EnqueueMissingStateCells(chunk.to_vec())).await?;
            }
            Ok(inserted)
        })
    }
    fn pending_mutations(&self) -> StoreFuture<'_, Vec<library_replica::StateMutation>> {
        Box::pin(async {
            let through = self.command(StorageCommand::OutboxWatermark).await?;
            let mut after = i64::MIN;
            let mut mutations = Vec::new();
            loop {
                let page: Vec<library_replica::StateMutation> = self.command(StorageCommand::PendingMutations { after, through }).await?;
                if page.is_empty() {
                    return Ok(mutations);
                }
                after = page.last().unwrap().replica_seq.get() as i64;
                mutations.extend(page);
            }
        })
    }
    fn acknowledge_mutations(&self, ids: &[MutationId]) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::AcknowledgeMutations(ids.to_vec()))
    }
    fn apply_pull_page(&self, page: &PullStateResponse) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::ApplyPullPage(page.clone()))
    }
    fn apply_exchange_response(&self, ids: &[MutationId], page: &PullStateResponse) -> StoreFuture<'_, ()> {
        let ids = ids.to_vec();
        let page = page.clone();
        Box::pin(async move {
            // Preserve the existing browser command protocol. The combined
            // transaction optimization currently applies to native storage.
            if !ids.is_empty() {
                self.acknowledge_mutations(&ids).await?;
            }
            self.apply_pull_page(&page).await
        })
    }
}

/// Invoked only in the SharedWorker's separate WASM module.
pub async fn run(bytes: &[u8], storage: client_platform_web::transport::BytesOperation) -> Result<Vec<u8>, String> {
    let input: SyncInput = rmp_serde::from_slice(bytes).map_err(|error| error.to_string())?;
    let server_url = sync_transport::ServerUrl::parse(&input.server_url).map_err(|error| error.to_string())?;
    let credentials = Some(sync_transport::SyncCredentials::new(server_url, input.access_token));
    let checked = AtomicBool::new(input.inventory_checked);
    let store = CoordinatorStore(storage);
    let client = reqwest::Client::new();
    let engine = sync_engine::SyncEngine::new(sync_engine::SyncEngineConfig { store, http_client: &client, library_id: input.library_id, replica_id: input.replica_id, credentials, inventory_checked: &checked });
    let result = if input.repair {
        engine.repair_inventory().await.map(|()| sync_engine::SyncOutcome::default())
    } else if input.outbox_only {
        engine.drain_outbox().await
    } else {
        engine.synchronize().await
    }
    .map(|outcome| outcome.mutation_issues);
    rmp_serde::to_vec_named(&SyncOutput { inventory_checked: engine.inventory_checked(), result }).map_err(|error| error.to_string())
}
