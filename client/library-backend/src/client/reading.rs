use super::LibraryClient;
use crate::library::commands::library_requests;

impl LibraryClient {
    pub async fn audiobook_playback_metadata(&self, content_hash: crate::ContentHash) -> Result<Option<crate::library::PlaybackMetadata>, String> {
        self.request(library_requests::AudiobookPlaybackMetadata { content_hash }).await
    }
    pub async fn reading_position(&self, content_hash: crate::ContentHash) -> Result<Option<String>, String> {
        self.request(library_requests::ReadingPosition { content_hash }).await
    }
    pub async fn pdf_reading_position(&self, content_hash: crate::ContentHash) -> Result<Option<(u32, f32)>, String> {
        self.request(library_requests::PdfReadingPosition { content_hash }).await
    }
    pub async fn update_reading_position(&self, content_hash: crate::ContentHash, position: String, progress: Option<f32>, entry: Option<String>) -> Result<(), String> {
        self.request(library_requests::UpdateReadingPosition { content_hash, position, progress, entry }).await
    }
    pub async fn annotations(&self, content_hash: crate::ContentHash) -> Result<Vec<book_model::ReaderAnnotation>, String> {
        self.request(library_requests::Annotations { content_hash }).await
    }
    pub async fn upsert_annotation(&self, annotation: book_model::ReaderAnnotation) -> Result<(), String> {
        self.request(library_requests::UpsertAnnotation { annotation }).await
    }
    pub async fn delete_annotation(&self, annotation_id: String, modified_at: i64) -> Result<(), String> {
        self.request(library_requests::DeleteAnnotation { annotation_id, modified_at }).await
    }
}
