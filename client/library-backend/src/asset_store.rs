//! Host adapters for library-owned filesystem services.
#[cfg(target_arch = "wasm32")]
use crate::executor::BoxedBackendFuture;
pub use library_files::asset_store::*;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn open(locator: &str) -> Result<AssetStore, AssetStoreError> {
    AssetStore::open(locator)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn open(locator: &str, cpu: std::sync::Arc<dyn cpu_host::CpuHost>) -> Result<AssetStore, AssetStoreError> {
    AssetStore::open(locator, std::sync::Arc::new(WorkerHashService(cpu)))
}

#[cfg(target_arch = "wasm32")]
struct WorkerHashService(std::sync::Arc<dyn cpu_host::CpuHost>);
#[cfg(target_arch = "wasm32")]
impl std::fmt::Debug for WorkerHashService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WorkerHashService").finish_non_exhaustive()
    }
}
#[cfg(target_arch = "wasm32")]
impl library_files::hashing::HashService for WorkerHashService {
    fn start(&self) -> BoxedBackendFuture<'static, Result<Box<dyn library_files::hashing::HashSession>, AssetStoreError>> {
        let host = self.0.clone();
        Box::pin(async move {
            let session = cpu_host::RemoteHashSession::new(&host).await.map_err(AssetStoreError::operation)?;
            Ok(Box::new(WorkerHashSession(session)) as Box<dyn library_files::hashing::HashSession>)
        })
    }
}
#[cfg(target_arch = "wasm32")]
struct WorkerHashSession(cpu_host::RemoteHashSession);
#[cfg(target_arch = "wasm32")]
impl library_files::hashing::HashSession for WorkerHashSession {
    fn update<'a>(&'a mut self, bytes: &'a [u8]) -> BoxedBackendFuture<'a, Result<(), AssetStoreError>> {
        Box::pin(async move { self.0.update(bytes).await.map_err(AssetStoreError::operation) })
    }
    fn finish(self: Box<Self>) -> BoxedBackendFuture<'static, Result<String, AssetStoreError>> {
        Box::pin(async move { self.0.finish().await.map_err(AssetStoreError::operation) })
    }
}
