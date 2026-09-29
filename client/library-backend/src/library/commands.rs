//! Execution of a library protocol command by its owning session.
//!
//! The runtime defines the wire command, but only the library module is
//! allowed to interpret it against a resource-bearing LibrarySession.

use crate::ThumbnailResolution;
use client_runtime::reply::Reply;

use super::pending::OperationKind;
use super::LibrarySession;
use futures_util::future::LocalBoxFuture;
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
mod web;

pub(super) enum PreparedRequest {
    Ready(Reply),
    Pending(OperationKind, LocalBoxFuture<'static, Result<Reply, crate::BackendError>>),
}

/// Preparation owns the activity until the durable import coordinator takes
/// over. Dropping an unpolled/rejected operation or unwinding cannot leak it.
struct PreparingImport {
    library: Rc<LibrarySession>,
    activity: uuid::Uuid,
    handed_off: bool,
}
impl PreparingImport {
    fn hand_off(mut self) {
        self.handed_off = true;
    }
}
impl Drop for PreparingImport {
    fn drop(&mut self) {
        if !self.handed_off {
            self.library.finish_directory_import(self.activity);
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum LibraryCommand {
    Scanning,
    DownloadState {
        content_hash: crate::ContentHash,
    },
    TransferSnapshot,
    ImportProgress {
        activity: uuid::Uuid,
    },
    SyncStatus {
        signed_in: bool,
        remote_present: Option<bool>,
    },
    LocalStorageUsage,
    // Renewal only authorizes the original remote book. It must not reopen
    // metadata or change source type if the user downloaded a local copy.
    RenewRemoteAudio {
        content_hash: crate::ContentHash,
    },
    ReadFileRange {
        source: crate::library::RemoteFile,
        offset: u64,
        length: usize,
    },
    ReadFileBundle {
        source: crate::library::RemoteFile,
        ranges: Vec<(u64, usize)>,
    },
    BookCount,
    Home {
        limit: i32,
    },
    Thumbnail {
        content_hash: crate::ContentHash,
        resolution: ThumbnailResolution,
    },
    DownloadBook {
        content_hash: crate::ContentHash,
    },
    DownloadFolder {
        query: library_model::LibraryBrowseQuery,
    },
    DownloadSubject {
        query: library_model::LibraryBrowseQuery,
    },
    RemoveLocalBookCopy {
        content_hash: crate::ContentHash,
    },
    EvictFolderDownloads {
        directory_id: crate::DirId,
    },
    FolderDestinations,
    Trash,
    RestoreBook {
        content_hash: crate::ContentHash,
    },
    RestoreDirectory {
        directory_id: crate::DirId,
        parent_id: Option<crate::DirId>,
    },
    PurgeBook {
        content_hash: crate::ContentHash,
    },
    PurgeDirectory {
        directory_id: crate::DirId,
    },
    EmptyTrash,
    CreateDirectory {
        parent_id: crate::DirId,
        name: String,
    },
    RenameDirectory {
        directory_id: crate::DirId,
        name: String,
    },
    MoveDirectory {
        directory_id: crate::DirId,
        parent_id: crate::DirId,
    },
    CopyDirectory {
        directory_id: crate::DirId,
        parent_id: crate::DirId,
    },
    RestoreBookPlacement {
        content_hash: crate::ContentHash,
        directory_id: crate::DirId,
    },
    CopyBookToDirectory {
        content_hash: crate::ContentHash,
        source_id: crate::DirId,
        destination_id: crate::DirId,
    },
    MoveBookToDirectory {
        content_hash: crate::ContentHash,
        source_id: crate::DirId,
        destination_id: crate::DirId,
    },
    RemoveBookFromDirectory {
        content_hash: crate::ContentHash,
        source_id: crate::DirId,
    },
    MoveBookToTrash {
        content_hash: crate::ContentHash,
    },
    MoveDirectoryToTrash {
        directory_id: crate::DirId,
    },
    ImportBookBytes {
        parent_id: crate::DirId,
        file_name: String,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    PrepareStagedDirectoryImport {
        activity: uuid::Uuid,
        job: String,
        parent_id: crate::DirId,
        name: Option<String>,
        directories: Vec<Vec<String>>,
    },
    FinishDirectoryImport {
        activity: uuid::Uuid,
    },
    DirectoryImportProgress,
    AdvanceDirectoryImport {
        activity: uuid::Uuid,
        total: u64,
        succeeded: u64,
        failed: u64,
    },
    FolderContents {
        query: library_model::LibraryBrowseQuery,
    },
    SubjectContents {
        query: library_model::LibraryBrowseQuery,
    },
    LibrarySearch {
        query: String,
    },
    BookDetail {
        content_hash: crate::ContentHash,
    },
    Authors {
        sort: library_model::AuthorSort,
    },
    BookFormat {
        content_hash: crate::ContentHash,
    },
    AudiobookPlaybackMetadata {
        content_hash: crate::ContentHash,
    },
    ReadingPosition {
        content_hash: crate::ContentHash,
    },
    PdfReadingPosition {
        content_hash: crate::ContentHash,
    },
    UpdateReadingPosition {
        content_hash: crate::ContentHash,
        position: String,
        progress: Option<f32>,
        entry: Option<String>,
    },
    Annotations {
        content_hash: crate::ContentHash,
    },
    UpsertAnnotation {
        annotation: book_model::ReaderAnnotation,
    },
    DeleteAnnotation {
        annotation_id: String,
        modified_at: i64,
    },
    #[cfg(target_arch = "wasm32")]
    OpenRemoteAudio {
        content_hash: crate::ContentHash,
    },
    #[cfg(target_arch = "wasm32")]
    ReadBookRange {
        capability: String,
        offset: u64,
        length: usize,
    },
    #[cfg(target_arch = "wasm32")]
    ImportStagedBook {
        parent_id: crate::DirId,
        file_name: String,
        physical: String,
        length: u64,
        hash: crate::ContentHash,
    },
    #[cfg(target_arch = "wasm32")]
    CleanupStagedImport {
        job: String,
        files: u32,
    },
}

pub mod library_requests {
    pub use super::LibraryCommand::*;
}

/// A serialized reply produced by a library command.
pub trait LibraryReply: crate::executor::BackendOutput + serde::Serialize + serde::de::DeserializeOwned {}
impl<T: crate::executor::BackendOutput + serde::Serialize + serde::de::DeserializeOwned> LibraryReply for T {}

/// Executes short commands in receive order, or prepares owned async work.
/// The exhaustive match is the ordering audit: adding an async operation must
/// choose its concurrency/cancellation class explicitly. Native replies stay
/// typed; encoding still belongs only to the worker boundary.
pub(super) fn prepare_library_request(library: Rc<LibrarySession>, command: LibraryCommand) -> Result<PreparedRequest, crate::BackendError> {
    Ok(match command {
        LibraryCommand::Scanning => PreparedRequest::Ready(Reply::new(library.scanner_running())),
        LibraryCommand::DownloadState { content_hash } => PreparedRequest::Ready(Reply::new(library.download_state(content_hash)?)),
        LibraryCommand::TransferSnapshot => PreparedRequest::Ready(Reply::new(library.transfer_snapshot()?)),
        LibraryCommand::ImportProgress { activity } => PreparedRequest::Ready(Reply::new(library.directory_import_details_for(activity))),
        LibraryCommand::SyncStatus { signed_in, remote_present } => PreparedRequest::Ready(Reply::new(library.sync_status(signed_in, remote_present)?)),
        LibraryCommand::LocalStorageUsage => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(library.local_storage_usage().await?)) })),
        // Renewal only authorizes the original remote book. It must not reopen
        // metadata or change source type if the user downloaded a local copy.
        LibraryCommand::RenewRemoteAudio { content_hash } => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(library.direct_audio_source(content_hash).await?)) })),
        LibraryCommand::ReadFileRange { source, offset, length } => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(serde_bytes::ByteBuf::from(library.read_file_range(&source, offset, length).await?))) })),
        LibraryCommand::ReadFileBundle { source, ranges } => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(serde_bytes::ByteBuf::from(library.read_file_bundle(&source, ranges).await?))) })),
        LibraryCommand::BookCount => PreparedRequest::Ready(Reply::new(library.book_count()?)),
        LibraryCommand::Home { limit } => PreparedRequest::Ready(Reply::new(library.home(limit, library.downloaded_books_only()?)?)),
        LibraryCommand::Thumbnail { content_hash, resolution } => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(library.thumbnail(content_hash, resolution).await?.map(serde_bytes::ByteBuf::from))) })),
        LibraryCommand::DownloadBook { content_hash } => PreparedRequest::Ready(Reply::new(library.queue_download(content_hash)?)),
        LibraryCommand::DownloadFolder { query } => PreparedRequest::Ready(Reply::new(library.queue_browse_downloads(query, false)?)),
        LibraryCommand::DownloadSubject { query } => PreparedRequest::Ready(Reply::new(library.queue_browse_downloads(query, true)?)),
        LibraryCommand::RemoveLocalBookCopy { content_hash } => PreparedRequest::Ready(Reply::new(library.remove_local_book_copy(content_hash)?)),
        LibraryCommand::EvictFolderDownloads { directory_id } => PreparedRequest::Ready(Reply::new(library.evict_folder_downloads(&directory_id)?)),
        LibraryCommand::FolderDestinations => PreparedRequest::Ready(Reply::new(library.folder_destinations()?)),
        LibraryCommand::Trash => PreparedRequest::Ready(Reply::new(library.trash()?)),
        LibraryCommand::RestoreBook { content_hash } => PreparedRequest::Ready(Reply::new(library.restore_book(&content_hash)?)),
        LibraryCommand::RestoreDirectory { directory_id, parent_id } => PreparedRequest::Ready(Reply::new(library.restore_directory(&directory_id, parent_id.as_ref())?)),
        LibraryCommand::PurgeBook { content_hash } => PreparedRequest::Ready(Reply::new(library.purge_book(&content_hash)?)),
        LibraryCommand::PurgeDirectory { directory_id } => PreparedRequest::Ready(Reply::new(library.purge_directory(&directory_id)?)),
        LibraryCommand::EmptyTrash => PreparedRequest::Ready(Reply::new(library.empty_trash()?)),
        // The new folder identity stays server-side; callers refresh instead.
        LibraryCommand::CreateDirectory { parent_id, name } => PreparedRequest::Ready(Reply::new(library.create_directory(&parent_id, &name).map(|_| ())?)),
        LibraryCommand::RenameDirectory { directory_id, name } => PreparedRequest::Ready(Reply::new(library.queue_directory_move(&directory_id, None, Some(&name))?)),
        LibraryCommand::MoveDirectory { directory_id, parent_id } => PreparedRequest::Ready(Reply::new(library.queue_directory_move(&directory_id, Some(&parent_id), None)?)),
        LibraryCommand::CopyDirectory { directory_id, parent_id } => PreparedRequest::Ready(Reply::new(library.copy_directory(&directory_id, &parent_id)?)),
        LibraryCommand::RestoreBookPlacement { content_hash, directory_id } => PreparedRequest::Ready(Reply::new(library.restore_book_placement(&content_hash, &directory_id)?)),
        LibraryCommand::CopyBookToDirectory { content_hash, source_id, destination_id } => PreparedRequest::Ready(Reply::new(library.copy_book_to_directory(&content_hash, &source_id, &destination_id)?)),
        LibraryCommand::MoveBookToDirectory { content_hash, source_id, destination_id } => PreparedRequest::Ready(Reply::new(library.move_book_to_directory(&content_hash, &source_id, &destination_id)?)),
        LibraryCommand::RemoveBookFromDirectory { content_hash, source_id } => PreparedRequest::Ready(Reply::new(library.remove_book_from_directory(&content_hash, &source_id)?)),
        LibraryCommand::MoveBookToTrash { content_hash } => PreparedRequest::Ready(Reply::new(library.move_book_to_trash(&content_hash)?)),
        LibraryCommand::MoveDirectoryToTrash { directory_id } => PreparedRequest::Ready(Reply::new(library.move_directory_to_trash(&directory_id)?)),
        LibraryCommand::ImportBookBytes { parent_id, file_name, bytes } => PreparedRequest::Pending(OperationKind::Import, Box::pin(async move { Ok(Reply::new(library.import_book_bytes(&parent_id, file_name, bytes).await?)) })),
        // Staging registers the activity first so a failed preparation cannot
        // leave the import counted as running.
        LibraryCommand::PrepareStagedDirectoryImport { activity, job, parent_id, name, directories } => {
            library.begin_directory_import(activity);
            let activity = PreparingImport { library: library.clone(), activity, handed_off: false };
            PreparedRequest::Pending(
                OperationKind::Import,
                Box::pin(async move {
                    let prepared = library.prepare_directory_import_job(&parent_id, name, directories, Some(job)).await?;
                    activity.hand_off();
                    Ok(Reply::new(prepared))
                }),
            )
        }
        LibraryCommand::FinishDirectoryImport { activity } => PreparedRequest::Ready({
            library.finish_directory_import(activity);
            Reply::new(())
        }),
        LibraryCommand::DirectoryImportProgress => PreparedRequest::Ready(Reply::new(library.directory_import_details())),
        LibraryCommand::AdvanceDirectoryImport { activity, total, succeeded, failed } => PreparedRequest::Ready({
            library.advance_directory_import(activity, total, succeeded, failed);
            Reply::new(())
        }),
        LibraryCommand::FolderContents { query } => PreparedRequest::Ready(Reply::new(library.folder_browse_data(&query, library.downloaded_books_only()?)?)),
        LibraryCommand::SubjectContents { query } => PreparedRequest::Ready(Reply::new(library.subject_browse_data(&query, library.downloaded_books_only()?)?)),
        LibraryCommand::LibrarySearch { query } => PreparedRequest::Ready(Reply::new(library.library_search(&query, library.downloaded_books_only()?)?)),
        LibraryCommand::BookDetail { content_hash } => PreparedRequest::Ready(Reply::new(library.book_detail(content_hash)?)),
        LibraryCommand::Authors { sort } => PreparedRequest::Ready(Reply::new(library.authors(sort, library.downloaded_books_only()?)?)),
        LibraryCommand::BookFormat { content_hash } => PreparedRequest::Ready(Reply::new(library.book_format(content_hash)?)),
        LibraryCommand::AudiobookPlaybackMetadata { content_hash } => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(library.audiobook_playback_metadata(content_hash).await?)) })),
        LibraryCommand::ReadingPosition { content_hash } => PreparedRequest::Ready(Reply::new(library.reading_position(content_hash)?)),
        LibraryCommand::PdfReadingPosition { content_hash } => PreparedRequest::Ready(Reply::new(library.pdf_reading_position(content_hash)?)),
        LibraryCommand::UpdateReadingPosition { content_hash, position, progress, entry } => PreparedRequest::Ready(Reply::new(library.update_reading_position(content_hash, &position, progress, entry.as_deref())?)),
        LibraryCommand::Annotations { content_hash } => PreparedRequest::Ready(Reply::new(library.annotations(content_hash)?)),
        LibraryCommand::UpsertAnnotation { annotation } => PreparedRequest::Ready(Reply::new(library.upsert_annotation(&annotation)?)),
        LibraryCommand::DeleteAnnotation { annotation_id, modified_at } => PreparedRequest::Ready(Reply::new(library.delete_annotation(&annotation_id, modified_at)?)),
        #[cfg(target_arch = "wasm32")]
        LibraryCommand::OpenRemoteAudio { content_hash } => PreparedRequest::Pending(OperationKind::Read, Box::pin(async move { Ok(Reply::new(web::open_remote_audio(&library, content_hash).await?)) })),
        #[cfg(target_arch = "wasm32")]
        LibraryCommand::ReadBookRange { capability, offset, length } => PreparedRequest::Ready(Reply::new(web::read_book_range(&library, &capability, offset, length)?)),
        #[cfg(target_arch = "wasm32")]
        LibraryCommand::ImportStagedBook { parent_id, file_name, physical, length, hash } => {
            PreparedRequest::Pending(OperationKind::Import, Box::pin(async move { Ok(Reply::new(library.import_staged_book(parent_id, file_name, physical, length, hash).await?)) }))
        }
        #[cfg(target_arch = "wasm32")]
        LibraryCommand::CleanupStagedImport { job, files } => PreparedRequest::Ready(Reply::new(web::cleanup_staged_import(&library, &job, files)?)),
    })
}

#[cfg(test)]
pub(crate) async fn dispatch_library_request_in_session(library: Rc<LibrarySession>, command: LibraryCommand) -> Result<Reply, crate::BackendError> {
    match prepare_library_request(library, command)? {
        PreparedRequest::Ready(reply) => Ok(reply),
        PreparedRequest::Pending(_, operation) => operation.await,
    }
}
