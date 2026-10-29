//! Book operations and their failure wording, shared by overlays and selections.
//!
//! Book commands run without asking first: moving to Trash and removing from a
//! folder can both be undone from Trash, so a prompt only slowed down the
//! common case.

use ui_components::user_facing_error;

use gpui::{App, Window};
use library_backend::{BackendError, LibraryClient};
use library_model::{BookCardRow, BookLocator};
use sync_common::{BookId, DirId, LibraryId};

use super::notify_error;

#[derive(Clone, Copy)]
pub(crate) enum BookCommand {
    RemoveFromFolder(DirId),
    TrashBook,
    TrashBooks,
    RemoveDownload,
    SetRead(bool),
}

impl BookCommand {
    pub(crate) async fn execute(self, library: &LibraryClient, books: Vec<BookId>) -> Result<(), BackendError> {
        for book in books {
            match self {
                Self::RemoveFromFolder(directory) => {
                    library.remove_book_from_directory(book, directory).await?;
                }
                Self::TrashBook | Self::TrashBooks => {
                    library.move_book_to_trash(book).await?;
                }
                Self::RemoveDownload => {
                    library.remove_local_book_copy(book).await?;
                }
                Self::SetRead(read) => library.set_book_read(book, read).await?,
            }
        }
        Ok(())
    }

    pub(crate) fn report_failure(self, error: BackendError, window: &mut Window, cx: &mut App) {
        let (id, prefix) = match self {
            Self::RemoveFromFolder(_) => ("book-remove-error", "Could not remove book from folder"),
            Self::TrashBook => ("book-trash-error", "Could not move book to Trash"),
            Self::TrashBooks => ("book-trash-error", "Could not move selected books to Trash"),
            Self::RemoveDownload => ("book-evict-error", "Could not remove download"),
            Self::SetRead(true) => ("book-read-error", "Could not mark the book read"),
            Self::SetRead(false) => ("book-read-error", "Could not mark the book unread"),
        };
        notify_error(id, format!("{prefix}. {}", user_facing_error(&error, "Please try again.")), window, cx);
    }
}

pub(crate) fn open_book_windows(library_id: LibraryId, books: &[BookCardRow], window: &mut Window, cx: &mut App) {
    for book in books {
        if let Some(hash) = book.library_file() {
            let target = crate::NewWindowTarget::Book { locator: BookLocator::new(library_id, hash), title: book.title.clone() };
            window.dispatch_action(Box::new(crate::OpenInNewWindow(target)), cx);
        }
    }
}
