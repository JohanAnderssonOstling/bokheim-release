//! Thread-confined access to the application backend.
//!
//! Browser hosts keep SQLite and its synchronous OPFS VFS behind an
//! origin-wide shared worker and use a serialized RPC protocol. Native hosts
//! send typed jobs to one application-owned backend thread.

use crate::app::AppBackend;
use std::fmt;
use sync_common::LibraryId;
use tokio::sync::oneshot;

pub(crate) mod dispatch;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::Transport;
mod protocol;
pub use crate::library::LibraryCommand;
pub(crate) mod requests;
pub(crate) use crate::library::commands::library_requests;
pub(crate) use crate::library::commands::LibraryReply;
pub use requests::AppCommand;
#[cfg(target_arch = "wasm32")]
pub(crate) mod web;
#[cfg(target_arch = "wasm32")]
pub(crate) use web::Transport;

use crate::api::AppClient;
pub use crate::library::LibraryBrowseData;
pub use dispatch::{app_startup_state, dispatch_worker_request};
pub use protocol::*;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(target_arch = "wasm32")]
mod host;
#[cfg(target_arch = "wasm32")]
pub use host::BackendWorkerHost;

#[cfg(any(target_arch = "wasm32", test))]
pub mod coordinator_lifecycle;

// Shared subscription API; transports own delivery and cancellation.
impl Transport {
    pub(crate) async fn notification_interest_transport(&self, browsing: bool) -> Result<async_channel::Receiver<()>, crate::BackendError> {
        self.unit_subscription(AppCommand::SubscribeNotifications { browsing }).await
    }
    pub(crate) async fn library_list_changes_transport(&self) -> Result<async_channel::Receiver<()>, crate::BackendError> {
        self.unit_subscription(AppCommand::SubscribeLibraryList).await
    }
}

#[cfg(target_arch = "wasm32")]
pub use crate::web::requests::WebAppCommand as HostAppCommand;
#[cfg(not(target_arch = "wasm32"))]
pub use native::NoHostCommand as HostAppCommand;
