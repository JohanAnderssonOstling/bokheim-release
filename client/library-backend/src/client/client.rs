use crate::library::commands::LibraryReply;
#[cfg(not(target_arch = "wasm32"))]
use crate::BackendError;
use crate::LibraryId;

#[cfg(target_arch = "wasm32")]
use std::rc::Rc as Shared;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc as Shared;

/// A library-scoped client. The host supplies access without exposing application services.
#[derive(Clone)]
pub struct LibraryClient {
    library_id: LibraryId,
    pub(super) transport: Shared<dyn super::ClientTransport>,
}
impl LibraryClient {
    pub fn new(library_id: LibraryId, transport: impl super::ClientTransport + 'static) -> Self {
        Self { library_id, transport: Shared::new(transport) }
    }

    pub fn id(&self) -> &LibraryId {
        &self.library_id
    }

    pub(crate) async fn request<T: LibraryReply>(&self, command: crate::library::LibraryCommand) -> Result<T, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.transport.handle().map_err(|error| error.to_string())?.dispatch(command).await.and_then(client_runtime::reply::Reply::take).map_err(|error| error.to_string())
        }
        #[cfg(target_arch = "wasm32")]
        {
            client_runtime::wire::decode_worker_payload(self.transport.request(command).await?).map_err(|error| error.to_string())
        }
    }

    pub async fn updates(&self) -> Result<(async_channel::Receiver<library_model::LibraryUpdate>, bool), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let handle = self.transport.handle().map_err(|error| error.to_string())?;
            let events = handle.subscribe_updates();
            Ok((events, handle.scanning().await.map_err(|error| error.to_string())?))
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.transport.updates().await
        }
    }

    pub async fn download_changes(&self, content_hash: crate::ContentHash) -> Result<(async_channel::Receiver<crate::DownloadState>, crate::DownloadState), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.transport.handle().map_err(|error| error.to_string())?.download_changes(content_hash).await.map_err(|error| error.to_string())
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.transport.download_changes(content_hash).await
        }
    }
}

/// Native hosts retain their worker lifetime while exposing the already-open library actor.
#[cfg(not(target_arch = "wasm32"))]
pub trait ClientTransport: Send + Sync {
    fn handle(&self) -> Result<&crate::library::LibraryHandle, BackendError>;
}
#[cfg(not(target_arch = "wasm32"))]
impl ClientTransport for crate::library::LibraryHandle {
    fn handle(&self) -> Result<&crate::library::LibraryHandle, BackendError> {
        Ok(self)
    }
}

#[cfg(target_arch = "wasm32")]
use crate::executor::BoxedBackendFuture;
#[cfg(target_arch = "wasm32")]
pub type TransportResult<'a, T> = BoxedBackendFuture<'a, Result<T, String>>;

/// Browser hosts route library requests and subscriptions over their worker transport.
#[cfg(target_arch = "wasm32")]
pub trait ClientTransport {
    fn request(&self, command: crate::library::LibraryCommand) -> TransportResult<'_, client_runtime::wire::WorkerPayload>;
    fn updates(&self) -> TransportResult<'_, (async_channel::Receiver<library_model::LibraryUpdate>, bool)>;
    fn download_changes(&self, content_hash: crate::ContentHash) -> TransportResult<'_, (async_channel::Receiver<crate::DownloadState>, crate::DownloadState)>;
    fn subscribe_book(&self, content_hash: crate::ContentHash) -> TransportResult<'_, (async_channel::Receiver<()>, crate::ResolvedBookData)>;
    fn import_directory(&self, parent_id: crate::DirId, directory: crate::DirectoryImport, create_root: bool) -> TransportResult<'_, Vec<crate::ImportFailure>>;
    fn diagnostics(&self) -> async_channel::Receiver<String>;
}
