//! Host interfaces for SQLite scheduling and background cache writes.
use std::{fmt, path::Path};

#[cfg(not(target_arch = "wasm32"))]
pub trait CacheSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> CacheSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait CacheSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> CacheSend for T {}

#[derive(Clone, Copy)]
pub enum CachePriority {
    Interactive,
    Background,
}
#[derive(Debug)]
pub struct CacheError(String);
impl CacheError {
    pub fn operation(error: impl fmt::Display) -> Self {
        Self(error.to_string())
    }
}
impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for CacheError {}
impl From<rusqlite::Error> for CacheError {
    fn from(error: rusqlite::Error) -> Self {
        Self::operation(error)
    }
}

pub trait CacheStorage: Clone + fmt::Debug + CacheSend + Sync + 'static {
    fn open(path: &Path, initialize: fn(&rusqlite::Connection) -> rusqlite::Result<()>) -> Result<Self, CacheError>;
    fn command<T: CacheSend + 'static>(&self, priority: CachePriority, operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T, CacheError> + CacheSend + 'static) -> crate::FetchFuture<'static, Result<T, CacheError>>;
}
pub trait CacheExecutor {
    fn spawn_detached(&self, future: crate::FetchFuture<'static, ()>);
}

impl CacheExecutor for client_platform_runtime::executor::BackendExecutor {
    fn spawn_detached(&self, future: crate::FetchFuture<'static, ()>) {
        client_platform_runtime::executor::BackendExecutor::spawn_detached(self, future);
    }
}
