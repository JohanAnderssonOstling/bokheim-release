//! Process-local services owned by concrete library sessions.

pub mod account;
pub mod events;
pub mod interest;
pub mod scheduler;
pub mod transfer;
pub mod transfer_history;

pub use client_platform_runtime::executor;
pub use client_platform_runtime::storage_queue;

#[cfg(not(target_arch = "wasm32"))]
pub use client_platform_native::sqlite;
#[cfg(target_arch = "wasm32")]
pub use client_platform_web::sqlite;

use client_runtime::BackendError;
pub use transfer::{ClaimedTransferJob, HistoryPersistence, LibraryTransfers, MessagePackTransferHistory, TransferJob, TransferOperation, TransferOrigin, TransferProducer, TransferQueue, TransferState, TransferStatus};
