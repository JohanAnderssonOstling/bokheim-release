use super::*;
use book_model::{BookDetails, BookFormat, BookRecord};
use cloud_storage::{
    connections::{CloudConnections, ConnectionPersistence, GoogleConnection},
    oauth::{GoogleAccount, GoogleOAuth, GoogleOAuthConfig, GoogleTokens},
};
use library_database::Database;
use library_files::AssetStore;
use library_placement::LibraryPlacement;
use library_replica::LibraryStateEntry;
use library_runtime::{events::LibraryEventRouter, scheduler::SyncScheduler};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use sync_common::{
    ROOT_DIR_ID,
    storage::{CloudProvider, LibraryStorage, ProviderScope},
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use url::Url;

fn root() -> CloudLibraryRoot {
    CloudLibraryRoot { scope: ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() }, folder_id: ProviderId::new("folder").unwrap() }
}

fn importer(path: &Path, library: Uuid) -> LibraryImport {
    let database = Arc::new(Database::open(path.join("library.sqlite").to_str().unwrap()).unwrap());
    database.initialize_library().unwrap();
    database.set_storage_policy(&LibraryStorage::External(root()), false).unwrap();
    let download = path.join("downloads");
    std::fs::create_dir_all(&download).unwrap();
    let assets = AssetStore::open(download.to_str().unwrap()).unwrap();
    let (sync, _) = SyncScheduler::new();
    let (assets_work, _) = SyncScheduler::new();
    let (files, _) = SyncScheduler::new();
    let (enrichment, _) = SyncScheduler::new();
    let (events, _) = LibraryEventRouter::new(sync, assets_work, files, enrichment);
    let placement = Arc::new(LibraryPlacement { database: database.clone(), assets: assets.clone(), events: events.clone() });
    LibraryImport {
        library_id: library,
        cloud: Default::default(),
        provider_upload_lock: Default::default(),
        database,
        assets,
        cpu: Arc::new(client_platform_native::cpu_host::NativeHost),
        events,
        placement,
        root: download.to_str().unwrap().into(),
    }
}

async fn queued(importer: &LibraryImport, bytes: &[u8], parent: DirId) -> ProviderUploadDraft {
    let copied = importer.assets.stage_book(library_model::RelativeBookPath::parse("Book.m4b").unwrap(), Box::new(Cursor::new(bytes.to_vec()))).await.unwrap();
    let checksum = BookId::from(content_address::hash_reader(&mut Cursor::new(bytes)).unwrap());
    let draft = ProviderUploadDraft {
        id: ProviderUploadDraft::identity(parent, "Book.m4b", checksum),
        library_id: importer.library_id,
        root: root(),
        parent,
        file_name: "Book.m4b".into(),
        format: BookFormat::M4b,
        checksum,
        size_bytes: bytes.len() as u64,
        inspection: Some(InspectedBook { pdf: None, metadata: BookRecord { metadata: BookDetails { title: "Book".into(), ..Default::default() }, description: String::new() }, audiobook: None, toc: Vec::new() }),
        transfer: None,
    };
    copied.into_staged(checksum, bytes.len() as u64).retain_provider_upload(importer.assets.clone(), draft.id, serde_json::to_vec(&draft).unwrap()).await.unwrap();
    importer.database.queue_provider_upload(&draft).unwrap();
    draft
}

struct Tokens;
impl ConnectionPersistence for Tokens {
    fn load_google(&self, _: &ProviderId) -> Result<Option<GoogleConnection>, CloudError> {
        Ok(Some(GoogleConnection {
            account: GoogleAccount { id: root().scope.account_id, email: "reader@example.com".into(), name: None },
            tokens: GoogleTokens { access_token: "local-test-token".into(), refresh_token: None, expires_at: u64::MAX },
        }))
    }
    fn save_google(&self, _: &GoogleConnection, _: Option<&str>) -> Result<bool, CloudError> {
        panic!("fresh test token must not be refreshed")
    }
}

#[derive(Clone, Copy)]
enum Failure {
    FirstChunk,
    FinalResponse,
    ExpiredFinalSession,
    StorageQuota,
}

#[derive(Clone)]
struct Request {
    method: String,
    path: String,
    range: Option<String>,
    body: Vec<u8>,
}

async fn request(socket: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let end = loop {
        let mut chunk = [0; 8192];
        let n = socket.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
    let mut line = headers.lines().next().unwrap().split_whitespace();
    let method = line.next().unwrap().to_owned();
    let path = line.next().unwrap().to_owned();
    let value = |key: &str| headers.lines().filter_map(|line| line.split_once(':')).find(|(name, _)| name.eq_ignore_ascii_case(key)).map(|(_, value)| value.trim().to_owned());
    assert_eq!(value("Authorization").as_deref(), Some("Bearer local-test-token"));
    let length = value("Content-Length").map(|value| value.parse::<usize>().unwrap()).unwrap_or(0);
    let range = value("Content-Range").or_else(|| value("Range"));
    while bytes.len() < end + length {
        let mut chunk = vec![0; (end + length - bytes.len()).min(64 * 1024)];
        let n = socket.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    Request { method, path, range, body: bytes[end..end + length].to_vec() }
}

async fn reply(socket: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &[u8]) {
    let mut message = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (key, value) in headers {
        message.push_str(&format!("{key}: {value}\r\n"));
    }
    message.push_str("\r\n");
    socket.write_all(message.as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
}

fn metadata(size: usize, parent: &str, revision: u64, name: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"id":"reserved", "name":name, "mimeType":"application/octet-stream", "parents":[parent], "size":size.to_string(), "version":revision.to_string()})).unwrap()
}

async fn drive(size: usize, failure: Failure) -> (ConnectedDrive, Arc<Mutex<Vec<Request>>>, JoinHandle<()>, Arc<AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "test-client".into(), client_secret: None }).unwrap();
    let connected = CloudConnections::new(oauth, Arc::new(Tokens)).test_google_drive(root().scope, url.clone()).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let revision = Arc::new(AtomicU64::new(1));
    let current_revision = revision.clone();
    let task = tokio::spawn(async move {
        let mut uploaded = Vec::new();
        let mut failed = false;
        let mut uploaded_name = "Book.m4b".to_owned();
        let mut nested: Option<Value> = None;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = request(&mut socket).await;
            let path = Url::parse(&format!("http://mock{}", request.path)).unwrap();
            let parent = if nested.is_some() { "nested-folder" } else { "folder" };
            let revision = current_revision.load(Ordering::SeqCst);
            let (status, headers, body) = match (request.method.as_str(), path.path()) {
                ("GET", "/drive/v3/files") => (200, Vec::new(), serde_json::to_vec(&json!({"files": nested.iter().collect::<Vec<_>>()})).unwrap()),
                ("GET", "/drive/v3/files/nested-folder") => (200, Vec::new(), serde_json::to_vec(nested.as_ref().unwrap()).unwrap()),
                ("POST", "/drive/v3/files") => {
                    let mut created: Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(created["parents"], json!(["folder"]));
                    assert!(created["appProperties"]["bokheimDirectory"].as_str().unwrap().parse::<DirId>().is_ok());
                    created["id"] = json!("nested-folder");
                    nested = Some(created.clone());
                    (200, Vec::new(), serde_json::to_vec(&created).unwrap())
                }
                ("GET", "/drive/v3/files/generateIds") => (200, Vec::new(), serde_json::to_vec(&json!({"ids":["reserved"]})).unwrap()),
                ("GET", "/drive/v3/files/reserved") if uploaded.len() != size => (404, Vec::new(), Vec::new()),
                ("GET", "/drive/v3/files/reserved") => {
                    if path.query_pairs().any(|(key, value)| key == "alt" && value == "media") {
                        let (start, end) = request.range.as_deref().unwrap().strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                        let start: usize = start.parse().unwrap();
                        let end: usize = end.parse().unwrap();
                        (206, vec![("Content-Range", format!("bytes {start}-{end}/{size}"))], uploaded[start..=end].to_vec())
                    } else {
                        (200, Vec::new(), metadata(size, parent, revision, &uploaded_name))
                    }
                }
                ("POST", "/upload/drive/v3/files") if matches!(failure, Failure::StorageQuota) && !failed => {
                    failed = true;
                    (403, Vec::new(), serde_json::to_vec(&json!({"error":{"code":403,"errors":[{"reason":"storageQuotaExceeded"}]}})).unwrap())
                }
                ("POST", "/upload/drive/v3/files") => {
                    let metadata: Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(metadata["id"], "reserved");
                    assert_eq!(metadata["parents"], json!([parent]));
                    uploaded_name = metadata["name"].as_str().unwrap().to_owned();
                    (200, vec![("Location", url.join("upload/drive/v3/session").unwrap().to_string())], Vec::new())
                }
                ("PUT", "/upload/drive/v3/session") => {
                    let range = request.range.as_deref().unwrap();
                    if range.starts_with("bytes */") {
                        if uploaded.len() == size && matches!(failure, Failure::ExpiredFinalSession) {
                            (404, Vec::new(), Vec::new())
                        } else if uploaded.len() == size {
                            (200, Vec::new(), metadata(size, parent, revision, &uploaded_name))
                        } else {
                            (308, vec![("Range", format!("bytes=0-{}", uploaded.len() - 1))], Vec::new())
                        }
                    } else {
                        let (span, _) = range.strip_prefix("bytes ").unwrap().split_once('/').unwrap();
                        let (start, _) = span.split_once('-').unwrap();
                        assert_eq!(start.parse::<usize>().unwrap(), uploaded.len(), "confirmed bytes must never be uploaded twice");
                        uploaded.extend_from_slice(&request.body);
                        let should_fail = !failed
                            && match failure {
                                Failure::FirstChunk => uploaded.len() < size,
                                Failure::FinalResponse | Failure::ExpiredFinalSession => uploaded.len() == size,
                                Failure::StorageQuota => false,
                            };
                        if should_fail {
                            failed = true;
                            (503, Vec::new(), Vec::new())
                        } else if uploaded.len() == size {
                            (200, Vec::new(), metadata(size, parent, revision, &uploaded_name))
                        } else {
                            (308, vec![("Range", format!("bytes=0-{}", uploaded.len() - 1))], Vec::new())
                        }
                    }
                }
                _ => panic!("unexpected mock Drive request {} {}", request.method, request.path),
            };
            capture.lock().unwrap().push(request);
            reply(&mut socket, status, &headers, &body).await;
        }
    });
    (connected, requests, task, revision)
}

async fn interruption(failure: Failure, nested: bool) {
    let root = tempfile::tempdir().unwrap();
    let library = Uuid::new_v4();
    let importer = importer(root.path(), library);
    let bytes = (0..MAX_RANGE_BYTES + 17).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let parent = if nested {
        let id = DirId::new_v4();
        importer.database.register_created_directory(&id, &ROOT_DIR_ID, "Audio", "Audio").unwrap();
        id
    } else {
        ROOT_DIR_ID
    };
    let draft = queued(&importer, &bytes, parent).await;
    let (connection, requests, server, _) = drive(bytes.len(), failure).await;
    let upload = importer.database.provider_upload(draft.id).unwrap().unwrap();
    assert!(importer.run_provider_upload_connected(upload, &connection).await.is_err());
    assert!(importer.database.provider_remote_books().unwrap().is_empty());
    assert!(!importer.database.sync_publishable_mutations().unwrap().iter().any(|publication| publication.state.value.book().is_some()));
    assert!(importer.database.provider_upload(draft.id).unwrap().unwrap().session.is_some());
    drop(importer);
    let importer = self::importer(root.path(), library);
    let upload = importer.database.provider_upload(draft.id).unwrap().unwrap();
    let id = tokio::time::timeout(std::time::Duration::from_secs(10), importer.run_provider_upload_connected(upload, &connection)).await.unwrap().unwrap();
    assert!(id.is_external());
    assert_ne!(id, draft.checksum);
    assert!(importer.database.is_book_downloaded(&id).unwrap());
    assert!(importer.assets.book_exists(&id, &importer.database.book_paths(&id).unwrap()).unwrap());
    assert!(importer.database.provider_upload(draft.id).unwrap().is_none());
    assert!(importer.assets.retained_provider_uploads().unwrap().is_empty());
    assert!(importer.database.pending_book_uploads(0, 100).unwrap().is_empty());
    assert!(
        importer
            .database
            .sync_publishable_mutations()
            .unwrap()
            .iter()
            .any(|publication| matches!(&publication.state.value, LibraryStateEntry::ExternalBook { content_hash, value } if *content_hash == id && value.file.content_hash == draft.checksum.content_hash()))
    );
    let capture = requests.lock().unwrap();
    assert_eq!(capture.iter().filter(|request| request.method == "POST" && request.path.starts_with("/upload/")).count(), 1, "resuming must retain the original provider object/session");
    if nested {
        assert_eq!(capture.iter().filter(|request| request.method == "POST" && request.path.starts_with("/drive/")).count(), 1);
        let created: Value = capture.iter().find(|request| request.method == "POST" && request.path.starts_with("/drive/")).map(|request| serde_json::from_slice(&request.body).unwrap()).unwrap();
        assert_eq!(created["appProperties"]["bokheimLibrary"], library.to_string());
        assert_eq!(created["appProperties"]["bokheimDirectory"], parent.to_string());
        assert_eq!(importer.database.provider_directory(parent).unwrap().unwrap().as_str(), "nested-folder");
    }
    assert_eq!(capture.iter().filter(|request| request.path.contains("generateIds")).count(), 1);
    let chunks = capture.iter().filter(|request| request.range.as_ref().is_some_and(|range| range.starts_with("bytes ") && !range.starts_with("bytes */"))).map(|request| request.body.clone()).flatten().collect::<Vec<_>>();
    assert_eq!(chunks, bytes);
    server.abort();
}

#[tokio::test]
async fn provider_quota_preserves_the_pending_book_across_reopen_and_retry() {
    let directory = tempfile::tempdir().unwrap();
    let library = Uuid::new_v4();
    let bytes = b"retained audiobook source";
    let importer = importer(directory.path(), library);
    let draft = queued(&importer, bytes, ROOT_DIR_ID).await;
    let (connection, _, server, _) = drive(bytes.len(), Failure::StorageQuota).await;
    let upload = importer.database.provider_upload(draft.id).unwrap().unwrap();
    let error = importer.run_provider_upload_connected(upload, &connection).await.unwrap_err();
    assert_eq!(error.failure(), Some(client_runtime::Failure::CloudStorageFull));
    assert!(importer.database.provider_remote_books().unwrap().is_empty());
    assert!(importer.database.pending_book_uploads(0, 10).unwrap().is_empty());
    assert!(!importer.database.sync_publishable_mutations().unwrap().iter().any(|publication| publication.state.value.book().is_some()));
    let mut retained = importer.assets.open_provider_upload(draft.id).unwrap();
    let mut actual = Vec::new();
    std::io::Read::read_to_end(&mut retained, &mut actual).unwrap();
    assert_eq!(actual, bytes);
    drop(retained);
    drop(importer);
    let importer = self::importer(directory.path(), library);
    let upload = importer.database.provider_upload(draft.id).unwrap().unwrap();
    let id = importer.run_provider_upload_connected(upload, &connection).await.unwrap();
    assert!(importer.database.is_book_downloaded(&id).unwrap());
    assert!(importer.database.provider_upload(draft.id).unwrap().is_none());
    assert!(importer.assets.retained_provider_uploads().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn transfer_retry_refreshes_reader_state_and_recovers_the_existing_upload_session() {
    use book_model::{Annotation, AnnotationAnchor, AnnotationStyle, ChapterReadStatus, ReadingPosition};
    use library_database::transfer::BookTransferState;
    use library_replica::{BookAddition, BookFacts, EnrichmentResult, ReadingPositionState};

    let directory = tempfile::tempdir().unwrap();
    let library = Uuid::new_v4();
    let importer = importer(directory.path(), library);
    let bytes = vec![7; MAX_RANGE_BYTES + 17];
    let checksum = BookId::from(blake3::hash(&bytes));
    let reading = |progress| LibraryStateEntry::ReadingPosition { content_hash: checksum, value: ReadingPositionState { location: ReadingPosition::start(BookFormat::M4b), progress } };
    let mut state = BookTransferState {
        content_hash: checksum,
        byte_checksum: checksum.content_hash(),
        provider_file: None,
        addition: BookAddition {
            facts: BookFacts { added_at: 1, format: BookFormat::M4b },
            record: BookRecord { metadata: BookDetails { title: "Book".into(), ..Default::default() }, description: String::new() },
            navigation: None,
            pdf: None,
            recording: None,
            enrichment: Some(EnrichmentResult::NoMatch),
        },
        reading: Some(reading(10.0)),
        chapter_read: None,
        annotations: Vec::new(),
    };
    let first = importer.import_transferred_book(ROOT_DIR_ID, "Book.m4b".into(), Box::new(Cursor::new(bytes.clone())), state.clone()).await.unwrap_err();
    assert!(first.to_string().contains("saved locally"), "{first}");
    let draft_id = ProviderUploadDraft::identity(ROOT_DIR_ID, "Book.m4b", checksum);
    let original = importer.database.provider_upload(draft_id).unwrap().unwrap().draft;
    let (connection, requests, server, _) = drive(bytes.len(), Failure::FirstChunk).await;
    let upload = importer.database.provider_upload(draft_id).unwrap().unwrap();
    assert!(importer.run_provider_upload_connected(upload, &connection).await.is_err());
    let pending = importer.database.provider_upload(draft_id).unwrap().unwrap();
    assert!(pending.session.is_some());

    state.reading = Some(reading(50.0));
    state.chapter_read = Some(LibraryStateEntry::ChapterReadStatus { content_hash: checksum, value: ChapterReadStatus::default() });
    state.annotations.push(Annotation {
        id: "new-note".into(),
        content_hash: checksum,
        anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
        exact_text: "Text".into(),
        style: AnnotationStyle::Underline,
        color: 0xffffff,
        note: "Added while waiting".into(),
        created_at: 7,
        toc_ordinal: None,
        progress: Some(50.0),
        modified_at: 9,
        deleted: false,
    });
    // No configured connection: requeue succeeds, but network access still fails.
    let retry = importer.import_transferred_book(ROOT_DIR_ID, "Book.m4b".into(), Box::new(Cursor::new(bytes.clone())), state.clone()).await.unwrap_err();
    assert!(retry.to_string().contains("saved locally"), "{retry}");
    let refreshed = importer.database.provider_upload(draft_id).unwrap().unwrap();
    assert_eq!(refreshed.file_id, pending.file_id);
    assert_eq!(refreshed.session, pending.session);
    assert_eq!(refreshed.draft.transfer.as_ref().unwrap().reading, state.reading);
    assert_eq!(refreshed.draft.transfer.as_ref().unwrap().chapter_read, state.chapter_read);
    assert_eq!(refreshed.draft.transfer.as_ref().unwrap().annotations, state.annotations);

    let mut changed = state.clone();
    changed.addition.record.description = "Different source metadata".into();
    let error = importer.import_transferred_book(ROOT_DIR_ID, "Book.m4b".into(), Box::new(Cursor::new(bytes)), changed).await.unwrap_err();
    assert!(error.to_string().contains("differs from its retained source"), "{error}");

    // Simulate a crash after refreshing the manifest, before updating SQLite.
    importer.database.queue_provider_upload(&original).unwrap();
    drop(importer);
    let importer = self::importer(directory.path(), library);
    assert!(importer.run_provider_uploads().await.is_err());
    let upload = importer.database.provider_upload(draft_id).unwrap().unwrap();
    assert_eq!(upload.session, pending.session);
    assert_eq!(upload.draft.transfer.as_ref().unwrap().reading, state.reading);
    let id = importer.run_provider_upload_connected(upload, &connection).await.unwrap();
    let expected = state.retarget(id).unwrap();
    let actual = importer.database.book_transfer_state(id).unwrap();
    assert_eq!(actual.reading, expected.reading);
    assert_eq!(actual.chapter_read, expected.chapter_read);
    assert_eq!(actual.annotations, expected.annotations);
    assert!(importer.database.provider_upload(draft_id).unwrap().is_none());
    assert!(importer.assets.retained_provider_uploads().unwrap().is_empty());
    assert_eq!(requests.lock().unwrap().iter().filter(|request| request.path.contains("generateIds")).count(), 1);
    server.abort();
}

#[tokio::test]
async fn interrupted_chunk_resumes_after_reopen_before_metadata_is_published() {
    interruption(Failure::FirstChunk, false).await;
}

#[tokio::test]
async fn lost_final_response_is_recovered_by_probing_the_saved_session() {
    interruption(Failure::FinalResponse, false).await;
}

#[tokio::test]
async fn expired_session_after_commit_verifies_and_reuses_the_existing_object() {
    interruption(Failure::ExpiredFinalSession, false).await;
}

#[tokio::test]
async fn nested_import_creates_a_provider_folder_and_preserves_it_on_resume() {
    interruption(Failure::FirstChunk, true).await;
}

#[tokio::test]
async fn imported_book_is_retained_without_bokheim_metadata_when_cloud_is_signed_out() {
    let root = tempfile::tempdir().unwrap();
    let library = Uuid::new_v4();
    let importer = importer(root.path(), library);
    let bytes = include_bytes!("../../../book-inspection/tests/fixtures/back-matter-evidence.epub");
    let error = importer.import_downloaded_book(ROOT_DIR_ID, "Book.epub", bytes.to_vec()).await.unwrap_err();
    assert!(error.to_string().contains("saved locally for cloud upload"));
    let before = importer.assets.retained_provider_uploads().unwrap();
    assert_eq!(before.len(), 1);
    let draft: ProviderUploadDraft = serde_json::from_slice(&before[0].1).unwrap();
    assert!(!draft.inspection.as_ref().unwrap().metadata.metadata.contributors.is_empty());
    let retry = importer.import_downloaded_book(ROOT_DIR_ID, "Book.epub", bytes.to_vec()).await.unwrap_err();
    assert!(retry.to_string().contains("saved locally for cloud upload"), "reinspection must retain the previous contributor IDs: {retry}");
    assert_eq!(importer.assets.retained_provider_uploads().unwrap(), before);
    assert!(importer.database.provider_remote_books().unwrap().is_empty());
    assert_eq!(importer.database.provider_upload_ids().unwrap().len(), 1);
    assert_eq!(importer.assets.retained_provider_uploads().unwrap().len(), 1);
    assert!(!importer.database.sync_publishable_mutations().unwrap().iter().any(|publication| publication.state.value.book().is_some()));
}

#[tokio::test]
async fn dropped_mp3_folder_uploads_an_archive_and_retains_local_track_playback() {
    use std::io::Read;
    let path = tempfile::tempdir().unwrap();
    let original = tempfile::tempdir().unwrap();
    let source = original.path().join("Audio");
    std::fs::create_dir(&source).unwrap();
    let mp3 = include_bytes!("../../../book-inspection/tests/fixtures/silence.mp3");
    std::fs::write(source.join("01.mp3"), mp3).unwrap();
    std::fs::write(source.join("02.mp3"), mp3).unwrap();
    std::fs::write(source.join("book.nfo"), b"<audiobook><title>Cloud audio</title></audiobook>").unwrap();
    let importer = importer(path.path(), Uuid::new_v4());
    let failures = importer.import_paths(ROOT_DIR_ID, vec![source.clone()], false).await.unwrap();
    assert_eq!(failures.len(), 1, "signed-out imports remain queued locally");
    let queued = importer.database.provider_upload_ids().unwrap();
    assert_eq!(queued.len(), 1);
    let upload = importer.database.provider_upload(queued[0]).unwrap().unwrap();
    assert_eq!(upload.draft.format, BookFormat::Mp3Folder);
    let (connection, requests, task, _) = drive(upload.draft.size_bytes as usize, Failure::FinalResponse).await;
    assert!(importer.run_provider_upload_connected(upload, &connection).await.is_err());
    let upload = importer.database.provider_upload(queued[0]).unwrap().unwrap();
    let id = importer.run_provider_upload_connected(upload, &connection).await.unwrap();
    let tracks = importer.database.audiobook_tracks(&id).unwrap();
    assert_eq!(tracks.len(), 2);
    assert!(tracks.iter().all(|track| track.archive_hash == Some(id)));
    let local = importer.database.book_paths(&id).unwrap().remove(0);
    assert_eq!(local.file_name(), "Audio");
    let mut reader = importer.assets.open_audiobook_track(local.clone(), tracks[1].name.clone()).await.unwrap().unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, mp3);
    drop(reader);
    let creation = requests.lock().unwrap().iter().find(|request| request.method == "POST" && request.path.starts_with("/upload/")).unwrap().body.clone();
    assert_eq!(serde_json::from_slice::<Value>(&creation).unwrap()["name"], "Audio.mp3folder");
    importer.assets.evict_book_files(&[local], &id).unwrap();
    assert_eq!(std::fs::read(source.join("01.mp3")).unwrap(), mp3);
    task.abort();
}

#[tokio::test]
async fn completed_upload_retry_preserves_a_newer_scanned_provider_revision() {
    use sync_common::storage::ProviderFileIdentity;
    let root = tempfile::tempdir().unwrap();
    let importer = importer(root.path(), Uuid::new_v4());
    let bytes = vec![42; 128];
    let draft = queued(&importer, &bytes, ROOT_DIR_ID).await;
    let (connection, _, server, revision) = drive(bytes.len(), Failure::FinalResponse).await;
    let upload = importer.database.provider_upload(draft.id).unwrap().unwrap();
    assert!(importer.run_provider_upload_connected(upload, &connection).await.is_err());
    let mut file = ProviderFile {
        identity: ProviderFileIdentity { scope: draft.root.scope.clone(), file_id: ProviderId::new("reserved").unwrap() },
        revision: ProviderId::new("1").unwrap(),
        size_bytes: bytes.len() as u64,
        content_hash: draft.checksum.content_hash(),
    };
    importer.database.confirm_provider_upload(draft.id, &file).unwrap();
    revision.store(2, Ordering::SeqCst);
    file.revision = ProviderId::new("2").unwrap();
    let mut new_inspection = draft.inspection.clone().unwrap();
    new_inspection.metadata.metadata.title = "New provider edition".into();
    let id = importer.database.admit_inspected_provider_book(ROOT_DIR_ID, file.clone(), draft.format, new_inspection).unwrap();
    let upload = importer.database.provider_upload(draft.id).unwrap().unwrap();
    assert_eq!(importer.run_provider_upload_connected(upload, &connection).await.unwrap(), id);
    assert_eq!(importer.database.provider_file(&id).unwrap(), Some(file));
    assert_eq!(importer.database.browse_book_detail(&id).unwrap().book.title, "New provider edition");
    assert!(!importer.database.is_book_downloaded(&id).unwrap());
    assert!(importer.database.provider_upload(draft.id).unwrap().is_none());
    assert!(importer.assets.retained_provider_uploads().unwrap().is_empty());
    server.abort();
}
