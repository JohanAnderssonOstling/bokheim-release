use super::error::SyncResult;
use super::library_sync::LibrarySync;
use asset_transfer::transfer::TransferWorker;
use asset_transfer::{TransferJob, TransferOrigin};
use library_database::TransferOutcome;
use library_database::ThumbnailSyncAction;
use sync_common::ContentHash;

// A manifest is metadata only.  Use the server's contractual ceiling here;
// actual book-byte uploads stay constrained by `book_batch` limits.
const BOOK_MANIFEST_PAGE_ENTRIES: i64 = sync_common::api::assets::MAX_BLOB_MANIFEST_ENTRIES as i64;
const COVER_PRESENCE_PAGE_ENTRIES: i64 = sync_common::api::assets::MAX_THUMBNAIL_PRESENCE_ENTRIES as i64;

struct TransferPlanGuard(std::sync::Arc<asset_transfer::TransferQueue>, u64);
impl Drop for TransferPlanGuard {
    fn drop(&mut self) {
        self.0.cancel_reconcile(self.1);
    }
}

use asset_transfer::planning::{AssetPlan, PlanCursor};

impl LibrarySync {
    pub(crate) async fn refresh_remote_cover_revisions(&self) -> SyncResult<()> {
        if !self.is_signed_in() { return Ok(()); }
        let credentials = self.credentials.credentials().ok_or_else(|| super::error::SyncError::failed("missing credentials"))?;
        let mut after = String::new();
        loop {
            let hashes = self.database()?.remote_thumbnail_presence_candidates_page(&after)?;
            if hashes.is_empty() { break; }
            after = hashes.last().expect("nonempty cover page").as_str().to_owned();
            let response = sync_transport::thumbnail_presence(&self.http_client, &credentials, &self.library_id, hashes.clone()).await.map_err(super::error::SyncError::failed)?;
            let requested = hashes.into_iter().collect::<std::collections::HashSet<_>>();
            let present = response.present.iter().copied().collect::<std::collections::HashSet<_>>();
            if present.len() != response.present.len() || !present.is_subset(&requested) {
                return Err(super::error::SyncError::failed("invalid thumbnail presence response"));
            }
            for hash in &present {
                self.database()?.record_remote_asset(library_replica::BlobKind::Thumbnail, hash)?;
            }
            let mut revised = std::collections::HashSet::new();
            for entry in response.revisions {
                if !present.contains(&entry.content_hash) || !revised.insert(entry.content_hash) {
                    return Err(super::error::SyncError::failed("unrequested thumbnail revision"));
                }
                let database = self.database()?;
                let changed = database.remote_thumbnail_revision(&entry.content_hash)?.is_some_and(|previous| previous != entry.revision);
                if changed && database.thumbnail_source_origin(&entry.content_hash)?.as_deref() == Some("remote") {
                    self.assets.remove(library_replica::BlobKind::Thumbnail, &entry.content_hash).map_err(super::error::SyncError::failed)?;
                    database.defer_thumbnail_to_download(&entry.content_hash)?;
                }
                database.record_remote_thumbnail_revision(&entry.content_hash, &entry.revision)?;
            }
            if !revised.is_empty() && revised != present {
                return Err(super::error::SyncError::failed("incomplete thumbnail revisions"));
            }
            for hash in requested.difference(&present) {
                let database = self.database()?;
                if database.has_remote_thumbnail(hash)? && database.thumbnail_source_origin(hash)?.as_deref() == Some("remote") {
                    self.assets.remove(library_replica::BlobKind::Thumbnail, hash).map_err(super::error::SyncError::failed)?;
                    database.shared_request_thumbnail(hash)?;
                }
                database.forget_remote_asset(library_replica::BlobKind::Thumbnail, hash)?;
                database.forget_remote_thumbnail_revision(hash)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn sync_pending_cover_changes(&self) -> SyncResult<()> {
        if !self.is_signed_in() { return Ok(()); }
        let credentials = self.credentials.credentials().ok_or_else(|| super::error::SyncError::failed("missing credentials"))?;
        let mut after = String::new();
        loop {
            let pending = self.database()?.pending_thumbnail_sync_page(&after)?;
            if pending.is_empty() { break; }
            for change in pending {
                after = change.content_hash.as_str().to_owned();
                match change.action {
                    ThumbnailSyncAction::Put => {
                        let Some(bytes) = self.assets.read_bytes(library_replica::BlobKind::Thumbnail, &change.content_hash).await.map_err(super::error::SyncError::failed)? else { continue };
                        if bytes.len() > sync_common::MAX_THUMBNAIL_BYTES as usize || !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                            continue;
                        }
                        sync_transport::upload_thumbnail_batch(&self.http_client, credentials.server_url(), credentials.access_token(), &self.library_id,
                            vec![sync_common::api::assets::ThumbnailBatchEntry { content_hash: change.content_hash, bytes }]).await.map_err(super::error::SyncError::failed)?;
                        self.database()?.record_remote_asset(library_replica::BlobKind::Thumbnail, &change.content_hash)?;
                    }
                    ThumbnailSyncAction::Delete => {
                        sync_transport::delete_thumbnail(&self.http_client, credentials.server_url(), credentials.access_token(), &self.library_id, &change.content_hash).await.map_err(super::error::SyncError::failed)?;
                        self.database()?.forget_remote_asset(library_replica::BlobKind::Thumbnail, &change.content_hash)?;
                        self.database()?.forget_remote_thumbnail_revision(&change.content_hash)?;
                    }
                }
                self.database()?.complete_thumbnail_sync(change)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn record_remote_thumbnail_presence(&self, hashes: Vec<ContentHash>) -> SyncResult<()> {
        if hashes.is_empty() || !self.is_signed_in() {
            return Ok(());
        }
        let credentials = self.credentials.credentials().ok_or_else(|| super::error::SyncError::failed("missing credentials"))?;
        let database = self.database()?;
        for page in hashes.chunks(COVER_PRESENCE_PAGE_ENTRIES as usize) {
            let response = sync_transport::thumbnail_presence(&self.http_client, &credentials, &self.library_id, page.to_vec()).await.map_err(super::error::SyncError::failed)?;
            for hash in response.present {
                database.record_remote_asset(library_replica::BlobKind::Thumbnail, &hash)?;
            }
        }
        Ok(())
    }
    async fn run_thumbnail_plan(&self, worker: &TransferWorker, generation: u64, plan: &AssetPlan) -> SyncResult<()> {
        let mut jobs = Vec::new();
        for hashes in plan.thumbnail_uploads.chunks(sync_common::api::assets::MAX_THUMBNAIL_BATCH_ENTRIES) {
            // Planning negotiates over a snapshot and hands the outcome back;
            // the session commits before those jobs reach the queue.
            let database = self.database()?;
            let snapshot = database.transfer_snapshot(hashes)?;
            let mut outcome = TransferOutcome::default();
            jobs.extend(worker.prepare_thumbnail_uploads_with_refresh(&snapshot, hashes, &mut outcome).await?);
            database.commit_transfer_outcome(&outcome)?;
        }
        jobs.extend(plan.thumbnail_downloads.iter().copied().map(|hash| TransferJob::download_thumbnail(hash, TransferOrigin::Background)));
        if !jobs.is_empty() {
            self.transfer_queue.reconcile_page(generation, jobs);
            worker.drive().await;
        }
        Ok(())
    }

    pub(super) async fn sync_assets(&self, targeted: Vec<ContentHash>) -> SyncResult<()> {
        self.transfer_queue.resume_authentication();
        let worker = self.transfer_worker();
        let generation = self.transfer_queue.begin_reconcile();
        let _plan = TransferPlanGuard(self.transfer_queue.clone(), generation);
        // Payload-targeted rows jump the lanes: plan exactly the woken
        // hashes through the shared inspect path, then fall through to
        // lanes and drain as usual.
        if !targeted.is_empty() {
            let rows = AssetPlan::read_rows(&self.database()?, &targeted)?;
            if !rows.is_empty() {
                let (mut plan, settled) = AssetPlan::inspect_rows(&self.assets, rows).await?;
                if !settled.is_empty() {
                    self.database()?.settle_asset_candidates(&settled)?;
                }
                // Cover uploads are selected below from durable state.
                plan.thumbnail_uploads.clear();
                self.run_thumbnail_plan(&worker, generation, &plan).await?;
            }
        }
        // Requested downloads run first: user-facing work jumps the bulk
        // queue. The request queue is read directly instead of deriving
        // from the asset_work drain below.
        let mut jobs = Vec::new();
        for hash in self.database()?.pending_download_requests()? {
            jobs.extend(self.prepare_requested_download(&hash)?);
        }
        if !jobs.is_empty() {
            self.transfer_queue.reconcile_page(generation, jobs);
            worker.drive().await;
        }
        // Book uploads read the intent queue directly instead of deriving
        // from the asset_work drain below, which now handles thumbnails
        // and file work. Drain successive bounded negotiation pages here:
        // server-present books are settled without a byte transfer and must
        // not wait for the background scheduler's retry interval.
        if self.asset_storage_enabled() {
            let mut after = 0;
            loop {
                let intents = self.database()?.pending_book_uploads(after, BOOK_MANIFEST_PAGE_ENTRIES)?;
                if intents.is_empty() {
                    break;
                }
                after = intents.last().expect("non-empty page has a last intent").id;
                let mut jobs = Vec::new();
                for intent in intents {
                    let local = self.assets.exists(library_replica::BlobKind::Book, &intent.content_hash).map_err(super::error::SyncError::failed)?;
                    if local {
                        jobs.push(TransferJob::upload_book_intent(intent, false, TransferOrigin::Background));
                    }
                }
                // A prepared entry can still lose its local blob outside the
                // application. Skip that stale entry for this pass so it
                // cannot hide later transferable records behind the page.
                if jobs.is_empty() {
                    continue;
                }
                // Upload negotiation and retries belong to the background driver.
                // Planning must return even when the book endpoint is stalled.
                worker.run_page(generation, jobs);
                tokio::task::yield_now().await;
            }

            self.sync_pending_cover_changes().await?;
            self.refresh_remote_cover_revisions().await?;

            // Covers are derived directly from durable thumbnail and remote
            // presence state. They must not depend on `asset_work`: that
            // table is only a lossy wake-up hint, and recording an uploaded
            // cover itself writes a new hint row.
            loop {
                let hashes = self.database()?.pending_cover_uploads(COVER_PRESENCE_PAGE_ENTRIES)?;
                if hashes.is_empty() {
                    break;
                }
                let database = self.database()?;
                let snapshot = database.transfer_snapshot(&hashes)?;
                let mut outcome = TransferOutcome::default();
                let jobs = worker.prepare_thumbnail_uploads_with_refresh(&snapshot, &hashes, &mut outcome).await?;
                database.commit_transfer_outcome(&outcome)?;
                if !jobs.is_empty() {
                    self.transfer_queue.reconcile_page(generation, jobs);
                    worker.drive().await;
                }
            }
        }
        let mut cursor = PlanCursor::default();
        // Each page is read synchronously, inspected with no database
        // reference held across the inspection's blocking-pool hop, and
        // committed synchronously again before the next page is read.
        while let Some(page) = AssetPlan::read_page(&self.database()?, &cursor)? {
            let (mut page, settled) = AssetPlan::inspect_page(&self.assets, page, &mut cursor).await?;
            if !settled.is_empty() {
                self.database()?.settle_asset_candidates(&settled)?;
            }
            // Cover uploads above use the direct durable-state query.  This
            // legacy candidate drain is retained only for thumbnail downloads
            // and local/file-work settlement.
            page.thumbnail_uploads.clear();
            self.run_thumbnail_plan(&worker, generation, &page).await?;
        }
        worker.finish_plan(generation);
        Ok(())
    }

    pub(super) fn prepare_requested_download(&self, hash: &ContentHash) -> SyncResult<Option<TransferJob>> {
        Ok(asset_transfer::planning::prepare_requested_download(&self.assets, &self.database()?, hash)?)
    }
}
