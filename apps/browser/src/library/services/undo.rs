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

use std::future::Future;

use library_backend::{BackendError, LibraryClient};
use sync_common::{BookId, DirId};

/// One reversal. Each names the operation that undoes a single change.
#[derive(Clone, Debug)]
pub(crate) enum UndoStep {
    /// Put a folder back under the parent it came from.
    MoveDirectory { directory: DirId, parent: DirId },
    /// Discard a folder that an action created — the copy a copy made.
    TrashDirectory { directory: DirId },
    /// Put a folder back that an action discarded.
    RestoreDirectory { directory: DirId, parent: Option<DirId> },
    /// Drop a placement an action created.
    RemoveBookFromDirectory { book: BookId, directory: DirId },
    /// Put back a placement an action removed.
    RestoreBookPlacement { book: BookId, directory: DirId },
    /// Rename a folder back.
    RenameDirectory { directory: DirId, name: String },
}

impl UndoStep {
    async fn apply(self, library: &LibraryClient) -> Result<(), BackendError> {
        match self {
            Self::MoveDirectory { directory, parent } => library.move_directory(directory, parent).await.map(|_| ()),
            Self::TrashDirectory { directory } => library.move_directory_to_trash(directory).await,
            Self::RestoreDirectory { directory, parent } => library.restore_directory(directory, parent).await.map(|_| ()),
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
    steps: Vec<UndoAction>,
}

#[derive(Clone)]
struct UndoAction {
    library: Option<LibraryClient>,
    step: UndoStep,
}
impl std::fmt::Debug for UndoAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("UndoAction").field("library", &self.library.as_ref().map(LibraryClient::id)).field("step", &self.step).finish()
    }
}

impl UndoEntry {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), steps: Vec::new() }
    }

    pub(crate) fn with(mut self, step: UndoStep) -> Self {
        self.push(step);
        self
    }

    pub(crate) fn push(&mut self, step: UndoStep) {
        self.steps.push(UndoAction { library: None, step });
    }

    /// Cross-library actions retain the backend that owns each inverse step.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn push_in(&mut self, library: &LibraryClient, step: UndoStep) {
        self.steps.push(UndoAction { library: Some(library.clone()), step });
    }

    /// Stop at the first failure, retaining undo for every completed item.
    pub(crate) async fn record<T, F, Fut>(&mut self, items: impl IntoIterator<Item = T>, mut apply: F) -> Result<(), BackendError>
    where
        F: FnMut(T) -> Fut,
        Fut: Future<Output = Result<UndoStep, BackendError>>,
    {
        for item in items {
            self.push(apply(item).await?);
        }
        Ok(())
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
#[derive(Default)]
pub(crate) struct UndoStack {
    entries: Vec<UndoEntry>,
}

impl UndoStack {
    pub(crate) fn push(&mut self, entry: UndoEntry) {
        if entry.is_empty() {
            return;
        }
        let entries = &mut self.entries;
        entries.push(entry);
        if entries.len() > UNDO_DEPTH {
            entries.remove(0);
        }
    }

    pub(crate) fn take(&mut self) -> Option<UndoEntry> {
        self.entries.pop()
    }

    /// Steps run in reverse: the last change made is the first undone, so a
    /// move that displaced something is put back before whatever displaced it.
    pub(crate) async fn apply(entry: UndoEntry, library: &LibraryClient) -> Result<(), BackendError> {
        for action in entry.steps.into_iter().rev() {
            action.step.apply(action.library.as_ref().unwrap_or(library)).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use futures_util::FutureExt;
    use sync_common::ROOT_DIR_ID;

    use super::{UndoEntry, UndoStack, UndoStep};

    #[test]
    fn partial_batch_retains_only_completed_steps_and_stops() {
        let mut entry = UndoEntry::new("Batch");
        let mut attempted = Vec::new();
        let result = entry
            .record(0..4, |item| {
                attempted.push(item);
                ready(if item == 2 { Err("failed".into()) } else { Ok(UndoStep::RenameDirectory { directory: ROOT_DIR_ID, name: item.to_string() }) })
            })
            .now_or_never()
            .unwrap();
        assert_eq!(result.unwrap_err().to_string(), "failed");
        assert_eq!(attempted, [0, 1, 2]);
        assert_eq!(entry.steps.len(), 2);
        assert!(matches!(&entry.steps[0].step, UndoStep::RenameDirectory { name, .. } if name == "0"));
        assert!(matches!(&entry.steps[1].step, UndoStep::RenameDirectory { name, .. } if name == "1"));
        let mut stack = UndoStack::default();
        stack.push(entry);
        assert_eq!(stack.take().unwrap().steps.len(), 2);
    }

    #[test]
    fn failure_before_any_change_does_not_add_an_undo_action() {
        let mut entry = UndoEntry::new("Batch");
        let result = entry.record([()], |_| ready(Err("failed".into()))).now_or_never().unwrap();
        assert!(result.is_err());
        let mut stack = UndoStack::default();
        stack.push(entry);
        assert!(stack.take().is_none());
    }

    #[test]
    fn successive_batches_preserve_earlier_undo_steps() {
        let mut entry = UndoEntry::new("Batch");
        for count in [2, 3] {
            entry.record(0..count, |_| ready(Ok(UndoStep::TrashDirectory { directory: ROOT_DIR_ID }))).now_or_never().unwrap().unwrap();
        }
        assert_eq!(entry.steps.len(), 5);
    }
}
