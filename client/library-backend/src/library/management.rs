//! User-initiated library organization backed by the shared SQLite model.
use crate::BackendError;

use super::{filenames, LibrarySession};
use crate::{ContentHash, DirId};
use library_model::{LibraryFolderDestination, LibraryTrashView};

impl LibrarySession {
    pub async fn rename_directory(&self, directory_id: &DirId, requested_name: &str) -> Result<String, BackendError> {
        self.move_library_directory(directory_id, None, Some(requested_name)).await
    }

    pub async fn move_directory(&self, directory_id: &DirId, parent_id: &DirId) -> Result<String, BackendError> {
        self.move_library_directory(directory_id, Some(parent_id), None).await
    }

    /// Duplicates a folder and its contents under another folder. The books it
    /// holds gain a second placement; their bytes are not duplicated. Returns
    /// the copy's directory ID, which is what an undo of this acts on.
    pub fn copy_directory(&self, directory_id: &DirId, destination_parent_id: &DirId) -> Result<DirId, BackendError> {
        let copy = self.db.copy_directory(directory_id, destination_parent_id)?;
        self.notify_contents_changed();
        Ok(copy)
    }

    /// Puts a book back in a folder it was removed from.
    pub fn restore_book_placement(&self, content_hash: &ContentHash, directory_id: &DirId) -> Result<(), BackendError> {
        self.db.restore_book_placement(content_hash, directory_id)?;
        self.notify_contents_changed();
        Ok(())
    }

    async fn move_library_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>, requested_name: Option<&str>) -> Result<String, BackendError> {
        let name = self.queue_directory_move(directory_id, requested_parent, requested_name)?;
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(error) = self.assets.placement().prepare_directory_with_database(&self.db, *directory_id).await {
            log::warn!("directory rename will retry from the durable queue: {error}");
        }
        Ok(name)
    }

    /// The logical mutation is ordered; physical replay is durable background
    /// work and must not hold up later commands or reader events.
    pub(super) fn queue_directory_move(&self, directory_id: &DirId, requested_parent: Option<&DirId>, requested_name: Option<&str>) -> Result<String, BackendError> {
        let requested_name = requested_name.map(filenames::validate_component).transpose()?;
        let name = self.db.move_directory(directory_id, requested_parent, requested_name.as_deref())?;
        self.notify_contents_changed();
        Ok(name)
    }

    pub fn copy_book_to_directory(&self, content_hash: &ContentHash, source_id: &DirId, destination_id: &DirId) -> Result<String, BackendError> {
        self.transfer_book_placement(content_hash, source_id, destination_id, false)
    }

    pub fn move_book_to_directory(&self, content_hash: &ContentHash, source_id: &DirId, destination_id: &DirId) -> Result<String, BackendError> {
        self.transfer_book_placement(content_hash, source_id, destination_id, true)
    }

    fn transfer_book_placement(&self, hash: &ContentHash, source: &DirId, destination: &DirId, moving: bool) -> Result<String, BackendError> {
        let name = self.db.transfer_book_placement(hash, source, destination, moving)?;
        self.notify_contents_changed();
        Ok(name)
    }

    /// Removes one placement. Returns `true` when the final placement
    /// is removed and the book consequently moves to Trash.
    pub fn remove_book_from_directory(&self, content_hash: &ContentHash, source_id: &DirId) -> Result<bool, BackendError> {
        let orphaned = self.db.remove_book_placement(content_hash, source_id)?;
        self.notify_contents_changed();
        Ok(orphaned)
    }

    pub fn move_book_to_trash(&self, content_hash: &ContentHash) -> Result<(), BackendError> {
        self.db.trash_book(content_hash)?;
        self.notify_contents_changed();
        Ok(())
    }

    pub fn restore_book(&self, content_hash: &ContentHash) -> Result<(), BackendError> {
        self.assets.ensure_restore_available().map_err(BackendError::operation)?;
        #[cfg(not(target_arch = "wasm32"))]
        let placement = self.assets.placement();
        let mut choices = Vec::new();
        for candidate in self.db.restore_book_plan(content_hash).map_err(BackendError::operation)? {
            // First live directory walking up that still exists on disk; the
            // root needs no check, matching the previous walk. Browser
            // storage has no out-of-band deleter, so liveness alone decides.
            let mut target = sync_common::ROOT_DIR_ID;
            for ancestor in &candidate.ancestry {
                if !ancestor.live {
                    continue;
                }
                #[cfg(not(target_arch = "wasm32"))]
                if !placement.directory_available(&ancestor.path) {
                    continue;
                }
                target = ancestor.id;
                break;
            }
            choices.push((candidate.original, target));
        }
        self.db.restore_book_commit(content_hash, &choices).map_err(BackendError::operation)?;
        self.notify_contents_changed();
        Ok(())
    }

    pub(crate) fn trash(&self) -> Result<LibraryTrashView, BackendError> {
        Ok(self.db.library_trash()?)
    }

    pub(crate) fn folder_destinations(&self) -> Result<Vec<LibraryFolderDestination>, BackendError> {
        Ok(self.db.folder_destinations()?)
    }

    pub fn purge_book(&self, content_hash: &ContentHash) -> Result<(), BackendError> {
        self.db.purge_book(content_hash)?;
        self.notify_contents_changed();
        Ok(())
    }

    pub fn purge_directory(&self, directory_id: &DirId) -> Result<(), BackendError> {
        self.db.purge_directory(directory_id)?;
        self.notify_contents_changed();
        Ok(())
    }

    pub fn empty_trash(&self) -> Result<(), BackendError> {
        self.db.empty_trash()?;
        self.notify_contents_changed();
        Ok(())
    }

    pub fn move_directory_to_trash(&self, directory_id: &DirId) -> Result<(), BackendError> {
        self.db.trash_directory(directory_id)?;
        self.notify_contents_changed();
        Ok(())
    }

    pub fn restore_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>) -> Result<String, BackendError> {
        let name = self.db.restore_directory(directory_id, requested_parent)?;
        self.notify_contents_changed();
        Ok(name)
    }
}
