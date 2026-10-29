//! GPUI state scoped to the selected library.

mod context;
mod undo;
mod update_bridge;

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
pub(crate) use context::feed_detail;
pub(crate) use context::{LibraryContext, OpenBook};
pub(crate) use undo::{UndoEntry, UndoStack, UndoStep};
pub(crate) use update_bridge::{DownloadSubscription, LibraryContentsChanged, LibraryCoversChanged, LibraryDownloadChanged, LibraryScanChanged, LibraryUpdateBridge, initial_download_state};

/// How selected books or folders are placed in their chosen destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderPlacement {
    Copy,
    Move,
}
