//! Host-owned asynchronous hashing; browser work stays on the CPU worker.
use client_platform_runtime::executor::BoxedBackendFuture;
/// Incremental BLAKE3 hashing. Dropping a session must release worker resources.
pub trait HashSession {
    fn update<'a>(&'a mut self, bytes: &'a [u8]) -> BoxedBackendFuture<'a, Result<(), crate::asset_store::AssetStoreError>>;
    fn finish(self: Box<Self>) -> BoxedBackendFuture<'static, Result<String, crate::asset_store::AssetStoreError>>;
}
/// Starts independently owned sessions; the returned digest is hexadecimal BLAKE3.
pub trait HashService: std::fmt::Debug + Send + Sync {
    fn start(&self) -> BoxedBackendFuture<'static, Result<Box<dyn HashSession>, crate::asset_store::AssetStoreError>>;
}
