#![cfg(not(target_arch = "wasm32"))]

use account_client::{Session, SessionPersistence};
use asset_transfer::transfer::AssetExecutor;
use book_model::{BookDetails, BookFormat, BookRecord};
use client_platform_runtime::{
    executor::{defer_library_maintenance, BackendExecutor},
    transfers::{NetworkCost, POLICY},
};
use futures_util::io::{AsyncReadExt, AsyncSeekExt};
use library_backend::{
    integration::LibraryAccountSession,
    library::{DocumentSource, LibraryCommand, LibraryHandle, LibraryOpenConfig, LibraryRuntimeSpec, PendingCommand, RemoteFile},
    BookId, DownloadState, LibraryClient, LibraryId, ThumbnailResolution, ROOT_DIR_ID,
};
use library_database::{Database, ImportCommit};
use library_model::{BlobKind, PublishedStagedBook, TransferOrigin};
use std::{
    io::SeekFrom,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    time::timeout,
};

#[derive(Clone, Debug)]
struct Account(Session);

impl SessionPersistence for Account {
    fn load_account_session(&self) -> std::io::Result<Option<Session>> {
        Ok(Some(self.0.clone()))
    }
    fn save_account_session(&self, _: &Session) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_account_session(&self) -> std::io::Result<()> {
        Ok(())
    }
}

fn open_library(root: &Path, server: &str, executor: &BackendExecutor) -> (LibraryHandle, LibraryClient) {
    let url = server.parse().unwrap();
    let account = LibraryAccountSession::load(Account(Session::try_new(url, "user".into(), "reader@example.com".into(), "a".repeat(43)).unwrap()), server.parse().unwrap()).unwrap();
    let id = LibraryId::new_v4();
    let handle = LibraryHandle::open(
        LibraryRuntimeSpec {
            config: LibraryOpenConfig {
                id,
                display_name: "Metered reading".into(),
                root: root.to_string_lossy().into_owned(),
                database_locator: root.join("library.sqlite").to_string_lossy().into_owned(),
                default_sync_server_url: server.parse().unwrap(),
                metadata_server_url: server.into(),
                storage: sync_common::storage::LibraryStorage::Bokheim,
                reset_cloud_presence: false,
            },
            account: account.library_account(),
            cloud: Default::default(),
            notification_interest: Default::default(),
            root_failures: None,
        },
        executor,
    )
    .unwrap();
    (handle.clone(), LibraryClient::new(id, handle))
}

// This integration binary owns its global policy, so disabling metered
// transfers cannot interfere with the backend's other concurrently run tests.
#[tokio::test]
async fn explicit_reading_and_downloads_bypass_metering_while_automatic_downloads_defer() {
    let _maintenance = defer_library_maintenance();
    let root = tempfile::tempdir().unwrap();
    // Valid EPUB metadata plus an unused stored payload for later demand reads.
    let bytes = Arc::new(include_bytes!("fixtures/metered-reading.epub").to_vec());
    let hash = BookId::from(blake3::hash(&bytes));
    {
        let db = Database::open(root.path().join("library.sqlite")).unwrap();
        db.initialize_library().unwrap();
        db.commit_import(ImportCommit {
            parent_id: ROOT_DIR_ID,
            file_name: "book.epub".into(),
            format: BookFormat::Epub,
            content_hash: hash,
            size_bytes: bytes.len() as u64,
            inspection_version: 1,
            inspection: Some(book_inspection::InspectedBook { pdf: None, audiobook: None, toc: vec![], metadata: BookRecord { metadata: BookDetails { title: "Remote book".into(), ..Default::default() }, description: String::new() } }),
            published: PublishedStagedBook { name: "book.epub".into(), relative_path: "/book.epub".into(), published: false, fingerprint: None },
            request_thumbnail: false,
            restore_paths: vec![],
        })
        .unwrap();
        db.record_remote_asset(BlobKind::Book, &hash).unwrap();
        db.evict_local_book(&hash).unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let server = tokio::spawn({
        let bytes = bytes.clone();
        let requests = requests.clone();
        async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut line = String::new();
                socket.read_line(&mut line).await.unwrap();
                let mut range = None;
                let mut authenticated = false;
                loop {
                    line.clear();
                    socket.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("range: bytes=") {
                        range = Some(value.trim().to_owned());
                    }
                    if lower.trim() == format!("authorization: bearer {}", "a".repeat(43)) {
                        authenticated = true;
                    }
                }
                assert!(authenticated);
                requests.fetch_add(1, Ordering::SeqCst);
                // Exercise the reader's fallback for single-range servers.
                if range.as_ref().is_some_and(|value| value.contains(',')) {
                    socket.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    socket.flush().await.unwrap();
                    continue;
                }
                let partial = range.is_some();
                let (start, end) = range.map_or((0, bytes.len() - 1), |value| {
                    let (start, end) = value.split_once('-').unwrap();
                    if start.is_empty() {
                        (bytes.len().saturating_sub(end.parse::<usize>().unwrap()), bytes.len() - 1)
                    } else {
                        (start.parse::<usize>().unwrap(), if end.is_empty() { bytes.len() - 1 } else { end.parse::<usize>().unwrap() })
                    }
                });
                let end = end.min(bytes.len() - 1);
                let status = if partial { "206 Partial Content" } else { "200 OK" };
                let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: \"{hash}\"\r\nConnection: close\r\n\r\n", end - start + 1, bytes.len());
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(&bytes[start..=end]).await.unwrap();
                socket.flush().await.unwrap();
            }
        }
    });

    let executor = BackendExecutor::new().unwrap();
    let (handle, client) = open_library(root.path(), &address, &executor);
    POLICY.set_allow_metered(false);
    POLICY.set_network_cost(NetworkCost::Metered);
    assert!(!POLICY.allowed());
    let covers = futures_util::future::join_all((0..8).map(|_| client.thumbnail(hash, ThumbnailResolution::HighDensity)));
    let results = timeout(Duration::from_secs(1), covers).await.expect("paused covers must release reader slots so opening a book cannot be blocked");
    assert!(results.into_iter().all(|result| result.unwrap().is_none()));
    let book = timeout(Duration::from_secs(5), client.resolve_book(hash)).await.expect("opening a remote book must bypass the metered restriction").unwrap().into_document().unwrap();
    let DocumentSource::Async(source) = book.source else { panic!("expected an async remote source") };
    let mut reader = source.reader();
    // Seek beyond the opening prefix to require another HTTP request.
    let before_page = requests.load(Ordering::SeqCst);
    reader.seek(SeekFrom::Start(192 * 1024)).await.unwrap();
    let mut page = vec![0; 1024];
    timeout(Duration::from_secs(5), reader.read_exact(&mut page)).await.expect("subsequent page reads must also bypass metering").unwrap();
    assert_eq!(page, bytes[192 * 1024..193 * 1024]);
    assert!(requests.load(Ordering::SeqCst) > before_page, "the page read must exercise HTTP access after opening");

    let source = RemoteFile { hash, length: bytes.len() as u64, revision: None };
    let bundle: serde_bytes::ByteBuf =
        timeout(Duration::from_secs(5), client.request(LibraryCommand::Pending(PendingCommand::ReadFileBundle { source, ranges: vec![(32, 128), (96 * 1024, 256)] }))).await.expect("PDF range bundles must bypass metering").unwrap();
    assert_eq!(bundle.as_ref(), [bytes[32..160].to_vec(), bytes[96 * 1024..96 * 1024 + 256].to_vec()].concat());
    assert_eq!(client.download_state(hash).await.unwrap(), DownloadState::NotDownloaded, "reading must not install a complete downloaded copy");

    let database = Arc::new(Database::open(root.path().join("library.sqlite")).unwrap());
    let account = LibraryAccountSession::load(Account(Session::try_new(address.parse().unwrap(), "user".into(), "reader@example.com".into(), "a".repeat(43)).unwrap()), address.parse().unwrap()).unwrap().library_account();
    let assets = AssetExecutor {
        asset_storage_enabled: Arc::new(AtomicBool::new(true)),
        assets: library_files::AssetStore::open(root.path().to_str().unwrap()).unwrap(),
        http_client: reqwest::Client::new(),
        library_id: handle.id(),
        events: None,
        credentials: account.credentials(),
        account: Some(account),
        notification_interest: Default::default(),
        availability_changed: Arc::new(AtomicBool::new(false)),
        database: database.clone(),
        cpu: Arc::new(client_platform_native::cpu_host::NativeHost::default()),
    };
    database.sync_request_book_downloads(&[hash], TransferOrigin::Background).unwrap();
    let before = requests.load(Ordering::SeqCst);
    assert!(timeout(Duration::from_secs(1), assets.download_book(hash)).await.expect("automatic work must release the worker").is_err());
    assert_eq!(requests.load(Ordering::SeqCst), before, "automatic downloads must not send requests on metered connections");
    database.sync_request_book_downloads(&[hash], TransferOrigin::UserInitiated).unwrap();
    // A later background pass must preserve the user's durable exemption.
    database.sync_request_book_downloads(&[hash], TransferOrigin::Background).unwrap();
    assert_eq!(database.book_download_origin(&hash).unwrap(), Some(TransferOrigin::UserInitiated));
    timeout(Duration::from_secs(5), assets.download_book(hash)).await.expect("explicit downloads must bypass metering").unwrap();
    assert_eq!(std::fs::read(root.path().join("book.epub")).unwrap(), *bytes);
    assert_eq!(client.download_state(hash).await.unwrap(), DownloadState::Downloaded);
    assert!(!POLICY.allowed(), "explicit requests must not change the device transfer preference");
    handle.shutdown();
    server.abort();
}
