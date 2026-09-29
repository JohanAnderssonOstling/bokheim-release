//! Book files, verified staging, leases, and physical placement.
// Physical asset layout. The backend owns the corresponding library policy.
pub(crate) const APP_HIDDEN_DIR: &str = ".bokheim";
pub mod asset_store;
#[cfg(not(target_arch = "wasm32"))]
pub mod filesystem;
#[cfg(target_arch = "wasm32")]
pub mod hashing;
pub use asset_store::{AssetStore, AssetStoreError};

impl From<AssetStoreError> for client_runtime::BackendError {
    fn from(error: AssetStoreError) -> Self {
        Self::operation(error)
    }
}
