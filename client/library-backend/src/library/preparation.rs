//! Upload admission is synchronous; optional enrichment has its own background lane.
use crate::library::LibrarySession;
use crate::BackendError;

impl LibrarySession {
    pub(crate) fn admit_uploads(&self) -> Result<(), BackendError> {
        if self.scanner_running() {
            return Err(BackendError::message("waiting for library scan"));
        }
        let snapshot = self.db.upload_preparation_snapshot().map_err(BackendError::operation)?;
        // Only durable local file work may delay admission. Thumbnail creation
        // and remote metadata providers are optional follow-up work: neither
        // must hold the book bytes or its lifecycle record off the server.
        let batch = self.db.prepared_upload_batch(&snapshot.hashes()).map_err(BackendError::operation)?;
        self.db.finish_upload_preparation(&batch).map_err(BackendError::operation)?;
        self.sync_wake.wake();
        self.asset_wake.wake();

        Ok(())
    }

    pub(crate) async fn run_enrichment(&self) -> Result<(), BackendError> {
        let snapshot = self.db.upload_preparation_snapshot().map_err(BackendError::operation)?;
        let scope = self.db.prepared_upload_batch(&snapshot.hashes()).map_err(BackendError::operation)?.hashes();
        if let Some(sync) = self.sync() {
            sync.record_remote_thumbnail_presence(scope.iter().copied().collect()).await.map_err(BackendError::operation)?;
        }
        if self.assets.local_access_enabled() {
            // Filesystem replay has its own actor job; slow inspection and
            // network enrichment must not delay subsequent directory moves.
            // Freeze membership before preparation. Newly discovered files wait
            // for the next batch, including files added during slow inspection.
            let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
            let members = scope.clone();
            let thumbnails = self.db.upload_thumbnail_candidates(&members, now).map_err(BackendError::operation)?;
            let _thumbnails = if thumbnails.is_empty() { None } else { Some(self.begin_thumbnail_work().ok_or_else(|| BackendError::message("local inspection is already running"))?) };
            if let Err(error) = self.run_thumbnail_members(thumbnails).await {
                log::warn!("some local inspection will retry: {error}");
            }
        }
        // Audible identifiers may feed the ebook metadata phases. Each recording
        // is attempted at most once; unavailable lookups remain durably retryable
        // and never affect upload admission.
        self.run_audible_batch(&scope).await?;
        let updated = self.enrich_library_batch(&scope).await?;
        if updated > 0 {
            self.sync_wake.wake();
        }
        Ok(())
    }
}
