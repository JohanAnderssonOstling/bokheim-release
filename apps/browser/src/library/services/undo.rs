//! Undo for library organization.
//!
//! An entry is one *user action* — a menu command, a drop, a keystroke — and
//! holds the steps that put things back. Batch commands are one entry, not one
//! per item, because the reader performed one action and expects one undo.
//!
//! These are inverse operations, not a rollback: undoing a move is another
//! move. The library may have changed underneath in the meantime, so a step can
//! fail, and a folder can come back under a different name if its old one was
//! taken while it was away.

use std::cell::RefCell;
use std::rc::Rc;

use library_backend::LibraryClient;
use sync_common::{ContentHash, DirId};

/// One reversal. Each names the operation that undoes a single change.
#[derive(Clone, Debug)]
pub(crate) enum UndoStep {
    /// Put a folder back under the parent it came from.
    MoveDirectory { directory: DirId, parent: DirId },
    /// Discard a folder that an action created — the copy a copy made.
    TrashDirectory { directory: DirId },
    /// Put a folder back that an action discarded.
    RestoreDirectory { directory: DirId, parent: Option<DirId> },
    /// Put a book back in the folder it was moved out of.
    MoveBook { book: ContentHash, from: DirId, to: DirId },
    /// Drop a placement an action created.
    RemoveBookFromDirectory { book: ContentHash, directory: DirId },
    /// Put back a placement an action removed.
    RestoreBookPlacement { book: ContentHash, directory: DirId },
    /// Rename a folder back.
    RenameDirectory { directory: DirId, name: String },
}

impl UndoStep {
    async fn apply(self, library: &LibraryClient) -> Result<(), String> {
        match self {
            Self::MoveDirectory { directory, parent } => library.move_directory(directory, parent).await.map(|_| ()),
            Self::TrashDirectory { directory } => library.move_directory_to_trash(directory).await,
            Self::RestoreDirectory { directory, parent } => library.restore_directory(directory, parent).await.map(|_| ()),
            Self::MoveBook { book, from, to } => library.move_book_to_directory(book, from, to).await.map(|_| ()),
            Self::RemoveBookFromDirectory { book, directory } => library.remove_book_from_directory(book, directory).await.map(|_| ()),
            Self::RestoreBookPlacement { book, directory } => library.restore_book_placement(book, directory).await,
            Self::RenameDirectory { directory, name } => library.rename_directory(directory, name).await.map(|_| ()),
        }
    }
}

/// One user action's worth of reversal.
#[derive(Clone, Debug)]
pub(crate) struct UndoEntry {
    /// What was done, in the words the notification will use: "Moved 3 books".
    pub(crate) label: String,
    steps: Vec<UndoStep>,
}

impl UndoEntry {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), steps: Vec::new() }
    }

    pub(crate) fn with(mut self, step: UndoStep) -> Self {
        self.steps.push(step);
        self
    }

    pub(crate) fn push(&mut self, step: UndoStep) {
        self.steps.push(step);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// How many actions can be taken back. Deep enough to cover a session's worth
/// of tidying, shallow enough that an entry cannot name state from hours ago
/// that no longer resembles what is on screen.
const UNDO_DEPTH: usize = 32;

/// The stack of reversible actions, shared by every page of one library.
#[derive(Clone, Default)]
pub(crate) struct UndoStack {
    entries: Rc<RefCell<Vec<UndoEntry>>>,
}

impl UndoStack {
    pub(crate) fn push(&self, entry: UndoEntry) {
        if entry.is_empty() {
            return;
        }
        let mut entries = self.entries.borrow_mut();
        entries.push(entry);
        if entries.len() > UNDO_DEPTH {
            entries.remove(0);
        }
    }

    pub(crate) fn peek_label(&self) -> Option<String> {
        self.entries.borrow().last().map(|entry| entry.label.clone())
    }

    pub(crate) fn take(&self) -> Option<UndoEntry> {
        self.entries.borrow_mut().pop()
    }

    /// Steps run in reverse: the last change made is the first undone, so a
    /// move that displaced something is put back before whatever displaced it.
    pub(crate) async fn apply(entry: UndoEntry, library: &LibraryClient) -> Result<(), String> {
        for step in entry.steps.into_iter().rev() {
            step.apply(library).await?;
        }
        Ok(())
    }
}
