//! Explicit file-operation and thumbnail jobs, also available while signed out.
use super::LibrarySession;
use crate::asset_workflow::AssetPlacementWorkflow;
use crate::library::events::LibraryEvent;
use crate::BackendError;
use crate::ContentHash;

impl LibrarySession {
    pub(crate) fn pending_local_jobs(&self) -> Result<bool, BackendError> {
        Ok(self.pending_file_jobs()? || self.pending_thumbnail_jobs()?)
    }

    pub(crate) fn pending_thumbnail_jobs(&self) -> Result<bool, BackendError> {
        Ok(self.db.pending_thumbnail_job_count()? > 0)
    }

    pub(crate) fn pending_file_jobs(&self) -> Result<bool, BackendError> {
        if self.assets.placement_work(&self.db).map_err(BackendError::operation)?.0 {
            return Ok(true);
        }
        Ok(self.db.has_pending_purge_work()?)
    }

    pub(crate) async fn run_file_jobs(&self) -> Result<(), BackendError> {
        let had_file_work = self.assets.placement_work(&self.db).map_err(BackendError::operation)?.0;
        let had_purge_work = self.db.has_pending_purge_work()?;
        let result = self.assets.run_file_jobs(&self.db).await;
        if had_purge_work {
            self.emit(LibraryEvent::StorageChanged);
        }
        if had_file_work {
            self.emit(LibraryEvent::TransferStatusChanged);
            // Replay completion refreshes views and sync, but is not another
            // request for replay. Durable work can remain queued (for example
            // while a book is leased); ContentsChanged would self-wake forever.
            self.notify_replayed_contents();
            self.notify_storage_changed();
        }
        result.map_err(BackendError::operation)
    }

    pub(crate) async fn run_thumbnail_jobs(&self, cover_inspected: impl Fn() + crate::executor::BackendSend) -> Result<(), BackendError> {
        self.run_thumbnail_batch(cover_inspected, &asset_transfer::thumbnail_jobs::ThumbnailExtractor::new(self.cpu.clone())).await
    }

    pub(crate) async fn run_thumbnail_batch(&self, cover_inspected: impl Fn() + crate::executor::BackendSend, extractor: &asset_transfer::thumbnail_jobs::ThumbnailExtractor) -> Result<(), BackendError> {
        asset_transfer::thumbnail_jobs::ThumbnailJobs::new(&self.db, &self.assets, extractor).run_batch(&mut ThumbnailEvents { library: self, cover_inspected }).await.map_err(BackendError::operation)
    }

    /// One activity spans the fixed preparation batch, including progress
    /// notifications. Failed members remain retryable without stopping peers.
    pub(crate) async fn run_thumbnail_members(&self, hashes: Vec<ContentHash>) -> Result<(), BackendError> {
        let extractor = asset_transfer::thumbnail_jobs::ThumbnailExtractor::new(self.cpu.clone());
        asset_transfer::thumbnail_jobs::ThumbnailJobs::new(&self.db, &self.assets, &extractor).run_members(hashes, &mut ThumbnailEvents { library: self, cover_inspected: || {} }).await.map_err(BackendError::operation)
    }

    pub(super) async fn recover_thumbnail(&self, hash: ContentHash) -> Result<bool, BackendError> {
        asset_transfer::thumbnail_jobs::ThumbnailJobs::new(&self.db, &self.assets, &asset_transfer::thumbnail_jobs::ThumbnailExtractor::new(self.cpu.clone()))
            .recover(hash, &mut ThumbnailEvents { library: self, cover_inspected: || {} })
            .await
            .map_err(BackendError::operation)
    }

    pub(crate) async fn store_thumbnail(&self, hash: ContentHash, versions: &cpu_host::ThumbnailBytes) -> Result<(), BackendError> {
        asset_transfer::thumbnail_jobs::store_thumbnail(&self.assets, hash, &versions.browse, &versions.high_density).await.map_err(BackendError::operation)?;
        self.emit(LibraryEvent::ThumbnailGenerated { content_hash: hash });
        Ok(())
    }
}

struct ThumbnailEvents<'a, F> {
    library: &'a LibrarySession,
    cover_inspected: F,
}
impl<F: Fn() + crate::executor::BackendSend> asset_transfer::thumbnail_jobs::ThumbnailObserver for ThumbnailEvents<'_, F> {
    type Activity = super::session::ThumbnailActivity;
    fn prepare(&mut self, pending: u64) {
        self.library.prepare_thumbnail_batch(pending);
    }
    fn begin(&mut self) -> Self::Activity {
        self.library.begin_thumbnail_generation()
    }
    fn completed(&mut self) {
        self.library.complete_thumbnail_batch_item();
        self.library.sync_wake.wake();
        (self.cover_inspected)();
    }
    fn ready(&mut self, content_hash: ContentHash) {
        self.library.emit(LibraryEvent::ThumbnailGenerated { content_hash });
    }
}
