//! Executes durable thumbnail recovery without application handles or UI events.

use book_access::BoxedBookReader;
use client_platform_runtime::executor::BoxedBackendFuture;
use client_runtime::BackendError;
use library_database::Database;
use library_files::asset_store::{AssetStore, AssetStoreError};
use library_model::BlobKind;
use std::io::SeekFrom;
use std::sync::Arc;
use sync_common::BookId;
pub use thumbnail::ThumbnailVersions;

/// The host chooses native workers or browser CPU-worker RPC.
pub trait ThumbnailProcessor: Sync {
    fn decode(&self, bytes: Vec<u8>) -> BoxedBackendFuture<'_, Result<ThumbnailVersions, BackendError>>;
    fn generate(&self, extension: String, reader: BoxedBookReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailVersions>, BackendError>>;
}

/// Host-backed thumbnail extraction. The extractor carries an explicit host, so
/// thumbnail jobs stay platform-neutral without a process-wide default.
#[derive(Clone)]
pub struct ThumbnailExtractor {
    host: Arc<dyn cpu_host::CpuHost>,
}
impl ThumbnailExtractor {
    pub fn new(host: Arc<dyn cpu_host::CpuHost>) -> Self {
        Self { host }
    }
}
impl ThumbnailProcessor for ThumbnailExtractor {
    fn decode(&self, bytes: Vec<u8>) -> BoxedBackendFuture<'_, Result<ThumbnailVersions, BackendError>> {
        let host = self.host.clone();
        Box::pin(async move { cpu_host::cover(&*host, bytes).await.map(|v| ThumbnailVersions { browse: v.browse, card_high_density: v.card_high_density, high_density: v.high_density }) })
    }
    fn generate(&self, extension: String, reader: BoxedBookReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailVersions>, BackendError>> {
        let host = self.host.clone();
        Box::pin(async move { host.generate_thumbnail(extension, cpu_host::as_cpu_reader(reader)).await.map(|v| v.map(|v| ThumbnailVersions { browse: v.browse, card_high_density: v.card_high_density, high_density: v.high_density })) })
    }
}

/// Runs entirely on a library's own dedicated thread (see
/// `client_platform_runtime::executor::LibraryExecutor`), never on the shared
/// multi-threaded executor, so implementors — including ones holding a
/// `&LibrarySession` — need not be `Send`.
pub trait ThumbnailObserver {
    fn completed(&mut self);
    fn ready(&mut self, hash: BookId);
}

pub struct ThumbnailJobs<'a, P> {
    db: &'a Database,
    assets: &'a AssetStore,
    processor: &'a P,
}

impl<'a, P: ThumbnailProcessor> ThumbnailJobs<'a, P> {
    pub fn new(db: &'a Database, assets: &'a AssetStore, processor: &'a P) -> Self {
        Self { db, assets, processor }
    }
    /// Makes covers for these books. A failure leaves the book's work pending
    /// for a later pass; the first failure is returned once all were tried.
    pub async fn run(&self, hashes: Vec<BookId>, observer: &mut impl ThumbnailObserver) -> Result<(), BackendError> {
        let mut failure = None;
        for hash in hashes {
            match self.recover(hash, observer).await {
                Ok(true) => {
                    observer.completed();
                }
                Ok(false) => {}
                Err(error) => {
                    let title = || self.db.browse_book_detail(&hash).ok().map(|detail| detail.book.title).unwrap_or_else(|| "Untitled".into());
                    log::warn!("thumbnail recovery for {:?} will retry: {error}", title());
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub async fn recover(&self, hash: BookId, observer: &mut impl ThumbnailObserver) -> Result<bool, BackendError> {
        async {
            let lease = self.assets.lease_book(&hash).await.map_err(BackendError::operation)?;
            let Some(format) = self.db.thumbnail_book_format(hash)? else { return Ok(false) };
            // Server covers are downloaded as finished images. Do not rebuild
            // a cached server master while local recovery is being retired.
            if self.db.server_has_cover(&hash)? {
                self.db.discard_thumbnail_work(&hash)?;
                return Ok(true);
            }
            // A book's cover is set once, so a stored cover is kept. A pending
            // job may have left incomplete bytes behind: decode the cached
            // master before confirming it, and rebuild its browse variant.
            let (_lease, cached) = if let Some(bytes) = self.assets.read_bytes(BlobKind::Thumbnail, &hash).await.map_err(BackendError::operation)? {
                drop(lease);
                let original = bytes.clone();
                let cached = self.processor.decode(bytes).await.ok();
                let lease = self.assets.lease_book(&hash).await.map_err(BackendError::operation)?;
                if self.assets.read_bytes(BlobKind::Thumbnail, &hash).await.map_err(BackendError::operation)?.as_deref() != Some(original.as_slice()) {
                    // A chosen image replaced the master during CPU decoding.
                    return Ok(false);
                }
                if cached.is_none() {
                    self.assets.remove(BlobKind::Thumbnail, &hash).map_err(BackendError::operation)?;
                }
                (lease, cached)
            } else {
                (lease, None)
            };
            if let Some(versions) = cached {
                drop(_lease);
                self.store(hash, &versions, observer).await?;
                self.db.complete_thumbnail_work(&hash, true)?;
                return Ok(true);
            }
            let title = || self.db.browse_book_detail(&hash).ok().map(|detail| detail.book.title).unwrap_or_else(|| "Untitled".into());
            let paths = self.db.book_paths(&hash)?;
            let mut local_reader = self.assets.open_book_reader(&hash, paths.clone()).await.map_err(BackendError::operation)?;
            let local_source_available = local_reader.is_some();
            // Artwork shipped with an audiobook becomes its cover.
            if format == book_model::BookFormat::M4b && local_source_available {
                if let Some(bytes) = self.assets.m4b_sidecar_cover(paths).await.map_err(BackendError::operation)? {
                    match self.processor.decode(bytes).await {
                        Ok(versions) => {
                            drop(_lease);
                            drop(local_reader);
                            self.store(hash, &versions, observer).await?;
                            self.db.complete_thumbnail_work(&hash, true)?;
                            return Ok(true);
                        }
                        Err(error) => log::warn!("ignoring invalid M4B sidecar cover for {:?}: {error}", title()),
                    }
                }
            }
            if format == book_model::BookFormat::Mp3Folder {
                if let Some(reader) = local_reader.as_mut() {
                    // An unreadable archive has no cover here; generation below says so.
                    let archived = audiobook_archive::archived_cover(&mut *reader).unwrap_or_else(|error| {
                        log::warn!("ignoring unreadable MP3 folder archive for {:?}: {error}", title());
                        None
                    });
                    if let Some(bytes) = archived {
                        match self.processor.decode(bytes).await {
                            Ok(versions) => {
                                drop(_lease);
                                drop(local_reader);
                                self.store(hash, &versions, observer).await?;
                                self.db.complete_thumbnail_work(&hash, true)?;
                                return Ok(true);
                            }
                            Err(error) => log::warn!("ignoring invalid MP3 folder cover for {:?}: {error}", title()),
                        }
                    }
                    reader.seek(SeekFrom::Start(0))?;
                }
            }
            let Some(mut reader) = local_reader else {
                return Ok(false);
            };
            if reader.seek(SeekFrom::End(0))? == 0 {
                // Retire legacy empty-file jobs without making them candidates
                // for external cover enrichment or publishing a thumbnail.
                self.db.discard_thumbnail_work(&hash)?;
                return Ok(true);
            }
            reader.seek(SeekFrom::Start(0))?;
            // The opened reader owns its bytes. Do not hold a placement lease
            // while waiting for CPU work: file moves must be able to proceed.
            drop(_lease);
            let versions = self.processor.generate(format.canonical_extension().to_owned(), reader).await.map_err(BackendError::operation)?;
            let _lease = self.assets.lease_book(&hash).await.map_err(BackendError::operation)?;
            if !self.db.thumbnail_book_format(hash).map(|format| format.is_some())? {
                return Ok(false);
            }
            // A selected catalog image may replace the cache while extraction
            // is in flight. Preserve that completed replacement.
            if self.db.thumbnail_work_state(&hash)?.as_deref() == Some("ready") && self.assets.thumbnail_set_exists(&hash).map_err(BackendError::operation)? {
                return Ok(true);
            }
            let has_cover = versions.is_some();
            drop(_lease);
            if let Some(versions) = versions {
                self.store(hash, &versions, observer).await?;
            }
            self.db.complete_thumbnail_work(&hash, has_cover)?;
            // Both ready and no_cover make external cover enrichment eligible.
            Ok(true)
        }
        .await
    }

    async fn store(&self, hash: BookId, versions: &ThumbnailVersions, observer: &mut impl ThumbnailObserver) -> Result<(), BackendError> {
        store_thumbnail(self.assets, hash, &versions.browse, &versions.card_high_density, &versions.high_density).await.map_err(BackendError::operation)?;
        observer.ready(hash);
        Ok(())
    }
}

/// Publish the master and both card densities before reporting a ready thumbnail.
pub async fn store_thumbnail(assets: &AssetStore, hash: BookId, browse: &[u8], card_high_density: &[u8], high_density: &[u8]) -> Result<(), AssetStoreError> {
    assets.write_thumbnail_set(&hash, high_density, browse, card_high_density).await
}
