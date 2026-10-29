//! State-sync coordination over short, typed storage commands. HTTP uses the
//! coordinator's network worker; this WASM entry point never opens storage.
#![cfg(target_arch = "wasm32")]

use crate::engine::{PreparedSync, StoreFuture, SyncStore};
use crate::state_sync::{PublicationOutbox, acknowledge, apply_response, next_outbox_page, prepare_outbox, prepare_sync};
use library_runtime::events::LibraryEventRouter;
use serde::{Deserialize, Serialize};
use sync_common::{LibraryId, PullStateResponse, ReplicaId, ReplicaSeq};

#[derive(Serialize, Deserialize)]
struct SyncInput {
    library_id: LibraryId,
    replica_id: ReplicaId,
    server_url: String,
    access_token: String,
    outbox_only: bool,
}

#[derive(Serialize, Deserialize)]
struct SyncOutput {
    result: Result<Vec<sync_common::MutationIssue>, crate::engine::SyncError>,
}

#[derive(Serialize, Deserialize)]
enum StorageCommand {
    PrepareSync,
    BeginCursorRecovery,
    CompleteCursorRecovery,
    PrepareOutbox,
    PendingMutations,
    AcknowledgeMutations(Vec<ReplicaSeq>),
    ApplyExchangeResponse { ids: Vec<ReplicaSeq>, page: PullStateResponse },
    RecordMutationIssues(Vec<sync_common::MutationIssue>),
}

async fn storage_command(database: &library_database::Database, events: Option<&LibraryEventRouter>, scan: &mut Option<PublicationOutbox>, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let command: StorageCommand = rmp_serde::from_slice(bytes).map_err(|error| error.to_string())?;
    macro_rules! reply {
        ($value:expr) => {
            rmp_serde::to_vec_named(&$value.map_err(|error| error.to_string())?).map_err(|error| error.to_string())
        };
    }
    match command {
        StorageCommand::RecordMutationIssues(issues) => {
            database.sync_record_mutation_issues(&issues).map_err(|error| error.to_string())?;
            if let Some(events) = events {
                events.publish_update(library_model::LibraryUpdate::Contents);
            }
            rmp_serde::to_vec_named(&()).map_err(|error| error.to_string())
        }
        StorageCommand::PrepareSync => {
            let prepared = match prepare_sync(database, events).map_err(|error| error.to_string())? {
                PreparedSync::RecoveryPending => {
                    *scan = None;
                    PreparedSync::RecoveryPending
                }
                PreparedSync::Ready { cursor, outbox, mutations } => {
                    *scan = Some(outbox);
                    PreparedSync::Ready { cursor, outbox: (), mutations }
                }
            };
            rmp_serde::to_vec_named(&prepared).map_err(|error| error.to_string())
        }
        StorageCommand::BeginCursorRecovery => reply!(database.sync_begin_cursor_recovery()),
        StorageCommand::CompleteCursorRecovery => {
            let inserted = crate::recovery::complete(database).await.map_err(|error| error.to_string())?;
            rmp_serde::to_vec_named(&inserted).map_err(|error| error.to_string())
        }
        StorageCommand::PrepareOutbox => {
            *scan = Some(prepare_outbox(database, events).map_err(|error| error.to_string())?);
            rmp_serde::to_vec_named(&()).map_err(|error| error.to_string())
        }
        StorageCommand::PendingMutations => {
            let scan = scan.as_mut().ok_or_else(|| "outbox scan has not been prepared".to_owned())?;
            reply!(next_outbox_page(database, events, scan))
        }
        StorageCommand::AcknowledgeMutations(ids) => reply!(acknowledge(database, events, &ids)),
        StorageCommand::ApplyExchangeResponse { ids, page } => reply!(apply_response(database, events, &ids, &page)),
    }
}

pub async fn synchronize(
    database: std::sync::Arc<library_database::Database>, events: Option<LibraryEventRouter>, library_id: LibraryId, replica_id: ReplicaId, user_id: &str, credentials: &sync_transport::SyncCredentials, outbox_only: bool,
) -> Result<crate::engine::SyncOutcome, crate::engine::SyncError> {
    let input = SyncInput { outbox_only, library_id, replica_id, server_url: credentials.server_url().to_string(), access_token: credentials.access_token().to_owned() };
    let bytes = rmp_serde::to_vec_named(&input).map_err(|error| crate::engine::SyncError::Failed(error.to_string()))?;
    let mut scan = None;
    let storage = async move |bytes: Vec<u8>| storage_command(&database, events.as_ref(), &mut scan, &bytes).await;
    let key = format!("{}|{user_id}|{library_id}|state", credentials.server_url());
    let result = async {
        let response = client_platform_web::transport::bridge::sync_request(key, bytes, storage).map_err(|error| format!("coordinator unavailable: {error:?}"))?;
        let response = response.await?;
        rmp_serde::from_slice::<SyncOutput>(&response).map_err(|error| error.to_string())
    }
    .await
    .map_err(crate::engine::SyncError::Failed)?;
    result.result.map(|mutation_issues| crate::engine::SyncOutcome { mutation_issues })
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
    type Outbox = ();

    fn prepare_sync(&self) -> StoreFuture<'_, PreparedSync<Self::Outbox>> {
        self.command(StorageCommand::PrepareSync)
    }
    fn prepare_outbox(&self) -> StoreFuture<'_, Self::Outbox> {
        self.command(StorageCommand::PrepareOutbox)
    }
    fn record_mutation_issues(&self, issues: &[sync_common::MutationIssue]) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::RecordMutationIssues(issues.to_vec()))
    }
    fn begin_cursor_recovery(&self) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::BeginCursorRecovery)
    }
    fn complete_cursor_recovery(&self) -> StoreFuture<'_, usize> {
        self.command(StorageCommand::CompleteCursorRecovery)
    }
    fn pending_mutations<'a>(&'a self, _scan: &'a mut Self::Outbox) -> StoreFuture<'a, Vec<library_replica::StatePublication>> {
        self.command(StorageCommand::PendingMutations)
    }
    fn acknowledge_mutations(&self, ids: &[ReplicaSeq]) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::AcknowledgeMutations(ids.to_vec()))
    }
    fn apply_exchange_response(&self, ids: &[ReplicaSeq], page: &PullStateResponse) -> StoreFuture<'_, ()> {
        self.command(StorageCommand::ApplyExchangeResponse { ids: ids.to_vec(), page: page.clone() })
    }
}

/// Invoked only in the SharedWorker's separate WASM module.
pub async fn run(bytes: &[u8], storage: client_platform_web::transport::BytesOperation) -> Result<Vec<u8>, String> {
    let input: SyncInput = rmp_serde::from_slice(bytes).map_err(|error| error.to_string())?;
    let server_url = sync_transport::Url::parse(&input.server_url).map_err(|error| error.to_string())?;
    let credentials = Some(sync_transport::SyncCredentials::new(server_url, input.access_token));
    let store = CoordinatorStore(storage);
    let client = reqwest::Client::new();
    let engine = crate::engine::SyncEngine::new(crate::engine::SyncEngineConfig { store, http_client: &client, library_id: input.library_id, replica_id: input.replica_id, credentials });
    let result = if input.outbox_only { engine.drain_outbox().await } else { engine.synchronize().await }.map(|outcome| outcome.mutation_issues);
    rmp_serde::to_vec_named(&SyncOutput { result }).map_err(|error| error.to_string())
}
