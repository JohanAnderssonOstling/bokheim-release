use app::AppClient;
use futures_util::future::{Either, select};
use gpui::AsyncWindowContext;
use library_backend::{BackendError, LibraryClient};
use library_model::{BookCardRow, BookSource, CatalogImportProgress, LibraryUpdate};
use sync_common::{BookId, DirId};
use ui_components::{BrowserDownloadPhase, BrowserDownloadUpdate, BrowserDownloads};

use super::format::download_byte_progress;
use crate::library::mark_feed_book_added;

/// Imports a source book into the requested folder. The caller owns progress
/// and completion feedback for its particular surface.
pub(crate) async fn import_catalog_book(app: &AppClient, library: &LibraryClient, source: BookSource, destination: DirId) -> Result<BookId, BackendError> {
    match source {
        BookSource::Catalog { content_hash } => library.add_catalog_book(destination, content_hash).await,
        BookSource::ExternalCatalog { entry, downloadable: true } => library.add_external_catalog_audiobook(destination, entry).await,
        BookSource::Opds { source, entry, downloadable: true, .. } => {
            let (name, bytes) = app.download_opds_book(source, entry).await?;
            library.import_downloaded_book(destination, name, bytes).await
        }
        BookSource::Library { content_hash } => Ok(content_hash),
        BookSource::ExternalCatalog { downloadable: false, .. } => Err("this book is read on its source website".into()),
        BookSource::Opds { downloadable: false, .. } => Err("this book is not offered for download".into()),
    }
}

/// The window owns progress and terminal feedback even if the initiating page
/// closes. Subscribe before polling the import so a fast response cannot race
/// its first progress update.
pub(crate) async fn import_catalog_book_with_progress(app: &AppClient, library: &LibraryClient, book: BookCardRow, destination: DirId, success_message: Option<&str>, cx: &mut AsyncWindowContext) -> Result<BookId, BackendError> {
    let (key, source) = (book.key(), book.source);
    let download = uuid::Uuid::new_v4().to_string();
    let mut downloads = BrowserDownloads::default();
    let mut update = BrowserDownloadUpdate { download: download.clone(), replaces: None, name: book.title, phase: BrowserDownloadPhase::Downloading, percent: None, message: None };
    let _ = cx.update(|window, cx| downloads.apply(update.clone(), window, cx));
    let updates = if matches!(source, BookSource::Catalog { .. } | BookSource::ExternalCatalog { .. }) {
        match library.updates().await {
            Ok((updates, _)) => Some(updates),
            Err(error) => {
                log::warn!("Could not subscribe to catalog import progress: {error}");
                None
            }
        }
    } else {
        None
    };
    let operation = import_catalog_book(app, library, source, destination);
    futures_util::pin_mut!(operation);
    let result = loop {
        let Some(updates) = &updates else { break operation.await };
        let progress = match select(operation.as_mut(), Box::pin(updates.recv())).await {
            Either::Left((result, _)) => break result,
            Either::Right((Ok(LibraryUpdate::CatalogImport { entry, progress }), _)) if entry == key => progress,
            Either::Right((Ok(_), _)) => continue,
            Either::Right((Err(_), _)) => break operation.await,
        };
        let _ = cx.update(|window, cx| match progress {
            CatalogImportProgress::Downloading { downloaded_bytes, total_bytes } => {
                let (percent, text) = download_byte_progress(downloaded_bytes, total_bytes);
                downloads.progress_detail(&download, percent, text.into(), window, cx);
            }
            CatalogImportProgress::Waiting => downloads.waiting(&download, window, cx),
            CatalogImportProgress::Importing => downloads.apply(BrowserDownloadUpdate { phase: BrowserDownloadPhase::Importing, ..update.clone() }, window, cx),
        });
    };
    update.phase = match &result {
        Ok(_) => BrowserDownloadPhase::Completed,
        Err(error) => {
            log::warn!("Could not add catalog/feed book {key:?} to library: {error}");
            BrowserDownloadPhase::Failed(error.failure())
        }
    };
    update.message = success_message.filter(|_| result.is_ok()).map(str::to_owned);
    let _ = cx.update(|window, cx| {
        if result.is_ok() {
            mark_feed_book_added(key, cx);
        }
        downloads.apply(update, window, cx);
    });
    result
}
