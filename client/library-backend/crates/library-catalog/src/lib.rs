//! Catalog files on their origin: hosted books checked against their hashes,
//! and retained cover images. Downloads need no account.
use client_platform_runtime::transfers;
use client_runtime::{BackendError, Failure};
use content_address::BookId;
use futures_util::StreamExt;
use library_model::{CardKey, CatalogImportProgress};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use web_time::Instant;

#[cfg(test)]
mod download_tests;

/// Cover files fetched this session, by book and filename suffix. Cache each
/// response once even when several card sizes fall back to the same image.
/// `None` remembers a missing file; failed requests can be retried.
static COVERS: OnceLock<Mutex<HashMap<(CardKey, String), Option<Vec<u8>>>>> = OnceLock::new();
static COVER_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Release retained JPEGs when entering a reader. Loads started before this
/// call may return to their caller, but cannot repopulate the cache.
pub fn evict_cover_caches() {
    COVER_GENERATION.fetch_add(1, Ordering::SeqCst);
    if let Some(covers) = COVERS.get() {
        covers.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }
}

/// Cache finished server artwork by density. Older origins supply their
/// existing card/detail JPEGs without applying local crop rules again.
pub async fn card_cover(key: CardKey, width: u32) -> Option<Vec<u8>> {
    let generation = COVER_GENERATION.load(Ordering::SeqCst);
    if ![thumbnail::BROWSE_THUMBNAIL_WIDTH, thumbnail::THUMBNAIL_WIDTH].contains(&width) {
        return None;
    }
    if let Some(bytes) = fetch_cover_at(key, &thumbnail::card_thumbnail_suffix(width), generation).await.ok()? {
        return Some(bytes);
    }
    let suffix = if width == thumbnail::THUMBNAIL_WIDTH { "detail" } else { "card" };
    let cover = fetch_cover_at(key, suffix, generation).await.ok().flatten();
    if cover.is_none() && width == thumbnail::BROWSE_THUMBNAIL_WIDTH {
        fetch_cover_at(key, "detail", generation).await.ok().flatten()
    } else {
        cover
    }
}

/// Where the catalog's files are served. `BOKHEIM_CATALOG_ORIGIN` points a
/// native build at another, for development before the catalog is hosted.
fn origin() -> String {
    std::env::var("BOKHEIM_CATALOG_ORIGIN").unwrap_or_else(|_| "https://catalogs.bokheim.se".into())
}

fn in_catalog(hash: &BookId) -> bool {
    library_database::catalog::contains(CardKey::from(hash), Some(false)).unwrap_or(false)
}

/// The catalog book's file, after checking it is the book the catalog names.
pub async fn download_book(hash: BookId) -> Result<Vec<u8>, BackendError> {
    download_book_with_progress(hash, &|_| {}).await
}

pub async fn download_book_with_progress(hash: BookId, progress: &impl Fn(CatalogImportProgress)) -> Result<Vec<u8>, BackendError> {
    if !in_catalog(&hash) {
        return Err(BackendError::classified(Failure::NotFound, "the book is not in the catalog"));
    }
    let url = format!("{}/books/{hash}.epub", origin());
    let bytes = download_with_progress(&url, progress).await?;
    progress(CatalogImportProgress::Importing);
    if Some(content_address::hash_reader(&mut bytes.as_slice())?) != hash.content_hash() {
        return Err(BackendError::classified(Failure::UnreadableBook, "the downloaded book differs from the catalog's"));
    }
    Ok(bytes)
}

/// An external audiobook from the archive its source offers, repacked as the
/// library's MP3 audiobook archive. Track bytes and source artwork are
/// preserved; catalog artwork is fetched separately for thumbnail generation.
/// The URL
/// comes from the catalog, never the caller; the source's bytes carry no
/// catalog hash to check.
pub async fn download_audiobook(entry: CardKey) -> Result<Vec<u8>, BackendError> {
    download_audiobook_with_progress(entry, &|_| {}).await
}

pub async fn download_audiobook_with_progress(entry: CardKey, progress: &impl Fn(CatalogImportProgress)) -> Result<Vec<u8>, BackendError> {
    let url = library_database::catalog::audiobook_url(entry).map_err(BackendError::operation)?.ok_or_else(|| BackendError::classified(Failure::NotFound, "the audiobook is not offered for download"))?;
    let bytes = download_with_progress(&url, progress).await?;
    progress(CatalogImportProgress::Importing);
    client_platform_runtime::executor::run_blocking(move || audiobook_archive::repack_zip(std::io::Cursor::new(bytes), None))
        .await
        .map_err(|error| BackendError::message(format!("could not prepare the downloaded audiobook: {error}")))?
        .map_err(|error| BackendError::classified(Failure::UnreadableBook, format!("could not read the downloaded audiobook: {error}")))
}

async fn download_with_progress(url: &str, progress: &impl Fn(CatalogImportProgress)) -> Result<Vec<u8>, BackendError> {
    // Catalog acquisitions are explicit user downloads.
    progress(CatalogImportProgress::Downloading { downloaded_bytes: 0, total_bytes: None });
    let response = reqwest::get(url).await.and_then(|response| response.error_for_status()).map_err(|error| BackendError::http("could not download the book", error))?;
    let total_bytes = response.content_length().filter(|size| *size > 0);
    progress(CatalogImportProgress::Downloading { downloaded_bytes: 0, total_bytes });
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    let mut last_report = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| BackendError::http("could not download the book", error))?;
        bytes.extend_from_slice(&chunk);
        if last_report.elapsed() >= Duration::from_millis(100) {
            progress(CatalogImportProgress::Downloading { downloaded_bytes: bytes.len() as u64, total_bytes });
            last_report = Instant::now();
        }
    }
    progress(CatalogImportProgress::Downloading { downloaded_bytes: bytes.len() as u64, total_bytes });
    Ok(bytes)
}

/// Fetch a cover directly by book hash, independently of catalog membership.
/// A failed fetch is not remembered, so the next request tries again.
pub async fn cover(hash: BookId, high_density: bool) -> Option<Vec<u8>> {
    fetch_cover(CardKey::from(hash), high_density).await
}

/// An external entry's retained image. The entry identity is not a book hash
/// and cannot authorize a book download.
pub async fn external_cover(entry: CardKey, high_density: bool) -> Option<Vec<u8>> {
    fetch_cover(entry, high_density).await
}

/// Fetch current catalog artwork for an explicit thumbnail replacement.
/// Bypass the browsing cache without modifying the audiobook archive.
pub async fn fresh_external_cover(entry: CardKey) -> Result<Option<Vec<u8>>, String> {
    let generation = COVER_GENERATION.load(Ordering::SeqCst);
    let cover = fetch_cover_asset_allowed(entry, "detail").await?;
    let mut covers = COVERS.get_or_init(Default::default).lock().map_err(|_| "cover cache is unavailable")?;
    if COVER_GENERATION.load(Ordering::SeqCst) == generation {
        covers.retain(|(key, _), _| *key != entry);
        covers.insert((entry, "detail".into()), cover.clone());
    }
    Ok(cover)
}

async fn fetch_cover(key: CardKey, high_density: bool) -> Option<Vec<u8>> {
    fetch_cover_at(key, if high_density { "detail" } else { "card" }, COVER_GENERATION.load(Ordering::SeqCst)).await.ok().flatten()
}

async fn fetch_cover_at(key: CardKey, suffix: &str, generation: u64) -> Result<Option<Vec<u8>>, String> {
    let covers = COVERS.get_or_init(Default::default);
    let address = (key, suffix.to_owned());
    if let Some(cover) = covers.lock().map_err(|_| "cover cache is unavailable")?.get(&address) {
        return Ok(cover.clone());
    }
    let cover = fetch_cover_asset(key, suffix).await?;
    let mut covers = covers.lock().map_err(|_| "cover cache is unavailable")?;
    if COVER_GENERATION.load(Ordering::SeqCst) == generation {
        covers.insert(address, cover.clone());
    }
    Ok(cover)
}

async fn fetch_cover_asset(key: CardKey, suffix: &str) -> Result<Option<Vec<u8>>, String> {
    // A paused cover must not occupy the library's bounded reader slots.
    // Failed fetches are not cached; the policy observer refreshes the view
    // when automatic transfers become available again.
    transfers::try_run(fetch_cover_asset_allowed(key, suffix)).await.map_err(|error| error.to_string())?
}

async fn fetch_cover_asset_allowed(key: CardKey, suffix: &str) -> Result<Option<Vec<u8>>, String> {
    let url = format!("{}/covers/{key}-{suffix}.jpg", origin());
    let request = reqwest::Client::new().get(&url);
    // A manually supplied Cache-Control header triggers a CORS preflight.
    // Let browser Fetch perform cache revalidation without that custom header.
    #[cfg(target_arch = "wasm32")]
    let request = request.fetch_cache_no_cache();
    #[cfg(not(target_arch = "wasm32"))]
    let request = request.header(reqwest::header::CACHE_CONTROL, "no-cache");
    let response = request.send().await.map_err(|error| format!("could not download the cover: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response.error_for_status().map_err(|error| format!("could not download the cover: {error}"))?;
    Ok(Some(response.bytes().await.map_err(|error| format!("could not download the cover: {error}"))?.to_vec()))
}
