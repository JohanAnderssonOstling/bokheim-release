//! Native file and granted-reader import entry points. Completion uses the shared workflow.
use super::LibrarySession;
use crate::BackendError;
use crate::{ContentHash, DirId};
use std::io::Read;

impl LibrarySession {
    pub(crate) async fn import_book_reader_with_progress(&self, parent_id: &DirId, source: Box<dyn Read + Send>, file_name: String, direct: bool, progress: crate::asset_store::ImportProgressObserver) -> Result<ContentHash, BackendError> {
        let mut import = self.validate_import(parent_id, &file_name)?;
        let relative = self.prepare_import_path(*parent_id, import.file_name.clone()).await?;
        let staged = if direct {
            let occupied = self.db.shared_occupied_file_names(parent_id)?;
            let (name, staged) = self.assets.copy_book_with_progress(relative, source, occupied, Some(progress.clone())).await.map_err(BackendError::operation)?;
            import.file_name = name;
            staged
        } else {
            self.assets.stage_book_with_progress(relative, source, Some(progress.clone())).await.map_err(BackendError::operation)?
        };
        self.finish_import_with_progress(import, staged, Some(progress)).await
    }

    pub(crate) async fn import_book_reader_direct(&self, parent_id: &DirId, source: Box<dyn Read + Send>, file_name: String) -> Result<ContentHash, BackendError> {
        let mut import = self.validate_import(parent_id, &file_name)?;
        let relative = self.prepare_import_path(*parent_id, import.file_name.clone()).await?;
        let occupied = self.db.shared_occupied_file_names(parent_id)?;
        let (name, copied) = self.assets.copy_book(relative, source, occupied).await.map_err(BackendError::operation)?;
        import.file_name = name;
        self.finish_import(import, copied).await
    }

    pub(super) async fn prepare_import_path(&self, parent: DirId, name: String) -> Result<String, BackendError> {
        self.assets.prepare_import_path(&self.db, parent, name).await.map_err(BackendError::operation)
    }

    pub async fn import_book_path(&self, parent_id: &DirId, file_name: String, path: &std::path::Path) -> Result<ContentHash, BackendError> {
        let import = self.validate_import(parent_id, &file_name)?;
        let relative = self.prepare_import_path(*parent_id, import.file_name.clone()).await?;
        let source = std::fs::File::open(path)?;
        let staged = self.assets.stage_book(relative, Box::new(source)).await.map_err(BackendError::operation)?;
        std::fs::remove_file(path)?;
        self.finish_import(import, staged).await
    }
}

/// Native import lifetime stays with its backend job, including in-flight writes.
pub(crate) struct DirectoryImportActivity<'a> {
    library: &'a LibrarySession,
    pub(crate) id: uuid::Uuid,
}

impl Drop for DirectoryImportActivity<'_> {
    fn drop(&mut self) {
        self.library.finish_directory_import(self.id);
    }
}

impl LibrarySession {
    pub(crate) fn directory_import_activity(&self) -> DirectoryImportActivity<'_> {
        self.directory_import_activity_with_id(uuid::Uuid::new_v4())
    }

    pub(crate) fn directory_import_activity_with_id(&self, id: uuid::Uuid) -> DirectoryImportActivity<'_> {
        self.begin_directory_import(id);
        DirectoryImportActivity { library: self, id }
    }
}
