use super::*;
use crate::integration::LibraryAccountSession;
use crate::library::{LibraryOpenConfig, LibraryRuntimeSpec, ResolvedBook};
use cloud_storage::{
    connections::{CloudConnections, ConnectionPersistence, GoogleConnection},
    oauth::{GoogleAccount, GoogleOAuth, GoogleOAuthConfig, GoogleTokens},
};
use library_model::DownloadState;
use serde_json::{json, Value};
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use sync_common::storage::{CloudProvider, LibraryStorage, ProviderFileIdentity, ProviderScope};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};

#[derive(Clone, Debug)]
struct NoBokheimAccount;
impl account_client::SessionPersistence for NoBokheimAccount {
    fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
        Ok(None)
    }
    fn save_account_session(&self, _: &account_client::Session) -> std::io::Result<()> {
        panic!("cloud reading must not sign in to Bokheim")
    }
    fn clear_account_session(&self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(in crate::library) fn scope() -> ProviderScope {
    ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("google-account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() }
}
fn root() -> CloudLibraryRoot {
    CloudLibraryRoot { scope: scope(), folder_id: ProviderId::new("folder").unwrap() }
}

struct Tokens;
impl ConnectionPersistence for Tokens {
    fn load_google(&self, _: &ProviderId) -> Result<Option<GoogleConnection>, CloudError> {
        Ok(Some(GoogleConnection {
            account: GoogleAccount { id: scope().account_id, email: "reader@example.com".into(), name: None },
            tokens: GoogleTokens { access_token: "test-google-token".into(), refresh_token: None, expires_at: u64::MAX },
        }))
    }
    fn save_google(&self, _: &GoogleConnection, _: Option<&str>) -> Result<bool, CloudError> {
        panic!("test credentials must remain fresh")
    }
}

fn library(path: &Path, cloud: CloudConnections) -> LibrarySession {
    library_with_storage(path, cloud, LibraryStorage::External(root()))
}

pub(in crate::library) fn library_with_storage(path: &Path, cloud: CloudConnections, storage: LibraryStorage) -> LibrarySession {
    let server = "http://127.0.0.1:1".parse().unwrap();
    let account = LibraryAccountSession::load(NoBokheimAccount, server).unwrap();
    LibrarySession::open(LibraryRuntimeSpec {
        config: LibraryOpenConfig {
            id: uuid::Uuid::new_v4(),
            display_name: "Google Drive".into(),
            root: path.to_string_lossy().into_owned(),
            database_locator: path.join("library.sqlite").to_string_lossy().into_owned(),
            default_sync_server_url: "http://127.0.0.1:1".parse().unwrap(),
            metadata_server_url: "http://127.0.0.1:1".into(),
            storage,
            reset_cloud_presence: false,
        },
        account: account.library_account(),
        cloud,
        notification_interest: Default::default(),
        root_failures: None,
    })
    .unwrap()
}

pub(in crate::library) async fn seed_provider_book(session: &LibrarySession, mock: &MockDrive) -> BookId {
    let connection = session.cloud.google_drive(scope()).unwrap();
    let inspection = session.inspect_cloud_file(connection, mock.file.clone(), "Book.epub".into(), BookFormat::Epub).await.unwrap();
    session.db.admit_inspected_provider_book(ROOT_DIR_ID, mock.file.clone(), BookFormat::Epub, inspection).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_inspection_uses_the_session_cpu_host() {
    use client_platform_runtime::executor::BoxedBackendFuture;
    use cpu_host::{BookTask, BoxedCpuReader, CpuHost, CpuRequest, CpuResponse};
    use std::sync::atomic::AtomicUsize;

    struct CountingHost {
        native: client_platform_native::cpu_host::NativeHost,
        inspections: Arc<AtomicUsize>,
    }
    impl CpuHost for CountingHost {
        fn book(&self, task: BookTask, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
            if matches!(task, BookTask::Inspect { .. }) {
                self.inspections.fetch_add(1, Ordering::SeqCst);
            }
            self.native.book(task, reader)
        }
        fn request(&self, request: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
            self.native.request(request)
        }
    }

    let root = tempfile::tempdir().unwrap();
    let mock = drive(include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub").to_vec(), "Book.epub").await;
    let mut session = library(root.path(), mock.cloud.clone());
    let inspections = Arc::new(AtomicUsize::new(0));
    session.cpu = Arc::new(CountingHost { native: Default::default(), inspections: inspections.clone() });
    let id = seed_provider_book(&session, &mock).await;
    assert_eq!(id, mock.file.identity.book_id());
    assert_eq!(inspections.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_range_batches_reject_invalid_bounds_before_network_io() {
    let bytes = include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
    let root = tempfile::tempdir().unwrap();
    let mock = drive(bytes.to_vec(), "Book.epub").await;
    let session = library(root.path(), mock.cloud.clone());
    let id = seed_provider_book(&session, &mock).await;
    let (source, _) = session.describe_remote_file(id).await.unwrap();
    let requests = mock.requests.lock().unwrap().len();
    for invalid in [vec![], vec![(0, 0)], vec![(source.length, 1)], vec![(u64::MAX, 1)], vec![(0, 4), (2, 4)], vec![(4, 2), (0, 2)]] {
        assert!(session.read_file_ranges(&source, invalid.clone()).await.is_err());
        assert!(session.read_file_bundle(&source, invalid).await.is_err());
    }
    assert_eq!(mock.requests.lock().unwrap().len(), requests);
    let expected = [&bytes[..4], &bytes[8..12]].concat();
    assert_eq!(session.read_file_ranges(&source, vec![(0, 4), (8, 4)]).await.unwrap(), expected);
    assert_eq!(session.read_file_bundle(&source, vec![(0, 4), (8, 4)]).await.unwrap(), expected);
}

pub(in crate::library) struct MockDrive {
    pub(in crate::library) cloud: CloudConnections,
    pub(in crate::library) file: ProviderFile,
    pub(in crate::library) revision: Arc<AtomicU64>,
    pub(in crate::library) requests: Arc<Mutex<Vec<String>>>,
    pub(in crate::library) fail_upload: Arc<AtomicBool>,
    scan_entries: Arc<Mutex<Vec<Value>>>,
    read_gate: Arc<Mutex<Option<Arc<ReadGate>>>>,
    task: JoinHandle<()>,
}

#[derive(Default)]
struct ReadGate {
    started: Notify,
    release: Notify,
}
impl Drop for MockDrive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn headers(socket: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let end = loop {
        let mut buffer = [0; 8192];
        let count = socket.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
    let length = headers.lines().filter_map(|line| line.split_once(':')).find(|(name, _)| name.eq_ignore_ascii_case("Content-Length")).map(|(_, value)| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
    while bytes.len() < end + length {
        let mut buffer = [0; 8192];
        let count = socket.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Some((headers, bytes[end..end + length].to_vec()))
}

pub(in crate::library) async fn drive(bytes: Vec<u8>, name: &'static str) -> MockDrive {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint: account_client::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "test-client".into(), client_secret: None }).unwrap();
    let cloud = CloudConnections::new(oauth, Arc::new(Tokens)).with_test_google_drive_endpoint(scope(), endpoint.clone()).unwrap();
    let file = ProviderFile { identity: ProviderFileIdentity { scope: scope(), file_id: ProviderId::new("book").unwrap() }, revision: ProviderId::new("1").unwrap(), size_bytes: bytes.len() as u64, content_hash: None };
    let revision = Arc::new(AtomicU64::new(1));
    let current = revision.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let fail_upload = Arc::new(AtomicBool::new(false));
    let deny_upload = fail_upload.clone();
    let scan_entries = Arc::new(Mutex::new(Vec::<Value>::new()));
    let inventory = scan_entries.clone();
    let read_gate = Arc::new(Mutex::new(None::<Arc<ReadGate>>));
    let gate = read_gate.clone();
    let task = tokio::spawn(async move {
        let mut uploaded = Vec::new();
        let mut upload_size = 0;
        let mut upload_name = String::new();
        let mut upload_parent = String::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let Some((request, request_body)) = headers(&mut socket).await else { continue };
            assert!(request.to_ascii_lowercase().contains("authorization: bearer test-google-token"));
            let line = request.lines().next().unwrap();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap();
            let path = parts.next().unwrap();
            let url: account_client::Url = format!("http://mock{path}").parse().unwrap();
            let uploaded_metadata = || serde_json::to_vec(&json!({"id":"uploaded","name":upload_name,"mimeType":"application/octet-stream","parents":[upload_parent],"size":upload_size.to_string(),"version":"1"})).unwrap();
            let (status, range, body) = match (method, url.path()) {
                ("GET", "/drive/v3/files/generateIds") => (200, None, serde_json::to_vec(&json!({"ids":["uploaded"]})).unwrap()),
                ("GET", "/drive/v3/files/uploaded") if uploaded.len() != upload_size || upload_size == 0 => (404, None, Vec::new()),
                ("GET", "/drive/v3/files/uploaded") => (200, None, uploaded_metadata()),
                ("POST", "/upload/drive/v3/files") if deny_upload.load(Ordering::SeqCst) => (503, None, Vec::new()),
                ("POST", "/upload/drive/v3/files") => {
                    let metadata: serde_json::Value = serde_json::from_slice(&request_body).unwrap();
                    assert_eq!(metadata["id"], "uploaded");
                    upload_name = metadata["name"].as_str().unwrap().to_owned();
                    upload_parent = metadata["parents"][0].as_str().unwrap().to_owned();
                    upload_size = request.lines().filter_map(|line| line.split_once(':')).find(|(name, _)| name.eq_ignore_ascii_case("X-Upload-Content-Length")).unwrap().1.trim().parse::<usize>().unwrap();
                    uploaded.clear();
                    (200, Some(endpoint.join("upload/drive/v3/session").unwrap().to_string()), Vec::new())
                }
                ("PUT", "/upload/drive/v3/session") => {
                    let range = request.lines().filter_map(|line| line.split_once(':')).find(|(name, _)| name.eq_ignore_ascii_case("Content-Range")).unwrap().1.trim();
                    if !range.starts_with("bytes */") {
                        let start = range.strip_prefix("bytes ").unwrap().split_once('-').unwrap().0.parse::<usize>().unwrap();
                        assert_eq!(start, uploaded.len());
                        uploaded.extend_from_slice(&request_body);
                    }
                    if uploaded.len() == upload_size {
                        (200, None, uploaded_metadata())
                    } else {
                        (308, uploaded.len().checked_sub(1).map(|last| format!("bytes=0-{last}")), Vec::new())
                    }
                }
                ("GET", "/drive/v3/files/book") if url.query_pairs().any(|(key, value)| key == "alt" && value == "media") => {
                    let paused = gate.lock().unwrap().take();
                    if let Some(paused) = paused {
                        paused.started.notify_one();
                        paused.release.notified().await;
                    }
                    let range = request.lines().filter_map(|line| line.split_once(':')).find(|(name, _)| name.eq_ignore_ascii_case("Range")).unwrap().1.trim();
                    let (start, end) = range.strip_prefix("bytes=").unwrap().split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end: usize = end.parse().unwrap();
                    (206, Some(format!("bytes {start}-{end}/{}", bytes.len())), bytes[start..=end].to_vec())
                }
                ("GET", "/drive/v3/files/book") => {
                    (200, None, serde_json::to_vec(&json!({"id":"book","name":name,"mimeType":"application/octet-stream","parents":["folder"],"size":bytes.len().to_string(),"version":current.load(Ordering::SeqCst).to_string()})).unwrap())
                }
                ("GET", path) if path.starts_with("/drive/v3/files/missing-") => (404, None, Vec::new()),
                ("GET", "/drive/v3/changes/startPageToken") => (200, None, serde_json::to_vec(&json!({"startPageToken":"checkpoint"})).unwrap()),
                ("GET", "/drive/v3/changes") => (200, None, serde_json::to_vec(&json!({"changes":[],"newStartPageToken":"checkpoint"})).unwrap()),
                ("GET", "/drive/v3/files") => {
                    let entries = if url.query_pairs().any(|(key, value)| key == "q" && value.contains("'folder' in parents")) { inventory.lock().unwrap().clone() } else { Vec::new() };
                    (200, None, serde_json::to_vec(&json!({"files":entries})).unwrap())
                }
                ("POST", "/drive/v3/files") => (403, None, Vec::new()),
                _ => panic!("unexpected Google Drive request {line}"),
            };
            capture.lock().unwrap().push(line.to_owned());
            let mut response = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
            if let Some(range) = range {
                let header = if method == "POST" {
                    "Location"
                } else if status == 308 {
                    "Range"
                } else {
                    "Content-Range"
                };
                response.push_str(&format!("{header}: {range}\r\n"));
            }
            response.push_str("\r\n");
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        }
    });
    MockDrive { cloud, file, revision, requests, fail_upload, scan_entries, read_gate, task }
}

fn scan_inventory(mock: &MockDrive) {
    *mock.scan_entries.lock().unwrap() = vec![
        json!({"id":"shelf","name":"Empty shelf","mimeType":"application/vnd.google-apps.folder","parents":["folder"]}),
        json!({"id":"book","name":"Book.epub","mimeType":"application/epub+zip","parents":["folder"],"size":mock.file.size_bytes.to_string(),"version":"1"}),
    ];
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_scan_stays_busy_until_books_are_admitted_and_cached_inventory_can_retry() {
    use crate::library::handle::LibraryActor;

    let path = tempfile::tempdir().unwrap();
    let bytes = include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub").to_vec();
    let mock = drive(bytes, "Book.epub").await;
    scan_inventory(&mock);
    let session = library(path.path(), mock.cloud.clone());
    let asset_requests = session.requests.as_ref().unwrap().assets.clone();
    let mut actor = LibraryActor::new(session);
    let session = actor.session.clone();
    let connection = mock.cloud.google_drive(scope()).unwrap();
    let root = root();
    let updates = actor.coordination.subscribe_updates();

    for cancel in [true, false] {
        let gate = Arc::new(ReadGate::default());
        *mock.read_gate.lock().unwrap() = Some(gate.clone());
        let mut scan = Box::pin(session.scan_cloud_library(&connection, &root));
        tokio::select! {
            _ = gate.started.notified() => {},
            result = &mut scan => panic!("scan finished before its book was inspected: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("inspection did not start"),
        }
        assert!(session.db.provider_scan().unwrap().unwrap().completed, "enumeration has already finished");
        assert!(session.scanner_running(), "a new UI subscription must still observe scanning during inspection");
        assert!(session.db.provider_remote_books().unwrap().is_empty());
        assert!(session.db.child_directory(&ROOT_DIR_ID, "Empty shelf").unwrap().is_some());
        actor.coordination.drain(&session.events);
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Scanning(true));
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Contents, "the first folders must appear without waiting for book inspection");
        assert!(updates.is_empty(), "enumeration must not announce scan completion");

        if cancel {
            drop(scan);
            gate.release.notify_one();
        } else {
            gate.release.notify_one();
            scan.await.unwrap();
            assert_eq!(session.db.provider_remote_books().unwrap().len(), 1);
            assert!(session.db.child_directory(&ROOT_DIR_ID, "Empty shelf").unwrap().is_some());
        }
        assert!(!session.scanner_running(), "completion and cancellation must release busy state");
        actor.coordination.drain(&session.events);
        let observed = std::iter::from_fn(|| updates.try_recv().ok()).collect::<Vec<_>>();
        assert!(observed.contains(&LibraryUpdate::Scanning(false)));
        assert!(observed.contains(&LibraryUpdate::Contents));
    }

    while asset_requests.try_recv().is_ok() {}
    session.scan_cloud_library(&connection, &root).await.unwrap();
    actor.coordination.drain(&session.events);
    assert!(asset_requests.is_empty(), "scan completion must not wake its own asset worker in a loop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cloud_inspection_releases_scanning_and_refreshes_folder_only_results() {
    use crate::library::handle::LibraryActor;

    let path = tempfile::tempdir().unwrap();
    let mock = drive(b"not an EPUB".to_vec(), "Book.epub").await;
    scan_inventory(&mock);
    let mut actor = LibraryActor::new(library(path.path(), mock.cloud.clone()));
    let updates = actor.coordination.subscribe_updates();
    let session = actor.session.clone();
    let connection = mock.cloud.google_drive(scope()).unwrap();
    assert!(session.scan_cloud_library(&connection, &root()).await.is_err());
    assert!(!session.scanner_running());
    assert!(session.db.child_directory(&ROOT_DIR_ID, "Empty shelf").unwrap().is_some());
    actor.coordination.drain(&session.events);
    let observed = std::iter::from_fn(|| updates.try_recv().ok()).collect::<Vec<_>>();
    assert_eq!(observed.first(), Some(&LibraryUpdate::Scanning(true)));
    assert!(observed.contains(&LibraryUpdate::Scanning(false)));
    assert_eq!(observed.last(), Some(&LibraryUpdate::Contents));
}

fn archive() -> (Vec<u8>, Vec<u8>) {
    let source = tempfile::tempdir().unwrap();
    let mp3 = include_bytes!("../../../crates/book-inspection/tests/fixtures/silence.mp3");
    let mut large = Vec::new();
    while large.len() < 3 * 1024 * 1024 {
        large.extend_from_slice(mp3);
    }
    std::fs::write(source.path().join("01.mp3"), mp3).unwrap();
    std::fs::write(source.path().join("02.mp3"), &large).unwrap();
    std::fs::write(source.path().join("book.nfo"), b"<audiobook><title>Cloud audio</title></audiobook>").unwrap();
    image::RgbImage::from_pixel(80, 120, image::Rgb([150, 70, 20])).save(source.path().join("cover.png")).unwrap();
    let tracks = audiobook_folder::discover(source.path()).unwrap();
    let bytes = audiobook_folder::write_folder_archive(source.path(), Cursor::new(Vec::new()), &tracks).unwrap().into_inner();
    (bytes, large)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_mp3_archive_streams_seeks_downloads_and_evicts_without_bokheim_login() {
    let root_path = tempfile::tempdir().unwrap();
    let (bytes, expected) = archive();
    let mock = drive(bytes.clone(), "Audio.mp3folder").await;
    let session = library(root_path.path(), mock.cloud.clone());
    assert!(session.sync.account().unwrap().current().unwrap().is_none());
    let connection = session.cloud.google_drive(scope()).unwrap();
    let inspection = session.inspect_cloud_file(connection.clone(), mock.file.clone(), "Audio.mp3folder".into(), BookFormat::Mp3Folder).await.unwrap();
    let id = session.db.admit_inspected_provider_book(ROOT_DIR_ID, mock.file.clone(), BookFormat::Mp3Folder, inspection).unwrap();
    let tracks = session.db.audiobook_tracks(&id).unwrap();
    assert_eq!(tracks.len(), 2);
    assert!(tracks.iter().all(|track| track.archive_hash == Some(id)));
    let remote = session.prepare_audiobook_track(id, 1).await.unwrap();
    let tail = client_platform_runtime::executor::run_blocking(move || {
        let ResolvedBook::Audiobook(mut book) = remote else { panic!("expected audio") };
        book.reader.seek(SeekFrom::End(-4096)).unwrap();
        let mut bytes = vec![0; 4096];
        book.reader.read_exact(&mut bytes).unwrap();
        bytes
    })
    .await
    .unwrap();
    assert_eq!(tail, expected[expected.len() - 4096..]);
    session.cloud_thumbnails(&connection, &root()).await.unwrap();
    assert_eq!(session.db.provider_cover_cached(&id).unwrap(), Some(true));
    assert!(session.thumbnail(id, crate::ThumbnailResolution::Browse).await.unwrap().is_some());
    assert!(session.db.provider_thumbnails(&id).unwrap().is_none(), "a read-only provider keeps generated artwork local");
    session.db.sync_request_book_downloads(&[id], TransferOrigin::UserInitiated).unwrap();
    session.download_cloud_book(&connection, id).await.unwrap();
    assert_eq!(session.persistent_download_state(id).unwrap(), DownloadState::Downloaded);
    let first_path = session.db.book_paths(&id).unwrap().remove(0).to_string();
    let local = session.prepare_audiobook_track(id, 1).await.unwrap();
    let content = client_platform_runtime::executor::run_blocking(move || {
        let ResolvedBook::Audiobook(mut book) = local else { panic!("expected audio") };
        let mut bytes = Vec::new();
        book.reader.read_to_end(&mut bytes).unwrap();
        bytes
    })
    .await
    .unwrap();
    assert_eq!(content, expected);
    assert_eq!(session.remove_local_book_copy(id).unwrap(), DownloadState::NotDownloaded);
    assert!(!root_path.path().join(first_path.trim_start_matches('/')).exists());
    assert!(session.db.provider_file(&id).unwrap().is_some());
    session.download_cloud_book(&connection, id).await.unwrap();
    assert_eq!(session.persistent_download_state(id).unwrap(), DownloadState::Downloaded);
    let paths = session.db.book_paths(&id).unwrap().into_iter().map(|path| path.to_string()).collect::<HashSet<_>>();
    assert_eq!(paths, HashSet::from([first_path]), "re-downloading must reuse the previous local folder name");
    assert!(mock.requests.lock().unwrap().iter().any(|line| line.starts_with("POST /drive/v3/files")), "cover generation must tolerate a denied write");
    assert!(mock.requests.lock().unwrap().iter().all(|line| line.contains("/drive/v3/")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cloud_downloads_do_not_starve_later_requests_or_pages() {
    let root_path = tempfile::tempdir().unwrap();
    let bytes = include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub").to_vec();
    let mock = drive(bytes, "Book.epub").await;
    let session = library(root_path.path(), mock.cloud.clone());
    let healthy = seed_provider_book(&session, &mock).await;
    let addition = session.db.book_transfer_state(healthy).unwrap().addition;
    let mut background = Vec::new();
    for index in 0..33 {
        let mut file = mock.file.clone();
        file.identity.file_id = ProviderId::new(format!("missing-{index}")).unwrap();
        let id = session.db.admit_external_book(ROOT_DIR_ID, library_replica::ExternalBook { file, addition: addition.clone(), thumbnails: None }).unwrap();
        background.push(id);
    }
    let failed_user = background.pop().unwrap();
    session.db.sync_request_book_downloads(&background, TransferOrigin::Background).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    session.db.sync_request_book_downloads(&[failed_user], TransferOrigin::UserInitiated).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    session.db.sync_request_book_downloads(&[healthy], TransferOrigin::UserInitiated).unwrap();
    assert!(session.db.book_download_requests(-1, None, 32).unwrap().iter().all(|(id, _)| background.contains(id)));
    let connection = session.cloud.google_drive(scope()).unwrap();
    assert!(session.cloud_downloads(&connection).await.is_err());
    assert_eq!(session.persistent_download_state(healthy).unwrap(), DownloadState::Downloaded);
    assert_eq!(session.db.book_download_origin(&failed_user).unwrap(), None);
    let pending = session.db.book_download_requests(-1, None, 100).unwrap();
    assert_eq!(pending.len(), background.len());
    assert!(pending.iter().all(|(id, _)| background.contains(id)));
    assert!(session.cloud_downloads(&connection).await.is_err());
    assert_eq!(session.persistent_download_state(healthy).unwrap(), DownloadState::Downloaded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_range_descriptors_reject_same_size_new_revisions() {
    let root_path = tempfile::tempdir().unwrap();
    let bytes = include_bytes!("../../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub").to_vec();
    let mock = drive(bytes, "Book.epub").await;
    let session = library(root_path.path(), mock.cloud.clone());
    let connection = session.cloud.google_drive(scope()).unwrap();
    let inspection = session.inspect_cloud_file(connection, mock.file.clone(), "Book.epub".into(), BookFormat::Epub).await.unwrap();
    let id = session.db.admit_inspected_provider_book(ROOT_DIR_ID, mock.file.clone(), BookFormat::Epub, inspection.clone()).unwrap();
    let (source, _) = session.describe_remote_file(id).await.unwrap();
    let pinned = session.remote_book_access(&source).unwrap();
    mock.revision.store(2, Ordering::SeqCst);
    assert!(pinned.read(0, 10).await.is_err());
    let mut changed = mock.file.clone();
    changed.revision = ProviderId::new("2").unwrap();
    session.db.admit_inspected_provider_book(ROOT_DIR_ID, changed, BookFormat::Epub, inspection).unwrap();
    let previous = mock.requests.lock().unwrap().len();
    assert!(session.read_file_range(&source, 0, 10).await.is_err());
    assert!(session.read_file_ranges(&source, vec![(0, 10)]).await.is_err());
    assert!(session.read_file_bundle(&source, vec![(0, 10)]).await.is_err());
    assert_eq!(mock.requests.lock().unwrap().len(), previous, "an obsolete descriptor must fail before reading newer bytes");
    let (current, _) = session.describe_remote_file(id).await.unwrap();
    assert_eq!(current.revision.as_deref(), Some("2"));
    assert_eq!(session.read_file_range(&current, 0, 10).await.unwrap().len(), 10);
    assert_eq!(session.read_file_ranges(&current, vec![(0, 10)]).await.unwrap().len(), 10);
    assert_eq!(session.read_file_bundle(&current, vec![(0, 10)]).await.unwrap().len(), 10);
}
