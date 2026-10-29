//! Transfers across storage identities exercise the real client/actor protocol.
use super::*;
use crate::{
    client::TransferSelection,
    library::cloud::tests::{drive, library_with_storage, scope, seed_provider_book},
    LibraryClient,
};
use book_model::{Annotation, AnnotationAnchor, AnnotationStyle};
use library_database::transfer::BookTransferState;
use std::sync::atomic::Ordering;
use sync_common::{
    storage::{CloudLibraryRoot, LibraryStorage, ProviderId},
    ROOT_DIR_ID,
};

const EPUB: &[u8] = include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");

fn actor(session: LibrarySession) -> (LibraryActor, LibraryClient, async_channel::Receiver<LibraryActorCommand>) {
    let actor = LibraryActor::new(session);
    let (commands, inbox) = async_channel::unbounded();
    let handle = LibraryHandle { id: actor.session.id, commands };
    let client = LibraryClient::new(handle.id, handle);
    (actor, client, inbox)
}

fn selection(book: BookId) -> TransferSelection {
    TransferSelection { folders: Vec::new(), books: vec![book], source: Some(ROOT_DIR_ID) }
}

#[tokio::test]
async fn copying_an_uncached_bokheim_book_while_signed_out_reports_sign_in() {
    let directory = tempfile::tempdir().unwrap();
    let session = library_with_storage(directory.path(), Default::default(), LibraryStorage::Bokheim);
    let book = Box::pin(session.import.import_downloaded_book(ROOT_DIR_ID, "Book.epub", EPUB.to_vec())).await.unwrap();
    let before = session.db.storage_copy_plan().unwrap();
    std::fs::remove_file(directory.path().join("Book.epub")).unwrap();
    let error = session.export_transfer_book(book).await.err().expect("uncached books require the Bokheim session");
    assert_eq!(error.failure(), Some(client_runtime::Failure::SignInRequired));
    assert_eq!(session.db.storage_copy_plan().unwrap(), before);
}
fn provider_storage(folder: &str) -> LibraryStorage {
    LibraryStorage::External(CloudLibraryRoot { scope: scope(), folder_id: ProviderId::new(folder).unwrap() })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_out_provider_download_is_queued_through_the_client_command() {
    let directory = tempfile::tempdir().unwrap();
    let mock = drive(EPUB.to_vec(), "Book.epub").await;
    let session = library_with_storage(directory.path(), mock.cloud.clone(), provider_storage("folder"));
    let id = seed_provider_book(&session, &mock).await;
    let (mut actor, client, inbox) = actor(session);
    let library = actor.session.clone();
    let exercise = Box::pin(async {
        let state: library_model::DownloadState = client.request(LibraryCommand::Ready(crate::library::ReadyCommand::DownloadBook { content_hash: id })).await.unwrap();
        assert_eq!(state, library_model::DownloadState::Queued);
        assert!(library.db.provider_download_snapshot(id).unwrap().is_some());
        assert!(library.db.pending_book_uploads(0, 10).unwrap().is_empty());
        client.transport.handle().await.unwrap().shutdown();
    });
    tokio::join!(Box::pin(actor.serve(inbox)), exercise);
}

fn progress(session: &LibrarySession, id: BookId) {
    let mut targets = Vec::new();
    fn leaves(entries: &[book_model::BookTocEntry], targets: &mut Vec<String>) {
        for entry in entries {
            if entry.children.is_empty() {
                targets.push(entry.target.clone());
            } else {
                leaves(&entry.children, targets);
            }
        }
    }
    if let Some(navigation) = &session.db.book_transfer_state(id).unwrap().addition.navigation {
        leaves(&navigation.entries, &mut targets);
    }
    session.db.update_reading_position(&id, "epubcfi(/6/2)", Some(42.0), targets.first().map(String::as_str)).unwrap();
    if let Some(target) = targets.get(1) {
        session.db.update_reading_position(&id, "epubcfi(/6/4)", Some(48.0), Some(target)).unwrap();
    }
    session
        .db
        .upsert_annotation(&Annotation {
            id: "provider-transfer-note".into(),
            content_hash: id,
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
            exact_text: "Text".into(),
            style: AnnotationStyle::Underline,
            color: 0xffffff,
            note: "Retain this note".into(),
            created_at: 7,
            toc_ordinal: None,
            progress: Some(42.0),
            modified_at: 9,
            deleted: false,
        })
        .unwrap();
}

fn assert_progress(source: &BookTransferState, destination: &LibrarySession, id: BookId) {
    let expected = source.clone().retarget(id).unwrap();
    let actual = destination.db.book_transfer_state(id).unwrap();
    assert_eq!(actual.addition, expected.addition);
    assert_eq!(actual.reading, expected.reading);
    assert_eq!(actual.chapter_read, expected.chapter_read);
    assert_eq!(actual.annotations, expected.annotations);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_to_bokheim_to_another_provider_keeps_progress_and_destination_undo_ids() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let third = tempfile::tempdir().unwrap();
    let source_drive = drive(EPUB.to_vec(), "Book.epub").await;
    let destination_drive = drive(Vec::new(), "Unused.epub").await;
    let source_session = library_with_storage(first.path(), source_drive.cloud.clone(), provider_storage("folder"));
    let original_id = seed_provider_book(&source_session, &source_drive).await;
    progress(&source_session, original_id);
    let original = source_session.db.book_transfer_state(original_id).unwrap();
    let plain_session = library_with_storage(second.path(), Default::default(), LibraryStorage::Bokheim);
    let provider_session = library_with_storage(third.path(), destination_drive.cloud.clone(), provider_storage("destination"));
    let (mut source_actor, source, source_inbox) = actor(source_session);
    let (mut plain_actor, plain, plain_inbox) = actor(plain_session);
    let (mut provider_actor, provider, provider_inbox) = actor(provider_session);
    let src = source_actor.session.clone();
    let plain_library = plain_actor.session.clone();
    let provider_library = provider_actor.session.clone();
    let exercise = Box::pin(async {
        let copied = plain.transfer_from(&source, selection(original_id), ROOT_DIR_ID, false).await;
        assert!(copied.error.is_none(), "{:?}", copied.error);
        let byte_id = BookId::from(blake3::hash(EPUB));
        assert_ne!(byte_id, original_id);
        assert_eq!(copied.created_placements, vec![(byte_id, ROOT_DIR_ID)]);
        assert_progress(&original, &plain_library, byte_id);
        let moved = provider.transfer_from(&plain, selection(byte_id), ROOT_DIR_ID, true).await;
        assert!(moved.error.is_none(), "{:?}", moved.error);
        let new_id = moved.created_placements[0].0;
        assert!(new_id.is_external());
        assert_ne!(new_id, original_id);
        assert_ne!(new_id, byte_id);
        assert_eq!(moved.removed_placements, vec![(byte_id, ROOT_DIR_ID)]);
        assert_progress(&original, &provider_library, new_id);
        assert!(plain_library.db.transfer_destination_name(&byte_id, &ROOT_DIR_ID).unwrap().is_none());
        assert_eq!(provider_library.db.pending_provider_upload_count().unwrap(), 0);
        assert!(provider_library.db.pending_book_uploads(0, 100).unwrap().is_empty());
        // Undo targets the new destination identity while restoring the source's
        // original identity and independent reading state.
        provider_library.placement.remove_book_from_directory(&new_id, &ROOT_DIR_ID).unwrap();
        plain_library.placement.restore_book_placement(&byte_id, &ROOT_DIR_ID).await.unwrap();
        assert_progress(&original, &plain_library, byte_id);
        assert_progress(&original, &src, original_id);
        assert!(source_drive.requests.lock().unwrap().iter().all(|request| request.starts_with("GET ")));
        source.transport.handle().await.unwrap().shutdown();
        plain.transport.handle().await.unwrap().shutdown();
        provider.transport.handle().await.unwrap().shutdown();
    });
    tokio::join!(Box::pin(source_actor.serve(source_inbox)), Box::pin(plain_actor.serve(plain_inbox)), Box::pin(provider_actor.serve(provider_inbox)), exercise);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_provider_move_keeps_the_source_and_retry_preserves_reader_state() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let source_session = library_with_storage(first.path(), Default::default(), LibraryStorage::Local);
    let id = Box::pin(source_session.import.import_downloaded_book(ROOT_DIR_ID, "Book.epub", EPUB.to_vec())).await.unwrap();
    progress(&source_session, id);
    let original = source_session.db.book_transfer_state(id).unwrap();
    let destination_drive = drive(Vec::new(), "Unused.epub").await;
    destination_drive.fail_upload.store(true, Ordering::SeqCst);
    let destination_session = library_with_storage(second.path(), destination_drive.cloud.clone(), provider_storage("destination"));
    let (mut source_actor, source, source_inbox) = actor(source_session);
    let (mut destination_actor, destination, destination_inbox) = actor(destination_session);
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let exercise = Box::pin(async {
        let failed = destination.transfer_from(&source, selection(id), ROOT_DIR_ID, true).await;
        assert!(failed.error.is_some());
        assert!(failed.removed_placements.is_empty());
        assert!(!src.db.book_paths(&id).unwrap().is_empty());
        assert!(first.path().join("Book.epub").exists());
        assert!(dst.db.provider_remote_books().unwrap().is_empty());
        assert_eq!(dst.db.pending_provider_upload_count().unwrap(), 1);
        destination_drive.fail_upload.store(false, Ordering::SeqCst);
        let retried = destination.transfer_from(&source, selection(id), ROOT_DIR_ID, true).await;
        assert!(retried.error.is_none(), "{:?}", retried.error);
        let new_id = retried.created_placements[0].0;
        let actual = dst.db.book_transfer_state(new_id).unwrap();
        let expected = original.clone().retarget(new_id).unwrap();
        assert_eq!(actual.addition.record, expected.addition.record);
        assert_eq!(actual.reading, expected.reading);
        assert_eq!(actual.chapter_read, expected.chapter_read);
        assert_eq!(actual.annotations, expected.annotations);
        assert_eq!(retried.removed_placements, vec![(id, ROOT_DIR_ID)]);
        assert_eq!(dst.db.pending_provider_upload_count().unwrap(), 0);
        source.transport.handle().await.unwrap().shutdown();
        destination.transport.handle().await.unwrap().shutdown();
    });
    tokio::join!(Box::pin(source_actor.serve(source_inbox)), Box::pin(destination_actor.serve(destination_inbox)), exercise);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_provider_revision_or_wrong_checksum_cannot_remove_a_move_source() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let mock = drive(EPUB.to_vec(), "Book.epub").await;
    let source_session = library_with_storage(first.path(), mock.cloud.clone(), provider_storage("folder"));
    let id = seed_provider_book(&source_session, &mock).await;
    let original = source_session.db.book_transfer_state(id).unwrap();
    let destination_session = library_with_storage(second.path(), Default::default(), LibraryStorage::Local);
    let (mut source_actor, source, source_inbox) = actor(source_session);
    let (mut destination_actor, destination, destination_inbox) = actor(destination_session);
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let exercise = Box::pin(async {
        mock.revision.store(2, Ordering::SeqCst);
        let obsolete = destination.transfer_from(&source, selection(id), ROOT_DIR_ID, true).await;
        assert!(obsolete.error.is_some());
        assert!(obsolete.created_placements.is_empty());
        assert!(obsolete.removed_placements.is_empty());
        assert!(src.db.transfer_destination_name(&id, &ROOT_DIR_ID).unwrap().is_some());

        mock.revision.store(1, Ordering::SeqCst);
        let mut corrupt = mock.file.clone();
        corrupt.content_hash = Some(blake3::hash(b"wrong bytes").into());
        src.db.admit_external_book(ROOT_DIR_ID, library_replica::ExternalBook { file: corrupt, addition: original.addition.clone(), thumbnails: None }).unwrap();
        let mismatch = destination.transfer_from(&source, selection(id), ROOT_DIR_ID, true).await;
        assert!(mismatch.error.is_some());
        assert!(mismatch.created_placements.is_empty());
        assert!(mismatch.removed_placements.is_empty());
        assert!(src.db.transfer_destination_name(&id, &ROOT_DIR_ID).unwrap().is_some());
        assert!(dst.db.transfer_destination_name(&BookId::from(blake3::hash(EPUB)), &ROOT_DIR_ID).unwrap().is_none());
        assert!(dst.db.pending_book_uploads(0, 100).unwrap().is_empty());
        assert_eq!(dst.db.pending_provider_upload_count().unwrap(), 0);
        source.transport.handle().await.unwrap().shutdown();
        destination.transport.handle().await.unwrap().shutdown();
    });
    tokio::join!(Box::pin(source_actor.serve(source_inbox)), Box::pin(destination_actor.serve(destination_inbox)), exercise);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storage_copy_resumes_without_writing_to_the_provider_or_changing_the_source() {
    use crate::library::StorageCopyProgress;
    use client_runtime::Failure;
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let mock = drive(EPUB.to_vec(), "Book.epub").await;
    let source_session = library_with_storage(first.path(), mock.cloud.clone(), provider_storage("folder"));
    let original_id = seed_provider_book(&source_session, &mock).await;
    progress(&source_session, original_id);
    let original = source_session.db.book_transfer_state(original_id).unwrap();
    let folder = source_session.placement.create_directory(&ROOT_DIR_ID, "Books").unwrap().id;
    source_session.placement.create_directory(&folder, "Empty").unwrap();
    source_session.placement.copy_book_to_directory(&original_id, Some(&ROOT_DIR_ID), &folder).await.unwrap();
    let original_plan = source_session.db.storage_copy_plan().unwrap();
    let destination_session = library_with_storage(second.path(), Default::default(), LibraryStorage::Local);
    let (mut source_actor, source, source_inbox) = actor(source_session);
    let (mut destination_actor, destination, destination_inbox) = actor(destination_session);
    let src = source_actor.session.clone();
    let dst = destination_actor.session.clone();
    let exercise = Box::pin(async {
        let src_handle = source.transport.handle().await.unwrap();
        let dst_handle = destination.transport.handle().await.unwrap();
        let mut saved = StorageCopyProgress::default();
        let failed = dst_handle
            .copy_storage_from(src_handle, saved.clone(), |progress| {
                saved = progress.clone();
                Err(BackendError::classified(Failure::StorageFull, "fixture checkpoint disk is full"))
            })
            .await
            .unwrap_err();
        assert_eq!(failed.failure(), Some(Failure::StorageFull));
        assert_eq!(saved.books.len(), 1);
        assert_eq!(src.db.storage_copy_plan().unwrap(), original_plan);
        dst_handle
            .copy_storage_from(src_handle, saved.clone(), |progress| {
                saved = progress.clone();
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(saved.books.len(), 2);
        let byte_id = BookId::from(blake3::hash(EPUB));
        assert!(saved.books.iter().all(|book| book.destination == byte_id));
        assert_progress(&original, &dst, byte_id);
        assert_progress(&original, &src, original_id);
        assert_eq!(dst.db.folder_destinations().unwrap().len(), 3, "empty folders must be copied too");
        assert_eq!(dst.db.storage_copy_plan().unwrap().books.len(), 2);
        assert_eq!(src.db.storage_copy_plan().unwrap(), original_plan);
        let requests = mock.requests.lock().unwrap().len();
        dst_handle.copy_storage_from(src_handle, saved, |_| Ok(())).await.unwrap();
        assert_eq!(mock.requests.lock().unwrap().len(), requests, "completed copies must not be downloaded again");
        assert!(mock.requests.lock().unwrap().iter().all(|request| request.starts_with("GET ")));
        src_handle.shutdown();
        dst_handle.shutdown();
    });
    tokio::join!(Box::pin(source_actor.serve(source_inbox)), Box::pin(destination_actor.serve(destination_inbox)), exercise);
}
