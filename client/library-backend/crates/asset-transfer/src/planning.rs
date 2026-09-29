//! Bounded asset work paging, remote negotiation, and transfer job selection.
//!
//! Nothing here touches a library's database connection while an `.await` is
//! outstanding: callers read a snapshot, hand this module plain data to
//! negotiate with the server, and commit the result themselves afterward.
use crate::deadline::transfer_deadline;
use crate::policy::SMALL_REQUEST_TIMEOUT;
use crate::{TransferError, TransferJob, TransferOrigin};
use library_database::BookUploadIntent;
use library_files::AssetStore;
use library_replica::BlobKind;
use std::collections::HashSet;
use sync_common::ContentHash;

/// The server's answer to a book-upload manifest, before it is committed.
pub struct BookUploadNegotiation {
    pub present_remotely: Vec<ContentHash>,
    pub rejected: Vec<(ContentHash, client_runtime::BackendError)>,
    pub upload_required: HashSet<ContentHash>,
}

/// The server's answer to a thumbnail-availability check, before it is committed.
pub struct ThumbnailAvailabilityNegotiation {
    pub available: Vec<ContentHash>,
    pub missing: Vec<ContentHash>,
}

/// Negotiates remote availability using the host account and library connection.
pub struct TransferPlanner<C> {
    pub http_client: reqwest::Client,
    pub library_id: sync_common::LibraryId,
    pub credentials: C,
}
impl<C> TransferPlanner<C>
where
    C: Fn() -> Result<sync_transport::SyncCredentials, TransferError> + Sync,
{
    fn credentials(&self) -> Result<sync_transport::SyncCredentials, TransferError> {
        (self.credentials)()
    }

    /// Pure network negotiation: no database reference is ever held across
    /// the request. The caller reads `intents` from its own database
    /// snapshot beforehand and commits `present_remotely`/`rejected`
    /// afterward.
    pub async fn negotiate_book_uploads(&self, intents: &[BookUploadIntent]) -> Result<BookUploadNegotiation, TransferError> {
        let credentials = self.credentials()?;
        let manifest = intents.iter().map(|job| sync_common::api::assets::BlobManifestEntry { content_hash: job.content_hash, checksum: job.checksum, size_bytes: job.size_bytes }).collect();
        let response = transfer_deadline(SMALL_REQUEST_TIMEOUT, crate::negotiate_blobs(&self.http_client, &credentials, &self.library_id, manifest)).await?.map_err(|error| match error {
            sync_transport::SyncRequestError::AuthenticationRequired => TransferError::AuthenticationRequired,
            error => TransferError::retryable(error),
        })?;
        let present_remotely = response.owned.iter().chain(&response.claimed).copied().collect::<Vec<_>>();
        let rejected = response
            .rejected
            .iter()
            .filter_map(|rejection| match TransferError::from_upload_admission(rejection.reason) {
                TransferError::Rejected(reason) => Some((rejection.content_hash, reason)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let upload_required = response.upload.into_iter().collect::<HashSet<_>>();
        Ok(BookUploadNegotiation { present_remotely, rejected, upload_required })
    }

    /// Pure network negotiation, same shape as [`Self::negotiate_book_uploads`].
    pub async fn negotiate_thumbnail_availability(&self, hashes: &[ContentHash]) -> Result<ThumbnailAvailabilityNegotiation, TransferError> {
        let credentials = self.credentials()?;
        let response = transfer_deadline(SMALL_REQUEST_TIMEOUT, sync_transport::thumbnail_presence(&self.http_client, &credentials, &self.library_id, hashes.to_vec())).await?.map_err(|error| match error {
            sync_transport::SyncRequestError::AuthenticationRequired => TransferError::AuthenticationRequired,
            error => TransferError::retryable(error),
        })?;
        let requested: HashSet<_> = hashes.iter().copied().collect();
        let mut answered = HashSet::new();
        for hash in &response.present {
            if !requested.contains(hash) || !answered.insert(*hash) {
                return Err(TransferError::retryable("thumbnail availability response is invalid"));
            }
        }
        let available = response.present;
        let missing = requested.difference(&answered).copied().collect();
        Ok(ThumbnailAvailabilityNegotiation { available, missing })
    }

    /// Confirms the server identity has not changed since a snapshot was
    /// read, before its negotiated result is committed.
    pub fn check_transfer_account(&self, credentials: &sync_transport::SyncCredentials) -> Result<(), TransferError> {
        if self.credentials()?.server_url() != credentials.server_url() {
            return Err(TransferError::retryable("credentials changed during asset transfer"));
        }
        Ok(())
    }
}

use crate::should_download_thumbnail;
/// One bounded page of asset work, submitted before inspecting the next page.
#[derive(Default)]
pub struct AssetPlan {
    pub thumbnail_uploads: Vec<ContentHash>,
    pub thumbnail_downloads: Vec<ContentHash>,
}

#[derive(Default)]
/// Tracks a fixed work watermark so newly enqueued work waits for the next pass.
pub struct PlanCursor {
    after: i64,
    through: Option<i64>,
    finished: bool,
}

/// One page of durable work, read from the database, ready for CPU-bound
/// inspection with no connection reference alive while that runs.
pub struct AssetPlanPage {
    rows: Vec<library_database::AssetCandidate>,
    through: i64,
    last: Option<i64>,
}

impl AssetPlan {
    /// Reads one bounded page from the database. Synchronous: returns before
    /// any network or CPU-bound work starts, so nothing here is held across
    /// an `.await`.
    pub fn read_page(database: &library_database::Database, cursor: &PlanCursor) -> Result<Option<AssetPlanPage>, TransferError> {
        if cursor.finished {
            return Ok(None);
        }
        let snapshot = database.asset_planning_page(cursor.after, cursor.through)?;
        let last = snapshot.rows.last().map(|row| row.work_id);
        Ok(Some(AssetPlanPage { rows: snapshot.rows, through: snapshot.through, last }))
    }

    /// Plans payload-woken hashes directly instead of paging from the
    /// cursor. Rows that already settled read as absent and are skipped;
    /// the bounded drain remains the backstop. Synchronous, like
    /// [`Self::read_page`].
    pub fn read_rows(database: &library_database::Database, hashes: &[ContentHash]) -> Result<Vec<library_database::AssetCandidate>, TransferError> {
        let mut rows = Vec::new();
        for hash in hashes {
            if let Some(candidate) = database.candidate_row_of(hash)? {
                rows.push(candidate);
            }
        }
        Ok(rows)
    }

    /// Inspects a page's local file availability and advances `cursor` for
    /// the next call to [`Self::read_page`]. Pure CPU/filesystem work; takes
    /// no database reference.
    pub async fn inspect_page(assets: &AssetStore, page: AssetPlanPage, cursor: &mut PlanCursor) -> Result<(Self, Vec<(i64, bool)>), TransferError> {
        let AssetPlanPage { rows, through, last } = page;
        cursor.through = Some(through);
        match last {
            Some(last) => cursor.after = last,
            None => cursor.finished = true,
        }
        Self::inspect_rows(assets, rows).await
    }

    /// Row inspection without cursor movement, for targeted payload reads.
    /// Same derivation as [`Self::inspect_page`]; the caller settles and
    /// builds jobs identically.
    pub async fn inspect_rows(assets: &AssetStore, rows: Vec<library_database::AssetCandidate>) -> Result<(Self, Vec<(i64, bool)>), TransferError> {
        let assets = assets.clone();
        let inspect = move || -> Result<_, TransferError> {
            let mut plan = Self::default();
            let mut settled = Vec::new();
            for state in rows {
                let hash = state.content_hash;
                if !state.live || state.path.is_none() {
                    settled.push((state.work_id, false));
                    continue;
                }
                let book_local = assets.exists(BlobKind::Book, &hash)?;
                let thumbnail_local = assets.exists(BlobKind::Thumbnail, &hash)?;
                let thumbnail_complete = thumbnail_local && assets.thumbnail_variant_exists(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH)?;
                let upload_thumbnail = thumbnail_local && state.thumbnail_upload_allowed;
                let download_thumbnail = should_download_thumbnail(book_local, thumbnail_complete, state.thumbnail_remote, state.thumbnail_pending);
                if upload_thumbnail {
                    plan.thumbnail_uploads.push(hash);
                }
                if download_thumbnail {
                    plan.thumbnail_downloads.push(hash);
                }
                if !state.requested && !upload_thumbnail && !download_thumbnail && !state.rejected && !state.file_work_pending {
                    settled.push((state.work_id, true));
                }
            }
            Ok((plan, settled))
        };
        #[cfg(not(target_arch = "wasm32"))]
        let (plan, settled) = client_platform_runtime::executor::run_blocking(inspect).await.map_err(TransferError::retryable)??;
        #[cfg(target_arch = "wasm32")]
        let (plan, settled) = inspect()?;
        Ok((plan, settled))
    }
}

pub fn prepare_requested_download(assets: &AssetStore, database: &library_database::Database, hash: &ContentHash) -> Result<Option<TransferJob>, TransferError> {
    if assets.exists(BlobKind::Book, hash)? {
        database.complete_local_book_request(*hash)?;
        return Ok(None);
    }
    Ok(database.requested_book_download(*hash)?.map(|placement| TransferJob::download_blob(*hash, placement, TransferOrigin::UserInitiated)))
}
