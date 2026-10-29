//! Shared Bokheim ZIP-based book opening policy over platform-owned byte readers.

use book_access::{AsyncBookSource, RemoteFile, RemoteFileError, SparseBookSource};
use book_content::EpubOpeningPlan;

use super::{BackendError, BookId, LibrarySession};
#[cfg(not(target_arch = "wasm32"))]
use super::{BookFormat, DocumentBook, ResolvedBook};
use crate::library::remote_access::{authenticated_file_request, range_backend_error};

pub(super) async fn describe(library: &LibrarySession, hash: BookId) -> Result<(RemoteFile, Vec<u8>), BackendError> {
    let account = library.account()?;
    let client = library.sync.http_client.clone();
    let id = library.id;
    authenticated_file_request(&account, |session| {
        let client = client.clone();
        async move {
            let url = sync_transport::blob_endpoint(session.server_url(), &id, &hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
            book_access::remote_file::describe_tail(&client, url, session.token(), hash).await
        }
    })
    .await
    .map_err(range_backend_error)
}

/// Both native and browser readers use this complete HTML/CSS opening plan.
/// Optional bounded prefetch falls back to eager demand reads for large books.
pub(super) async fn prepare_epub(source: SparseBookSource) -> Result<AsyncBookSource, BackendError> {
    let source = AsyncBookSource::from(source);
    let reader_source = source.clone();
    let plan = EpubOpeningPlan::read(move || reader_source.reader()).await.map_err(BackendError::operation)?;
    source.prefetch(plan.initial_ranges().map_err(BackendError::operation)?).await.map_err(BackendError::operation)?;
    let reader_source = source.clone();
    let required = plan.required_ranges(move || reader_source.reader()).await.map_err(BackendError::operation)?;
    source.prefetch(required).await.map_err(BackendError::operation)?;
    Ok(source)
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn open(library: &LibrarySession, hash: BookId, path: String, format: BookFormat) -> Result<ResolvedBook, BackendError> {
    let (file, tail) = describe(library, hash).await?;
    let access = library.remote_book_access(&file)?;
    let source = SparseBookSource::remote(file.length, file.length - tail.len() as u64, tail, access.clone().fetch()).map_err(BackendError::operation)?;
    let source = source.with_remote_prefetch(access.batch_fetch()).map_err(BackendError::operation)?;
    let source = if format == BookFormat::Epub { prepare_epub(source).await? } else { AsyncBookSource::from(source) };
    Ok(ResolvedBook::Document(DocumentBook::from_async_source(path, format, source)))
}
