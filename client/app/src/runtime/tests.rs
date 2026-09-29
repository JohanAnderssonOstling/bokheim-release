use super::*;
use std::io::Write;

async fn after_startup_scan(library: &library_backend::LibraryClient) -> async_channel::Receiver<library_backend::LibraryUpdate> {
    let (events, scanning) = library.updates().await.unwrap();
    if scanning {
        tokio::time::timeout(std::time::Duration::from_secs(5), async { while !matches!(events.recv().await.unwrap(), library_backend::LibraryUpdate::Scanning(false)) {} }).await.unwrap();
    }
    while events.try_recv().is_ok() {}
    events
}

fn epub() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in [
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#),
        (
            "content.opf",
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
        ),
        ("chapter.xhtml", r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>Shared handler test</body></html>"#),
    ] {
        zip.start_file(path, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

async fn wire_value<T: serde::de::DeserializeOwned>(backend: &AppBackend, command: impl Into<WorkerCommand>) -> T {
    let WorkerDispatchResult::Response(WorkerPayload(bytes)) = dispatch_worker_request(backend.clone(), AppWorkerRequest { id: 1, command: command.into() }).await.unwrap() else { panic!("expected metadata response") };
    decode_worker_message(&bytes).unwrap()
}

#[tokio::test]
async fn native_and_wire_handlers_preserve_imports_reads_and_subscriptions() {
    let root = tempfile::tempdir().unwrap();
    let context = crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let backend = AppBackend::new(context.clone()).unwrap();
    let entry = backend.ensure_import_library().unwrap();
    let id = *entry.library_id();
    let (client, worker) = native::channel(&backend);
    let requests = async {
        let library = client.library(id);
        let bytes = epub();
        let hash = library.import_book_bytes(crate::ROOT_DIR_ID, "book.epub".into(), bytes.clone()).await.unwrap();
        assert_eq!(library.book_count().await.unwrap(), 1);
        let annotation = library_backend::ReaderAnnotation {
            id: uuid::Uuid::new_v4().to_string(),
            content_hash: hash,
            anchor: library_backend::AnnotationAnchor::epub_cfi("epubcfi(/6/2!/4/2:0)"),
            exact_text: "A passage".into(),
            style: library_backend::AnnotationStyle::Highlight,
            color: "yellow".into(),
            note: "Library-owned annotation".into(),
            created_at: 1,
            modified_at: 1,
            toc_ordinal: Some(0),
            progress: Some(0.5),
        };
        library.upsert_annotation(annotation.clone()).await.unwrap();
        let stored = library.annotations(hash).await.unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap(), serde_json::to_value(vec![annotation.clone()]).unwrap());
        let wire_annotations: Vec<library_backend::ReaderAnnotation> = wire_value(&backend, WorkerCommand::Library { library_id: id, command: LibraryCommand::Annotations { content_hash: hash } }).await;
        assert_eq!(serde_json::to_value(wire_annotations).unwrap(), serde_json::to_value(stored).unwrap());
        library.delete_annotation(annotation.id, 2).await.unwrap();
        assert!(library.annotations(hash).await.unwrap().is_empty());

        // The generic position endpoint serves every reader, not just EPUB.
        // Exercise both transports; an EPUB-only projection silently loses
        // valid audiobook/PDF resume points while writes still succeed.
        for position in ["epubcfi(/6/2!/4/2:0)", "pdfpage(3)", "audiopos(75000)"] {
            library.update_reading_position(hash, position.to_owned(), Some(0.5), None).await.unwrap();
            assert_eq!(library.reading_position(hash).await.unwrap().as_deref(), Some(position));
            let stored: Option<String> = wire_value(&backend, WorkerCommand::Library { library_id: id, command: LibraryCommand::ReadingPosition { content_hash: hash } }).await;
            assert_eq!(stored.as_deref(), Some(position));
        }
        let wire_count: usize = wire_value(&backend, WorkerCommand::Library { library_id: id, command: LibraryCommand::BookCount }).await;
        assert_eq!(wire_count, 1);
        let native_home = library.home(10).await.unwrap();
        let wire_home: library_backend::LibraryHomeView = wire_value(&backend, WorkerCommand::Library { library_id: id, command: LibraryCommand::Home { limit: 10 } }).await;
        assert_eq!(serde_json::to_value(native_home).unwrap(), serde_json::to_value(wire_home).unwrap());

        // Book opening is a native transport operation, retaining its reader and placement path.
        let mut native_book = library.resolve_book(hash).await.unwrap().into_document().unwrap();
        assert!(!native_book.path.is_empty());
        let mut read = Vec::new();
        native_book.reader.read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
        assert!(library.thumbnail(hash, ThumbnailResolution::Browse).await.unwrap().is_none());

        let (native_events, native_initial) = library.download_changes(hash).await.unwrap();
        let WorkerDispatchResult::Subscription { initial: WorkerPayload(initial), events: WorkerSubscription::Library(LibraryEvents::DownloadStates(wire_events)) } =
            dispatch_worker_request(backend.clone(), AppWorkerRequest { id: 2, command: WorkerCommand::LibrarySubscription { library_id: id, subscription: LibrarySubscription::BookDownload { content_hash: hash } } }).await.unwrap()
        else {
            panic!("expected download subscription")
        };
        assert_eq!(native_initial, crate::DownloadState::Downloaded);
        assert_eq!(decode_worker_message::<crate::DownloadState>(&initial).unwrap(), native_initial);
        drop((native_events, wire_events));
        // Exercise live delivery through both transports without bypassing
        // library ownership or depending on authenticated download policy.
        let (native_events, _) = library.updates().await.unwrap();
        let WorkerDispatchResult::Subscription { events: WorkerSubscription::Library(LibraryEvents::Updates(wire_events)), .. } =
            dispatch_worker_request(backend.clone(), AppWorkerRequest { id: 3, command: WorkerCommand::LibrarySubscription { library_id: id, subscription: LibrarySubscription::Updates } }).await.unwrap()
        else {
            panic!("expected library subscription")
        };
        library.create_directory(crate::ROOT_DIR_ID, "Events".into()).await.unwrap();
        let timeout = std::time::Duration::from_secs(2);
        for events in [&native_events, &wire_events] {
            tokio::time::timeout(timeout, async { while !matches!(events.recv().await.unwrap(), library_backend::LibraryUpdate::Contents) {} }).await.unwrap();
        }

        // Both transports use the same validation and error path.
        let missing = test_support::fixture_dir_id("missing");
        let native_error = library.rename_directory(missing, "Renamed".into()).await.unwrap_err();
        let wire_result =
            dispatch_worker_request(backend.clone(), AppWorkerRequest { id: 3, command: WorkerCommand::Library { library_id: id, command: LibraryCommand::RenameDirectory { directory_id: missing, name: "Renamed".into() } } }).await;
        assert_eq!(wire_result.err().unwrap().to_string(), native_error);
        drop(library);
        drop(client);
    };
    let ((), ()) = tokio::join!(worker.serve(backend.clone()), requests);
}

#[tokio::test]
async fn independent_operations_start_together_and_keep_settings_responsive() {
    let root = tempfile::tempdir().unwrap();
    let launch = crate::BackendLaunch::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let (client, _) = AppClient::start_native(launch).unwrap();
    let (started, starts) = async_channel::unbounded();
    let (release, released) = async_channel::unbounded();
    let jobs = (0..9).map(|_| {
        let client = client.clone();
        let started = started.clone();
        let released = released.clone();
        async move {
            client
                .transport
                .run(move |_| async move {
                    started.send(()).await.unwrap();
                    released.recv().await.unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        }
    });
    let settings = async {
        for _ in 0..9 {
            tokio::time::timeout(std::time::Duration::from_secs(3), starts.recv()).await.unwrap().unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), client.update_reader_preferences(vec![app_preferences::ReaderPreferencePatch::Zoom(1.7)])).await.expect("settings must not wait for unrelated operations").unwrap();
        for _ in 0..9 {
            release.send(()).await.unwrap();
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(futures_util::future::join_all(jobs), settings);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn single_book_import_preserves_the_permission_failure_for_the_ui() {
    let root = tempfile::tempdir().unwrap();
    let context = crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let backend = AppBackend::new(context).unwrap();
    let id = *backend.ensure_import_library().unwrap().library_id();
    let (client, worker) = native::channel(&backend);
    let requests = async {
        let library = client.library(id);
        let cause = "could not read the selected book: Permission denied (os error 13)";
        let source = crate::DirectoryImport {
            name: "selected.epub".into(),
            directories: vec![],
            files: vec![crate::DirectoryImportFile { path: vec!["selected.epub".into()], source: crate::ImportSource::new(Box::new(move || Box::pin(async move { Err(cause.into()) }))) }],
        };
        let failures = library.import_directory_contents(crate::ROOT_DIR_ID, source).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].diagnostic.as_ref().is_some_and(|diagnostic| diagnostic.contains(cause)), "the UI must receive the failed operation: {failures:?}");
        assert!(library.directory_import_progress().await.unwrap().is_none());
        drop(library);
        drop(client);
    };
    tokio::join!(worker.serve(backend), requests);
}

#[tokio::test]
async fn directory_import_opens_files_only_after_previous_import_finishes() {
    let root = tempfile::tempdir().unwrap();
    let context = crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let backend = AppBackend::new(context).unwrap();
    let entry = backend.ensure_import_library().unwrap();
    let id = *entry.library_id();
    let (client, worker) = native::channel(&backend);
    let requests = async {
        let library = client.library(id);
        let updates = after_startup_scan(&library).await;
        let initially_scanning = false;
        let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut files = Vec::new();
        for index in 0..3 {
            let library = library.clone();
            let opened = opened.clone();
            let progress_client = client.clone();
            files.push(crate::DirectoryImportFile {
                path: vec!["Shelf".into(), format!("{index}.epub")],
                source: crate::ImportSource::new(Box::new(move || {
                    Box::pin(async move {
                        // Eager reading would open the second file while the
                        // first still has not reached the database.
                        assert_eq!(library.book_count().await.unwrap(), index);
                        assert!(library.updates().await.unwrap().1, "late subscribers must see an active folder import");
                        let groups = progress_client.transfers().await.unwrap();
                        let progress = groups.iter().find(|group| group.library_id == id).unwrap().scan_progress.unwrap();
                        assert_eq!(progress.total, 5, "unsupported files must not count toward progress");
                        opened.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let mut bytes = epub();
                        bytes.push(index as u8); // Distinct hashes, valid ZIPs.
                        Ok(Box::new(std::io::Cursor::new(bytes)) as Box<dyn std::io::Read + Send>)
                    })
                })),
            });
        }
        files.push(crate::DirectoryImportFile { path: vec!["notes.txt".into()], source: crate::ImportSource::new(Box::new(|| panic!("unsupported files must never be opened"))) });
        files.push(crate::DirectoryImportFile { path: vec!["Missing".into(), "book.epub".into()], source: crate::ImportSource::new(Box::new(|| panic!("files with missing parents must never be opened"))) });
        files.push(crate::DirectoryImportFile { path: vec!["Shelf".into(), "3-unreadable.epub".into()], source: crate::ImportSource::new(Box::new(|| Box::pin(async { Err("fixture read failure".into()) }))) });
        let source = crate::DirectoryImport { name: "Selected library".into(), directories: vec![vec!["Shelf".into()], vec!["Empty".into()], vec!["Shelf".into()]], files };
        let failures = library.import_directory_contents(crate::ROOT_DIR_ID, source).await.unwrap();
        loop {
            if matches!(updates.recv().await.unwrap(), library_backend::LibraryUpdate::Contents) {
                break;
            }
        }
        assert!(!std::iter::from_fn(|| updates.try_recv().ok()).any(|update| matches!(update, library_backend::LibraryUpdate::Contents)), "one completion notification per folder import");
        assert_eq!(library.updates().await.unwrap().1, initially_scanning, "completed import must preserve the filesystem scanner state");
        assert!(client.transfers().await.unwrap().iter().find(|group| group.library_id == id).unwrap().scan_progress.is_none(), "a parked filesystem scanner must not appear as active progress after the explicit import finishes");
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(library.book_count().await.unwrap(), 3);
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().any(|failure| failure.kind == crate::ImportFailureKind::File && failure.path.last().is_some_and(|name| name == "book.epub")));
        assert!(failures.iter().any(|failure| failure.kind == crate::ImportFailureKind::File && failure.path.last().is_some_and(|name| name == "3-unreadable.epub")));
        let folders = library.folder_destinations().await.unwrap();
        assert_eq!(folders.iter().filter(|folder| folder.label == "Shelf").count(), 1);
        assert!(folders.iter().any(|folder| folder.label == "Empty"));
        assert!(!folders.iter().any(|folder| folder.label == "Selected library"), "a library import must not add an extra root folder");
        library.import_directory(crate::ROOT_DIR_ID, crate::DirectoryImport { name: "Added folder".into(), directories: vec![], files: vec![] }).await.unwrap();
        assert!(library.folder_destinations().await.unwrap().iter().any(|folder| folder.label == "Added folder"));
        let target = library.folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Added folder").unwrap().id;
        let mut single = epub();
        single.extend_from_slice(b"single file fixture");
        let source = crate::DirectoryImport {
            name: "single.epub".into(),
            directories: vec![],
            files: vec![crate::DirectoryImportFile {
                path: vec!["single.epub".into()],
                source: crate::ImportSource::new(Box::new(move || Box::pin(async move { Ok(Box::new(std::io::Cursor::new(single)) as crate::DirectoryImportReader) }))),
            }],
        };
        assert!(library.import_directory_contents(target, source).await.unwrap().is_empty());
        assert_eq!(library.book_count().await.unwrap(), 4);
        let contents = library
            .folder_contents(library_backend::LibraryBrowseQuery {
                location: target.to_string(),
                search: String::new(),
                file_types: vec![],
                languages: vec![],
                chip_sort: Default::default(),
                book_sort: Default::default(),
                hide_finished: false,
                include_direct_child_books: false,
            })
            .await
            .unwrap();
        assert_eq!(contents.contents.books.len(), 1, "the single file belongs to the selected destination");
        let folders = library.folder_destinations().await.unwrap();
        assert!(!folders.iter().any(|folder| folder.label == "single.epub"), "adding a single file must not create a folder");
        drop(library);
        drop(client);
    };
    tokio::join!(worker.serve(backend), requests);
}

#[test]
fn import_book_wire_bytes_are_binary_and_round_trip() {
    let bytes = vec![255; 1024 * 1024];
    let message = LibraryCommand::ImportBookBytes { parent_id: crate::ROOT_DIR_ID, file_name: "book.epub".into(), bytes: bytes.clone() };
    let wire = encode_worker_message(&message).unwrap();
    assert!(wire.len() < bytes.len() + 256, "book bytes must not expand into individual MessagePack integers");
    let LibraryCommand::ImportBookBytes { bytes: decoded, .. } = decode_worker_message(&wire).unwrap() else { panic!("wrong command") };
    assert_eq!(decoded, bytes);
}

#[tokio::test]
async fn concurrent_first_imports_share_one_destination() {
    let root = tempfile::tempdir().unwrap();
    let launch = crate::BackendLaunch::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let (client, startup) = AppClient::start_native(launch).unwrap();
    assert!(startup.libraries.is_empty());
    let (first, second) = tokio::join!(client.library_for_import(None), client.library_for_import(None));
    assert_eq!(first.unwrap().id(), second.unwrap().id());
    assert_eq!(client.libraries().await.unwrap().len(), 1);
}

#[tokio::test]
async fn library_rename_persists_metadata_through_worker_transport() {
    let root = tempfile::tempdir().unwrap();
    let context = crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let backend = AppBackend::new(context).unwrap();
    let entry = backend.ensure_import_library().unwrap();
    let id = *entry.library_id();
    let changes = backend.library_list_changes();
    let _: () = wire_value(&backend, AppCommand::RenameLibrary { library_id: id, name: "  Renamed books  ".into() }).await;
    assert!(changes.try_recv().is_ok());
    let entries: Vec<crate::LibraryEntry> = wire_value(&backend, AppCommand::Libraries).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(*entries[0].library_id(), id);
    assert_eq!(entries[0].library_name(), "Renamed books");
    assert_eq!(entries[0].storage_locator(), entry.storage_locator());
    let manifest = library_registry::read_library_manifest(std::path::Path::new(entry.storage_locator())).unwrap().unwrap();
    assert_eq!(manifest, (id, "Renamed books".into()));
    assert!(backend.rename_library(&id, "  ").is_err());
    assert!(backend.rename_library(&crate::LibraryId::new_v4(), "Missing").is_err());
    assert_eq!(backend.libraries().unwrap()[0].library_name(), "Renamed books");
}

#[tokio::test]
async fn waiting_requests_do_not_starve_local_commands() {
    let root = tempfile::tempdir().unwrap();
    let context = crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap();
    let backend = AppBackend::new(context).unwrap();
    let (client, worker) = native::channel(&backend);
    let (started, starts) = async_channel::unbounded();
    let (release, released) = async_channel::unbounded::<()>();
    let requests = async move {
        let slow = (0..8).map(|_| {
            let started = started.clone();
            let released = released.clone();
            client.transport.run(move |_| async move {
                started.send(()).await.unwrap();
                released.recv().await.unwrap();
                Ok(())
            })
        });
        let local = async {
            for _ in 0..8 {
                starts.recv().await.unwrap();
            }
            // Library return can register background updates and read local data
            // while every remote operation is still stalled.
            let notifications = client.notification_interest(true).await.unwrap();
            drop(notifications);
            // All eight operations stay suspended until the local read returns.
            let libraries = client.transport.run(|backend| async move { backend.libraries() }).await.unwrap();
            assert!(libraries.is_empty());
            for _ in 0..8 {
                release.send(()).await.unwrap();
            }
        };
        let (results, ()) = tokio::join!(futures_util::future::join_all(slow), local);
        for result in results {
            result.unwrap();
        }
        drop(client);
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(worker.serve(backend), requests);
    })
    .await
    .expect("waiting requests blocked the local command");
}

#[tokio::test]
async fn directory_import_cancellation_waits_for_the_in_flight_file() {
    struct GatedReader {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        resume: std::sync::mpsc::Receiver<()>,
        bytes: std::io::Cursor<Vec<u8>>,
    }
    impl std::io::Read for GatedReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
                self.resume.recv_timeout(std::time::Duration::from_secs(10)).map_err(std::io::Error::other)?;
            }
            std::io::Read::read(&mut self.bytes, output)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let backend = AppBackend::new(crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap()).unwrap();
    let id = *backend.ensure_import_library().unwrap().library_id();
    let (client, worker) = native::channel(&backend);
    let requests = async {
        let library = client.library(id);
        let events = after_startup_scan(&library).await;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (resume, blocked) = std::sync::mpsc::channel();
        let reader = GatedReader { started: Some(started), resume: blocked, bytes: std::io::Cursor::new(epub()) };
        let source = crate::DirectoryImport {
            name: "Cancelled selection".into(),
            directories: vec![],
            files: vec![
                crate::DirectoryImportFile { path: vec!["a.epub".into()], source: crate::ImportSource::new(Box::new(move || Box::pin(async move { Ok(Box::new(reader) as crate::DirectoryImportReader) }))) },
                crate::DirectoryImportFile { path: vec!["b.epub".into()], source: crate::ImportSource::new(Box::new(|| panic!("cancellation must stop before the next file"))) },
            ],
        };
        let mut importing = Box::pin(library.import_directory_contents(crate::ROOT_DIR_ID, source));
        tokio::select! {
            result = &mut importing => panic!("import completed before release: {result:?}"),
            result = ready => result.unwrap(),
        }
        drop(importing);
        assert!(library.directory_import_progress().await.unwrap().is_some(), "cancelling the caller must not release an in-flight write");
        assert_eq!(library.book_count().await.unwrap(), 0);
        assert!(!std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event, library_backend::LibraryUpdate::Contents)));
        resume.send(()).unwrap();
        while !matches!(events.recv().await.unwrap(), library_backend::LibraryUpdate::Contents) {}
        assert!(library.directory_import_progress().await.unwrap().is_none());
        assert_eq!(library.book_count().await.unwrap(), 1, "the in-flight file must commit before completion is published");
        drop(library);
        drop(client);
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::join!(worker.serve(backend), requests);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn directory_import_panicking_source_releases_owned_activity() {
    let root = tempfile::tempdir().unwrap();
    let backend = AppBackend::new(crate::BackendContext::initialize(crate::AppDataLocation::native_path(root.path())).unwrap()).unwrap();
    let id = *backend.ensure_import_library().unwrap().library_id();
    let (client, worker) = native::channel(&backend);
    let requests = async {
        let library = client.library(id);
        let source = crate::DirectoryImport {
            name: "Broken source".into(),
            directories: vec![],
            files: vec![crate::DirectoryImportFile { path: vec!["book.epub".into()], source: crate::ImportSource::new(Box::new(|| panic!("source construction failed"))) }],
        };
        assert!(library.import_directory_contents(crate::ROOT_DIR_ID, source).await.is_err());
        assert!(library.directory_import_progress().await.unwrap().is_none());
        assert_eq!(library.book_count().await.unwrap(), 0);
        drop(library);
        drop(client);
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(worker.serve(backend), requests);
    })
    .await
    .unwrap();
}
