//! Native library-handle wrappers for shared reading-position persistence.

use crate::library::playback::PlaybackMetadata;
use crate::library::LibrarySession;
use crate::BackendError;
use crate::ContentHash;

impl LibrarySession {
    pub(crate) fn book_format(&self, hash: ContentHash) -> Result<Option<book_model::BookFormat>, BackendError> {
        self.db.book_format(&hash).map_err(BackendError::operation)
    }

    pub(crate) fn cached_audiobook_metadata(&self, hash: ContentHash) -> Result<Option<PlaybackMetadata>, String> {
        let snapshot = self.db.audiobook_playback_snapshot(&hash).map_err(|error| error.to_string())?;
        let tracks = self.db.audiobook_tracks(&hash).map_err(|error| error.to_string())?;
        let format = self.db.book_format(&hash).map_err(|error| error.to_string())?.unwrap_or(book_model::BookFormat::M4b);
        Ok(snapshot.cached.map(|metadata| PlaybackMetadata { format, title: snapshot.title, author: snapshot.author, narrator: metadata.narrator, duration_ms: metadata.duration_ms, chapters: metadata.chapters, tracks, cover: Vec::new() }))
    }

    pub(crate) async fn audiobook_playback_metadata(&self, hash: ContentHash) -> Result<Option<PlaybackMetadata>, String> {
        let mut metadata = self.cached_audiobook_metadata(hash)?;
        if let Some(metadata) = &mut metadata {
            metadata.cover = match self.assets.read_bytes(crate::BlobKind::Thumbnail, &hash).await {
                Ok(cover) => cover.unwrap_or_default(),
                Err(error) => {
                    log::warn!("Cached audiobook cover unavailable: {error}");
                    Vec::new()
                }
            };
        }
        Ok(metadata)
    }
    pub fn reading_position(&self, content_hash: ContentHash) -> Result<Option<String>, BackendError> {
        self.db.reading_position(&content_hash).map_err(BackendError::operation)
    }

    pub fn pdf_reading_position(&self, content_hash: ContentHash) -> Result<Option<(u32, f32)>, BackendError> {
        self.db.pdf_reading_position(&content_hash).map_err(BackendError::operation)
    }

    /// `entry` is the navigation entry the reader resolved the position to. It
    /// travels with the position because that is when it changes, and it is
    /// stored locally rather than synchronized: any device can recompute it
    /// from the position and the book's own navigation.
    pub fn update_reading_position(&self, content_hash: ContentHash, position: &str, progress: Option<f32>, entry: Option<&str>) -> Result<(), BackendError> {
        self.db.update_reading_position(&content_hash, position, progress, entry).map_err(BackendError::operation)
    }
}
