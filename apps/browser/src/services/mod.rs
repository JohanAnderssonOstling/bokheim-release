//! Explicit service boundaries for browser UI code.

mod app;
pub(crate) mod updates;
pub use updates::{UpdateAction, UpdateActionHandler, UpdateControls, UpdateView};
mod format;
pub(crate) use format::format_storage_bytes;
pub(crate) mod library_removal;
pub use library_removal::{LibraryRemovalHook, LibraryRemovalPreparation, library_is_being_removed};
pub(crate) type UiFuture<T> = std::pin::Pin<Box<dyn Future<Output = Result<T, String>> + 'static>>;
mod import_presentation;
pub(crate) mod import_progress;
mod notifications;
pub(crate) use import_presentation::import_failure_message;
pub(crate) use import_progress::ImportProgressToast;

pub(crate) use notifications::{notify_error, notify_info, notify_undoable, notify_warning};

#[cfg(feature = "kobo")]
pub use app::SaveKoboAutoRotate;
pub use app::{AppServices, AppStartup};

pub(crate) mod library_add_trace;
