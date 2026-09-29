//! Compatibility routing for the earlier flat browser book-subscription command.
//! Shared application requests and their reply contracts live in runtime::requests.
use crate::app::AppBackend;
use crate::runtime::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum WebAppCommand {
    SubscribeBook { library_id: sync_common::LibraryId, content_hash: crate::ContentHash },
}
impl WebAppCommand {
    pub(crate) fn is_book_subscription(&self) -> bool {
        matches!(self, Self::SubscribeBook { .. })
    }
    /// The subscription carries no events: its receiver is the reader's lease,
    /// and dropping it releases the worker-owned book generation.
    pub(crate) async fn dispatch_app(self, backend: AppBackend) -> Result<WorkerDispatchResult<client_runtime::reply::Reply>, crate::BackendError> {
        match self {
            Self::SubscribeBook { library_id, content_hash } => {
                let library = backend.library_directory().get(&library_id).map_err(crate::BackendError::operation)?;
                let (initial, events) = library.subscribe(LibrarySubscription::Book { content_hash }).await?;
                Ok(WorkerDispatchResult::Subscription { initial, events: WorkerSubscription::Library(events) })
            }
        }
    }
}

impl From<WebAppCommand> for AppCommand {
    fn from(command: WebAppCommand) -> Self {
        Self::Host(command)
    }
}

#[cfg(feature = "web-runtime-tests")]
pub fn wire_contract() {
    // Extensions remain flat on the existing wire, and keep their typed shape
    // after MessagePack decoding. No target-specific wrapper leaks to callers.
    let command = crate::runtime::library_requests::ImportStagedBook { parent_id: crate::ROOT_DIR_ID, file_name: "Book.epub".into(), physical: "staged/book".into(), length: 12, hash: "a".repeat(64).parse().unwrap() };
    let json = serde_json::to_value(&command).unwrap();
    assert!(json.get("ImportStagedBook").is_some() && json.get("Host").is_none());
    let restored: LibraryCommand = decode_worker_message(&encode_worker_message(&command).unwrap()).unwrap();
    assert!(matches!(restored, LibraryCommand::ImportStagedBook { length: 12, .. }));
    let command = AppCommand::from(WebAppCommand::SubscribeBook { library_id: crate::LibraryId::from_u128(1), content_hash: "a".repeat(64).parse().unwrap() });
    let json = serde_json::to_value(&command).unwrap();
    assert!(json.get("SubscribeBook").is_some() && json.get("Host").is_none());
    let restored: AppCommand = decode_worker_message(&encode_worker_message(&command).unwrap()).unwrap();
    assert!(restored.is_book_subscription());
}

#[cfg(feature = "web-runtime-tests")]
pub fn subscription_contract() {
    wire_contract();
    let library_id = crate::LibraryId::from_u128(1);
    let content_hash = "a".repeat(64).parse().unwrap();
    let book = AppWorkerRequest { id: 1, command: WorkerCommand::LibrarySubscription { library_id, subscription: LibrarySubscription::Book { content_hash } } };
    let book: AppWorkerRequest = decode_worker_message(&encode_worker_message(&book).unwrap()).unwrap();
    assert!(book.is_resolve_book());
    assert!(book.is_source_subscription());
    assert!(book.command.is_book_subscription(), "book leases must not be replayed after reconnect");
    for subscription in [LibrarySubscription::Updates, LibrarySubscription::BookDownload { content_hash }] {
        let request = AppWorkerRequest { id: 2, command: WorkerCommand::LibrarySubscription { library_id, subscription } };
        let request: AppWorkerRequest = decode_worker_message(&encode_worker_message(&request).unwrap()).unwrap();
        assert!(!request.is_resolve_book());
        assert!(!request.is_source_subscription());
        assert!(!request.command.is_book_subscription(), "ordinary subscriptions must be restorable");
    }
}
