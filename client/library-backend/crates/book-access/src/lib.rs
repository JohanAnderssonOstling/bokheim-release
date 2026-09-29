//! Bounded, revision-pinned book reads. Callers supply authenticated fetches.
pub use sync_common::ContentHash;
#[cfg(feature = "pdf")]
pub mod pdf_bundle;
pub mod range_cache;
pub mod remote_file;
use client_platform_runtime::executor as time;
pub use remote_file::{RemoteFile, RemoteFileError};

pub trait BookReader: std::io::Read + std::io::Seek + Send + Sync {}
impl<T: std::io::Read + std::io::Seek + Send + Sync> BookReader for T {}
pub type BoxedBookReader = Box<dyn BookReader>;
pub type BundleFetch = Box<dyn FnMut(Vec<(u64, usize)>) -> std::io::Result<Vec<u8>> + Send + Sync>;
#[cfg(not(target_arch = "wasm32"))]
pub use futures_util::future::BoxFuture as FetchFuture;
#[cfg(target_arch = "wasm32")]
pub use futures_util::future::LocalBoxFuture as FetchFuture;

#[cfg(feature = "sqlite-cache")]
pub mod cache;
#[cfg(feature = "sqlite-cache")]
pub mod storage;
