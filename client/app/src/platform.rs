//! Target-selected mechanisms. Shared storage and library policy live elsewhere.
#[cfg(target_arch = "wasm32")]
pub use client_platform_web::web_storage;

#[cfg(not(target_arch = "wasm32"))]
pub use client_platform_native::filesystem::{ImportReader as DirectoryImportReader, ImportReaderFuture as DirectoryImportReaderFuture, ImportSource};
#[cfg(target_arch = "wasm32")]
pub use client_platform_web::transport::import_io::{ImportReader as DirectoryImportReader, ImportReaderFuture as DirectoryImportReaderFuture, ImportSource};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use client_platform_native::reserve_database_capacity;
#[cfg(target_arch = "wasm32")]
pub(crate) use client_platform_web::reserve_database_capacity;
