//! Native library-handle wrappers for shared annotation persistence.

use crate::library::LibrarySession;
use crate::BackendError;
use crate::ContentHash;
use book_model::ReaderAnnotation;

impl LibrarySession {
    pub fn annotations(&self, content_hash: ContentHash) -> Result<Vec<ReaderAnnotation>, BackendError> {
        self.db.annotations(content_hash).map_err(BackendError::operation)
    }

    pub fn upsert_annotation(&self, annotation: &ReaderAnnotation) -> Result<(), BackendError> {
        self.db.upsert_annotation(annotation).map_err(BackendError::operation)?;
        Ok(())
    }

    pub fn delete_annotation(&self, annotation_id: &str, modified_at: i64) -> Result<(), BackendError> {
        self.db.delete_annotation(annotation_id, modified_at).map_err(BackendError::operation)?;
        Ok(())
    }
}
