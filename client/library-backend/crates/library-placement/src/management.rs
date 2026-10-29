//! User-initiated library organization backed by the shared SQLite model.
use client_runtime::BackendError;
use library_model::Dir;
use library_replica::filenames;
use sync_common::{BookId, DirId};

use crate::LibraryPlacement;

impl LibraryPlacement {
    pub fn create_directory(&self, parent_id: &DirId, name: &str) -> Result<Dir, BackendError> {
        let name = filenames::validate_component(name.trim())?;
        #[cfg(not(target_arch = "wasm32"))]
        let created = if self.assets.local_access_enabled() {
            let id = DirId::new_v4();
            let (_, path) = self.database.directory_creation_path(parent_id, &name)?;
            let chosen = self.assets.create_directory(&path, id).map_err(BackendError::operation)?;
            let parent = path.rsplit_once('/').map(|(parent, _)| parent);
            let path = parent.map_or_else(|| chosen.clone(), |parent| format!("{parent}/{chosen}"));
            self.database.register_created_directory(&id, parent_id, &chosen, &path)?
        } else {
            self.database.create_directory(parent_id, &name)?
        };
        #[cfg(target_arch = "wasm32")]
        let created = self.database.create_directory(parent_id, &name)?;
        self.events.notify_contents_changed();
        Ok(created)
    }

    /// Removes one placement. Returns `true` when the final placement
    /// is removed and the book consequently moves to Trash.
    pub fn remove_book_from_directory(&self, content_hash: &BookId, source_id: &DirId) -> Result<bool, BackendError> {
        let orphaned = self.database.remove_book_placement(content_hash, source_id)?;
        self.events.notify_contents_changed();
        Ok(orphaned)
    }

    pub fn move_book_to_trash(&self, content_hash: &BookId) -> Result<(), BackendError> {
        self.database.trash_book(content_hash)?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn restore_book(&self, content_hash: &BookId) -> Result<(), BackendError> {
        self.database.restore_book(content_hash)?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn purge_book(&self, content_hash: &BookId) -> Result<(), BackendError> {
        self.database.purge_book(content_hash)?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn purge_directory(&self, directory_id: &DirId) -> Result<(), BackendError> {
        self.database.purge_directory(directory_id)?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn empty_trash(&self) -> Result<(), BackendError> {
        self.database.empty_trash()?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn move_directory_to_trash(&self, directory_id: &DirId) -> Result<(), BackendError> {
        self.database.trash_directory(directory_id)?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn restore_directory(&self, directory_id: &DirId, requested_parent: Option<&DirId>) -> Result<String, BackendError> {
        let name = self.database.restore_directory(directory_id, requested_parent)?;
        self.events.notify_contents_changed();
        Ok(name)
    }

    /// Copies placements into a second directory without duplicating book bytes.
    pub fn copy_directory_metadata(&self, directory_id: &DirId, destination_parent_id: &DirId) -> Result<DirId, BackendError> {
        let copy = self.database.copy_directory(directory_id, destination_parent_id)?;
        self.events.notify_contents_changed();
        Ok(copy)
    }

    pub fn restore_book_placement_metadata(&self, content_hash: &BookId, directory_id: &DirId) -> Result<(), BackendError> {
        self.database.restore_book_placement(content_hash, directory_id)?;
        self.events.notify_contents_changed();
        Ok(())
    }

    pub fn queue_directory_move_metadata(&self, directory_id: &DirId, requested_parent: Option<&DirId>, requested_name: Option<&str>) -> Result<String, BackendError> {
        let requested_name = requested_name.map(filenames::validate_component).transpose()?;
        let name = self.database.move_directory(directory_id, requested_parent, requested_name.as_deref())?;
        self.events.notify_contents_changed();
        Ok(name)
    }

    /// Returns false for an existing assignment so callers retain it when undoing.
    pub fn copy_book_to_directory_metadata(&self, hash: &BookId, source: Option<&DirId>, destination: &DirId) -> Result<bool, BackendError> {
        let created = self.database.copy_book_to_directory(hash, source, destination)?;
        if created {
            self.events.notify_contents_changed();
        }
        Ok(created)
    }

    pub fn move_book_placement_metadata(&self, hash: &BookId, source: &DirId, destination: &DirId) -> Result<bool, BackendError> {
        let created = self.database.move_book_placement(hash, source, destination)?;
        self.events.notify_contents_changed();
        Ok(created)
    }
}
