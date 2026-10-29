//! Imports of files already stored by a caller. Completion uses the shared workflow.

use client_runtime::BackendError;
use library_placement::stage_import;
use sync_common::{BookId, DirId};

use crate::LibraryImport;

impl LibraryImport {
    pub async fn import_stored_book(&self, parent_id: DirId, file_name: String, location: std::path::PathBuf) -> Result<BookId, BackendError> {
        let mut import = self.validate_import(&parent_id, &file_name)?;
        let source = std::fs::File::open(&location)?;
        let (name, staged) = stage_import(&self.assets, &self.database, parent_id, import.file_name.clone(), Box::new(source), Default::default()).await.map_err(BackendError::operation)?;
        import.file_name = name;
        self.finish_import(import, staged).await
    }
}
