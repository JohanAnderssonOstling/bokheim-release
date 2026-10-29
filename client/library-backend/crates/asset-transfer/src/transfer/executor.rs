//! Typed asset execution on the library thread, with guarded database writes.
#[cfg(not(target_arch = "wasm32"))]
use crate::books::report_download_status;
#[cfg(not(target_arch = "wasm32"))]
use crate::deadline::download_deadline;
use crate::{books::BookTransfers, thumbnails::ThumbnailTransfers, TransferError};
use crate::{deadline::transfer_deadline, policy::SMALL_REQUEST_TIMEOUT};
use futures_util::{stream, StreamExt};
use library_database::Database;
use library_files::AssetStore;
use library_model::BookUploadIntent;
#[cfg(not(target_arch = "wasm32"))]
use library_model::{LibraryUpdate, TransferOrigin};
use library_runtime::{
    account::{LibraryAccount, SharedSyncCredentials},
    events::{LibraryEvent, LibraryEventRouter},
    interest::NotificationInterest,
};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use sync_common::{BookId, LibraryId};

pub struct AssetExecutor {
    pub asset_storage_enabled: Arc<AtomicBool>,
    pub assets: AssetStore,
    pub http_client: reqwest::Client,
    pub library_id: LibraryId,
    pub events: Option<LibraryEventRouter>,
    pub credentials: SharedSyncCredentials,
    pub account: Option<LibraryAccount>,
    pub notification_interest: NotificationInterest,
    pub availability_changed: Arc<AtomicBool>,
    pub database: Arc<Database>,
    pub cpu: Arc<dyn cpu_host::CpuHost>,
}

/// Poll uploads on the library thread. Ordinary failures leave later work
/// running; lost authentication stops the batch and drops in-flight requests.
async fn collect_uploads<F>(uploads: impl IntoIterator<Item = F>, concurrency: usize) -> Vec<Result<(), TransferError>>
where
    F: Future<Output = Result<(), TransferError>>,
{
    let uploads = stream::iter(uploads).buffer_unordered(concurrency);
    tokio::pin!(uploads);
    let mut results = Vec::new();
    while let Some(result) = uploads.next().await {
        let authentication = matches!(result, Err(TransferError::AuthenticationRequired));
        results.push(result);
        if authentication {
            break;
        }
    }
    results
}

impl AssetExecutor {
    fn asset_storage_enabled(&self) -> bool {
        self.asset_storage_enabled.load(Ordering::Acquire)
    }

    async fn negotiate_uploads(&self, intents: &[BookUploadIntent]) -> Result<sync_common::api::assets::BlobManifestResponse, TransferError> {
        let credentials = self.credentials()?;
        let manifest = intents.iter().map(|intent| sync_common::api::assets::BlobManifestEntry { content_hash: intent.content_hash, size_bytes: intent.size_bytes }).collect();
        let response = transfer_deadline(SMALL_REQUEST_TIMEOUT, crate::negotiate_blobs(&self.http_client, &credentials, &self.library_id, manifest)).await?.map_err(|error| match error {
            sync_transport::SyncRequestError::AuthenticationRequired => TransferError::AuthenticationRequired,
            error => TransferError::retryable(error),
        })?;
        self.check_transfer_account(&credentials)?;
        Ok(response)
    }

    /// Negotiate once per page; native uploads run four at a time, browser
    /// uploads sequentially. Every book writes as soon as its request finishes.
    pub async fn upload_books(&self, intents: &[BookUploadIntent]) -> Result<Vec<Result<(), TransferError>>, TransferError> {
        if !self.asset_storage_enabled() || intents.is_empty() {
            return Ok(Vec::new());
        }
        let _interest = self.notification_interest.work();
        let response = self.refreshed(|| self.negotiate_uploads(intents)).await?;
        let present: Vec<_> = intents.iter().filter(|intent| response.owned.contains(&intent.content_hash)).cloned().collect();
        self.database.settle_book_manifest(&present)?;
        let uploads = intents.iter().map(|intent| {
            let present = present.iter().any(|item| item.id == intent.id);
            let rejected = response.rejected.iter().find(|item| item.content_hash == intent.content_hash).map(|item| TransferError::from_upload_admission(item.reason));
            async move {
                if let Some(error) = rejected {
                    Err(error)
                } else if present {
                    Ok(())
                } else {
                    self.refreshed(|| self.upload_book(intent)).await
                }
            }
        });
        let concurrency = if cfg!(target_arch = "wasm32") { 1 } else { 4 };
        Ok(collect_uploads(uploads, concurrency).await)
    }

    async fn upload_book(&self, intent: &BookUploadIntent) -> Result<(), TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(());
        }
        let snapshot = self.database.transfer_snapshot(&[intent.content_hash])?;
        self.book_transfers().upload_blob_version(&snapshot, intent).await
    }

    /// Presence discovery also serves local extraction, which skips books with
    /// server covers. Validate the response before changing any presence facts.
    pub async fn cover_presence(&self, hashes: &[BookId]) -> Result<Vec<BookId>, TransferError> {
        self.refreshed(|| async {
            let credentials = self.credentials()?;
            let response = transfer_deadline(SMALL_REQUEST_TIMEOUT, sync_transport::thumbnail_presence(&self.http_client, &credentials, &self.library_id, hashes.to_vec())).await?.map_err(|error| match error {
                sync_transport::SyncRequestError::AuthenticationRequired => TransferError::AuthenticationRequired,
                error => TransferError::retryable(error),
            })?;
            let requested: std::collections::HashSet<_> = hashes.iter().copied().collect();
            let present: std::collections::HashSet<_> = response.present.iter().copied().collect();
            if present.len() != response.present.len() || !present.is_subset(&requested) {
                return Err(TransferError::retryable("invalid thumbnail presence response"));
            }
            self.check_transfer_account(&credentials)?;
            self.database.record_cover_presence(hashes, &response.present)?;
            Ok(hashes.iter().filter(|hash| !present.contains(hash)).copied().collect())
        })
        .await
    }

    pub async fn upload_covers(&self, hashes: &[BookId]) -> Result<Vec<Result<(), TransferError>>, TransferError> {
        if !self.asset_storage_enabled() {
            return Ok(Vec::new());
        }
        let _interest = self.notification_interest.work();
        let missing = self.cover_presence(hashes).await?;
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        match self.refreshed(|| async { self.thumbnail_transfers().upload_thumbnail_batch(&missing).await }).await {
            Err(TransferError::Rejected(_)) if missing.len() > 1 => {
                let uploads = missing.iter().map(|hash| self.refreshed(move || async move { self.thumbnail_transfers().upload_thumbnail_batch(std::slice::from_ref(hash)).await }));
                Ok(collect_uploads(uploads, 1).await)
            }
            result => Ok(vec![result]),
        }
    }

    pub async fn download_covers(&self, hashes: &[BookId]) -> Result<(), TransferError> {
        let _interest = self.notification_interest.work();
        self.refreshed(|| async { self.thumbnail_transfers().download_thumbnail_batch(hashes).await }).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn download_book(&self, hash: BookId) -> Result<(), TransferError> {
        let origin = self.database.book_download_origin(&hash)?.unwrap_or(TransferOrigin::Background);
        let result = self.download_book_with_origin(hash, origin).await;
        if let Err(error) = &result {
            self.report_download_failure(hash, origin, error);
        }
        result
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn report_download_failure(&self, hash: BookId, origin: TransferOrigin, error: &TransferError) {
        if origin == TransferOrigin::UserInitiated {
            // Alert even if a full disk also prevents clearing the request.
            // Local failures must not forget that the server holds this book.
            log::warn!("user download of {hash} failed: {error}");
            if let Some(events) = &self.events {
                events.publish_update(LibraryUpdate::DownloadFailed { content_hash: hash, failure: error.failure() });
            }
            if let Err(error) = self.database.complete_download(&hash) {
                log::warn!("could not clear failed user download {hash}: {error}");
            }
        }
        if let Err(error) = self.publish_download_status(hash) {
            log::warn!("could not publish failed download status for {hash}: {error}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn download_book_with_origin(&self, hash: BookId, origin: TransferOrigin) -> Result<(), TransferError> {
        let _interest = self.notification_interest.work();
        let paths = self.database.book_paths(&hash)?;
        if self.assets.book_exists(&hash, &paths)? {
            self.database.complete_local_book_request(&hash)?;
        } else {
            let snapshot = self.database.transfer_snapshot(&[hash])?;
            let placement = snapshot.placements.iter().find(|entry| entry.content_hash == hash && entry.book.is_some());
            if let Some(entry) = placement {
                let placement = library_model::BookPlacement::new(hash, entry.rel_path.clone());
                let result = self.refreshed(|| async { download_deadline(origin, crate::policy::BOOK_DOWNLOAD_TIMEOUT, self.book_transfers().download_blob(&snapshot, &hash, &placement)).await? }).await;
                if matches!(result, Err(TransferError::Rejected(_))) {
                    self.database.complete_download(&hash)?;
                }
                if let Err(error) = result {
                    return Err(error);
                }
            } else {
                self.database.complete_download(&hash)?;
            }
        }
        self.publish_download_status(hash)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn publish_download_status(&self, hash: BookId) -> Result<(), TransferError> {
        let present = self.assets.book_exists(&hash, &self.database.book_paths(&hash)?)?;
        let state = self.database.transfer_download_state(&hash, present)?;
        report_download_status(&self.availability_changed, self.events.as_ref(), hash, state);
        Ok(())
    }

    pub fn flush_availability_changes(&self) {
        if self.availability_changed.swap(false, Ordering::AcqRel) {
            if let Some(tx) = &self.events {
                let _ = tx.emit(LibraryEvent::ContentsChanged);
            }
        }
    }

    pub(super) fn credentials(&self) -> Result<sync_transport::SyncCredentials, TransferError> {
        if self.database.storage_policy()?.external().is_some() {
            return Err(TransferError::rejected("external library assets belong to the selected storage provider"));
        }
        self.credentials.credentials().ok_or(TransferError::AuthenticationRequired)
    }

    pub(crate) fn check_transfer_account(&self, credentials: &sync_transport::SyncCredentials) -> Result<(), TransferError> {
        if self.credentials()?.server_url() != credentials.server_url() {
            return Err(TransferError::retryable("credentials changed during asset transfer"));
        }
        Ok(())
    }

    pub(crate) fn thumbnail_transfers(&self) -> ThumbnailTransfers {
        ThumbnailTransfers {
            assets: self.assets.clone(),
            database: self.database.clone(),
            http_client: self.http_client.clone(),
            library_id: self.library_id,
            credentials: self.credentials.clone(),
            cpu: self.cpu.clone(),
            events: self.events.clone(),
        }
    }

    pub(crate) fn book_transfers(&self) -> BookTransfers {
        BookTransfers {
            assets: self.assets.clone(),
            database: self.database.clone(),
            http_client: self.http_client.clone(),
            library_id: self.library_id,
            credentials: self.credentials.clone(),
            availability_changed: self.availability_changed.clone(),
            events: self.events.clone(),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "executor/upload_tests.rs"]
mod upload_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod failure_tests {
    use super::*;
    use account_client::{Session, SessionPersistence};
    use book_inspection::InspectedBook;
    use book_model::{BookDetails, BookFormat, BookRecord};
    use client_platform_runtime::executor::BoxedBackendFuture;
    use client_runtime::BackendError;
    use cpu_host::{BookTask, BoxedCpuReader, CpuHost, CpuRequest, CpuResponse, ThumbnailBytes};
    use library_database::ImportCommit;
    use library_model::{BlobKind, DownloadState, PublishedStagedBook};
    use library_runtime::{account::LibraryAccountSession, events::LibraryEventState, scheduler::SyncScheduler};
    use std::{
        cell::Cell,
        io::{self, Error, ErrorKind},
        sync::RwLock,
    };
    use sync_common::ROOT_DIR_ID;

    struct UnusedCpu;
    impl CpuHost for UnusedCpu {
        fn book(&self, _: BookTask, _: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
            Box::pin(async { Err("unexpected CPU work".into()) })
        }
        fn request(&self, _: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
            Box::pin(async { Err("unexpected CPU work".into()) })
        }
        fn generate_thumbnail(&self, _: String, _: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailBytes>, BackendError>> {
            Box::pin(async { Err("unexpected CPU work".into()) })
        }
    }

    fn fixture(origin: TransferOrigin) -> (tempfile::TempDir, AssetExecutor, LibraryEventState, BookId) {
        let root = tempfile::tempdir().unwrap();
        let database = Arc::new(Database::open(":memory:").unwrap());
        database.initialize_library().unwrap();
        let hash = BookId::from_bytes([7; 32]);
        database
            .commit_import(ImportCommit {
                parent_id: ROOT_DIR_ID,
                file_name: "book.epub".into(),
                format: BookFormat::Epub,
                content_hash: hash,
                size_bytes: 4,
                inspection_version: 1,
                inspection: Some(InspectedBook { pdf: None, audiobook: None, metadata: BookRecord { metadata: BookDetails { title: "Remote book".into(), ..Default::default() }, description: String::new() }, toc: vec![] }),
                published: PublishedStagedBook { name: "book.epub".into(), relative_path: "/book.epub".into(), published: false, fingerprint: None },
                request_thumbnail: false,
                restore_paths: vec![],
            })
            .unwrap();
        database.record_remote_asset(BlobKind::Book, &hash).unwrap();
        database.sync_request_book_downloads(&[hash], origin).unwrap();
        let (events, state) = LibraryEventRouter::new(SyncScheduler::new().0, SyncScheduler::new().0, SyncScheduler::new().0, SyncScheduler::new().0);
        let executor = AssetExecutor {
            asset_storage_enabled: Arc::new(AtomicBool::new(true)),
            assets: AssetStore::open(root.path().to_str().unwrap()).unwrap(),
            http_client: reqwest::Client::new(),
            library_id: LibraryId::new_v4(),
            events: Some(events),
            credentials: Arc::new(RwLock::new(None)),
            account: None,
            notification_interest: Default::default(),
            availability_changed: Arc::new(AtomicBool::new(false)),
            database,
            cpu: Arc::new(UnusedCpu),
        };
        (root, executor, state, hash)
    }

    #[derive(Clone, Debug)]
    struct Persistence;
    impl SessionPersistence for Persistence {
        fn load_account_session(&self) -> io::Result<Option<Session>> {
            Ok(None)
        }
        fn save_account_session(&self, _: &Session) -> io::Result<()> {
            Ok(())
        }
        fn clear_account_session(&self) -> io::Result<()> {
            Ok(())
        }
    }

    fn session(user: &str, token: char) -> Session {
        Session::try_new("https://example.com".parse().unwrap(), user.into(), format!("{user}@example.com"), token.to_string().repeat(43)).unwrap()
    }

    fn install_account(executor: &mut AssetExecutor) -> LibraryAccountSession<Persistence> {
        let account = LibraryAccountSession::load(Persistence, "https://example.com".parse().unwrap()).unwrap();
        account.set(session("reader", 'a')).unwrap();
        let capability = account.library_account();
        executor.credentials = capability.credentials();
        executor.account = Some(capability);
        account
    }

    #[tokio::test]
    async fn transfer_preflight_rejects_missing_credentials_and_leaves_other_results_unchanged() {
        let (_root, mut executor, _state, _hash) = fixture(TransferOrigin::Background);
        let calls = Cell::new(0);
        let error = executor
            .refreshed(|| {
                calls.set(calls.get() + 1);
                std::future::ready(Ok(()))
            })
            .await
            .unwrap_err();
        assert!(matches!(error, TransferError::AuthenticationRequired));
        assert_eq!(calls.get(), 0);

        executor.credentials = Arc::new(RwLock::new(Some(sync_transport::SyncCredentials::new("https://example.com".parse().unwrap(), "fixture-token"))));
        for expected in [Ok(()), Err(TransferError::retryable("temporary failure")), Err(TransferError::rejected("permanent failure")), Err(TransferError::AuthenticationRequired)] {
            calls.set(0);
            let result = executor
                .refreshed(|| {
                    calls.set(calls.get() + 1);
                    std::future::ready(expected.clone())
                })
                .await;
            assert_eq!(calls.get(), 1, "a transfer without account renewal cannot retry");
            assert_eq!(result.as_ref().err().map(ToString::to_string), expected.as_ref().err().map(ToString::to_string));
        }
    }

    #[tokio::test]
    async fn concurrent_token_renewal_retries_once_and_keeps_the_renewed_session() {
        for succeeds in [true, false] {
            let (_root, mut executor, _state, _hash) = fixture(TransferOrigin::Background);
            let account = install_account(&mut executor);
            let calls = Cell::new(0);
            let result = executor
                .refreshed(|| async {
                    calls.set(calls.get() + 1);
                    if calls.get() == 1 {
                        // Another worker renewed the token while the first request ran.
                        account.set(session("reader", 'b')).unwrap();
                        return Err(TransferError::AuthenticationRequired);
                    }
                    assert_eq!(account.require().unwrap().token(), "b".repeat(43));
                    if succeeds {
                        Ok(42)
                    } else {
                        Err(TransferError::AuthenticationRequired)
                    }
                })
                .await;
            assert_eq!(calls.get(), 2);
            if succeeds {
                assert_eq!(result.unwrap(), 42);
            } else {
                assert!(matches!(result, Err(TransferError::AuthenticationRequired)));
            }
            assert_eq!(account.require().unwrap().token(), "b".repeat(43), "a second rejection must not start another renewal");
        }
    }

    #[tokio::test]
    async fn retry_cannot_adopt_an_account_that_signed_in_during_the_transfer() {
        let (_root, mut executor, _state, _hash) = fixture(TransferOrigin::Background);
        let account = install_account(&mut executor);
        let calls = Cell::new(0);
        let result: Result<(), TransferError> = executor
            .refreshed(|| async {
                calls.set(calls.get() + 1);
                account.set(session("other-reader", 'b')).unwrap();
                Err(TransferError::AuthenticationRequired)
            })
            .await;
        assert!(matches!(result, Err(TransferError::AuthenticationRequired)));
        assert_eq!(calls.get(), 1);
        let current = account.require().unwrap();
        assert_eq!(current.user_id(), "other-reader");
        assert_eq!(current.token(), "b".repeat(43));
    }

    #[tokio::test]
    async fn an_unrefreshable_rejected_session_stops_before_a_second_transfer() {
        let (_root, mut executor, _state, _hash) = fixture(TransferOrigin::Background);
        let account = install_account(&mut executor);
        let calls = Cell::new(0);
        let result: Result<(), TransferError> = executor
            .refreshed(|| async {
                calls.set(calls.get() + 1);
                Err(TransferError::AuthenticationRequired)
            })
            .await;
        assert!(matches!(result, Err(TransferError::AuthenticationRequired)));
        assert_eq!(calls.get(), 1);
        assert!(account.current().unwrap().is_none());
    }

    #[test]
    fn user_storage_failures_alert_and_allow_an_explicit_retry() {
        for kind in [ErrorKind::StorageFull, ErrorKind::PermissionDenied] {
            let (_root, executor, mut state, hash) = fixture(TransferOrigin::UserInitiated);
            let updates = state.subscribe_updates();
            let error = TransferError::retryable(Error::from(kind));
            executor.report_download_failure(hash, TransferOrigin::UserInitiated, &error);
            state.drain(executor.events.as_ref().unwrap());
            assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::DownloadFailed { content_hash: hash, failure: error.failure() });
            assert!(error.failure().is_some(), "storage failures are classified");
            assert_eq!(executor.database.book_download_origin(&hash).unwrap(), None);
            assert_eq!(state.download_state(&hash), Some(&DownloadState::NotDownloaded));
            assert!(executor.database.remote_asset_hashes(BlobKind::Book).unwrap().contains(&hash), "a local failure must preserve server presence");
            executor.database.sync_request_book_downloads(&[hash], TransferOrigin::UserInitiated).unwrap();
            assert_eq!(executor.database.book_download_origin(&hash).unwrap(), Some(TransferOrigin::UserInitiated));
        }
    }

    fn thumbnail_server(thumbnail: sync_common::api::assets::ThumbnailBatchEntry) -> (sync_transport::SyncCredentials, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use sync_common::{api::assets::ThumbnailBatchDownloadResponse, transport};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut socket = BufReader::new(socket);
            let mut length = 0;
            let mut line = String::new();
            socket.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /api/libraries/"));
            loop {
                line.clear();
                socket.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            socket.read_exact(&mut vec![0; length]).unwrap();
            let bytes = transport::encode(&ThumbnailBatchDownloadResponse { thumbnails: vec![thumbnail], missing: vec![] }).unwrap();
            write!(socket.get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            socket.get_mut().write_all(&bytes).unwrap();
        });
        (sync_transport::SyncCredentials::new(origin, "test-token"), server)
    }

    #[tokio::test]
    async fn server_thumbnail_download_preserves_both_images_without_cpu_work() {
        use library_files::asset_store::thumbnail_variant_name;
        use sync_common::api::assets::ThumbnailBatchEntry;

        // Cover both a fresh download and an older cache with only a master.
        for cached_master in [false, true] {
            let (_root, mut executor, _state, hash) = fixture(TransferOrigin::Background);
            if cached_master {
                executor.assets.write(BlobKind::Thumbnail, &hash, b"\xff\xd8\xffold master").await.unwrap();
            }
            let master = b"\xff\xd8\xffserver master";
            let browse = b"\xff\xd8\xffserver browse";
            let (credentials, server) = thumbnail_server(ThumbnailBatchEntry { content_hash: hash, bytes: master.to_vec(), browse_bytes: browse.to_vec() });
            executor.credentials = Arc::new(RwLock::new(Some(credentials)));
            let transfers = executor.thumbnail_transfers();
            transfers.download_thumbnail_batch(&[hash]).await.unwrap();
            server.join().unwrap();
            assert_eq!(executor.assets.read_bytes(BlobKind::Thumbnail, &hash).await.unwrap().unwrap(), master);
            assert_eq!(executor.assets.read_named(thumbnail_variant_name(&hash, thumbnail::BROWSE_THUMBNAIL_WIDTH)).await.unwrap().unwrap(), browse);
            assert_eq!(executor.assets.read_named(thumbnail_variant_name(&hash, thumbnail::THUMBNAIL_WIDTH)).await.unwrap().unwrap(), master);
            assert_eq!(executor.database.thumbnail_work_state(&hash).unwrap().as_deref(), Some("ready"));
            // The server has stopped: a cached pair must require no further request.
            transfers.download_thumbnail_batch(&[hash]).await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_server_browse_thumbnail_is_rejected_before_storage() {
        use sync_common::api::assets::ThumbnailBatchEntry;

        let (_root, mut executor, _state, hash) = fixture(TransferOrigin::Background);
        let (credentials, server) = thumbnail_server(ThumbnailBatchEntry { content_hash: hash, bytes: b"\xff\xd8\xffmaster".to_vec(), browse_bytes: b"not a JPEG".to_vec() });
        executor.credentials = Arc::new(RwLock::new(Some(credentials)));
        assert!(executor.thumbnail_transfers().download_thumbnail_batch(&[hash]).await.is_err());
        server.join().unwrap();
        assert!(!executor.assets.exists(BlobKind::Thumbnail, &hash).unwrap());
        assert!(!executor.assets.thumbnail_set_exists(&hash).unwrap());
    }

    #[tokio::test]
    async fn local_recovery_leaves_server_master_untouched() {
        use crate::thumbnail_jobs::{ThumbnailExtractor, ThumbnailJobs, ThumbnailObserver};

        struct Observer;
        impl ThumbnailObserver for Observer {
            fn completed(&mut self) {}
            fn ready(&mut self, _: BookId) {
                panic!("server cover must be downloaded, not generated");
            }
        }

        let (_root, executor, _state, hash) = fixture(TransferOrigin::Background);
        let master = b"\xff\xd8\xffcached server master";
        executor.assets.write(BlobKind::Thumbnail, &hash, master).await.unwrap();
        executor.database.record_remote_asset(BlobKind::Thumbnail, &hash).unwrap();
        executor.database.shared_request_thumbnail(&hash).unwrap();
        let processor = ThumbnailExtractor::new(executor.cpu.clone());
        let jobs = ThumbnailJobs::new(&executor.database, &executor.assets, &processor);
        jobs.run(vec![hash], &mut Observer).await.unwrap();
        assert_eq!(executor.assets.read_bytes(BlobKind::Thumbnail, &hash).await.unwrap().unwrap(), master);
    }

    #[test]
    fn background_storage_failures_remain_queued_without_user_alerts() {
        for kind in [ErrorKind::StorageFull, ErrorKind::PermissionDenied] {
            let (_root, executor, mut state, hash) = fixture(TransferOrigin::Background);
            let updates = state.subscribe_updates();
            executor.report_download_failure(hash, TransferOrigin::Background, &TransferError::retryable(Error::from(kind)));
            state.drain(executor.events.as_ref().unwrap());
            assert_eq!(executor.database.book_download_origin(&hash).unwrap(), Some(TransferOrigin::Background));
            assert_eq!(state.download_state(&hash), Some(&DownloadState::Queued));
            assert!(!std::iter::from_fn(|| updates.try_recv().ok()).any(|update| matches!(update, LibraryUpdate::DownloadFailed { .. })));
        }
    }

    #[tokio::test]
    async fn a_failure_before_download_bytes_arrive_also_reaches_the_user() {
        let (_root, executor, mut state, hash) = fixture(TransferOrigin::UserInitiated);
        let updates = state.subscribe_updates();
        assert!(matches!(executor.download_book(hash).await, Err(TransferError::AuthenticationRequired)));
        state.drain(executor.events.as_ref().unwrap());
        assert!(matches!(updates.try_recv().unwrap(), LibraryUpdate::DownloadFailed { content_hash, .. } if content_hash == hash));
        assert_eq!(executor.database.book_download_origin(&hash).unwrap(), None);
    }

    #[tokio::test]
    async fn actual_download_space_check_alerts_only_for_user_actions() {
        use std::time::Duration;
        use sync_transport::SyncCredentials;
        use tokio::{
            io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
            net::TcpListener,
            time::timeout,
        };

        for origin in [TransferOrigin::UserInitiated, TransferOrigin::Background] {
            let (_root, mut executor, mut state, hash) = fixture(origin);
            let updates = state.subscribe_updates();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let server_url = format!("http://{}", listener.local_addr().unwrap()).parse().unwrap();
            executor.credentials = Arc::new(RwLock::new(Some(SyncCredentials::new(server_url, "test-token"))));
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut line = String::new();
                loop {
                    line.clear();
                    if socket.read_line(&mut line).await.unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                // Advertise more bytes than any drive can hold. No disk needs
                // to be filled and no response body needs to be downloaded.
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9223372036854775807\r\nConnection: close\r\n\r\n").await.unwrap();
            });
            let error = timeout(Duration::from_secs(5), executor.download_book(hash)).await.unwrap().unwrap_err();
            server.await.unwrap();
            assert_eq!(error.failure(), Some(client_runtime::Failure::StorageFull), "{error}");
            state.drain(executor.events.as_ref().unwrap());
            let alerted = std::iter::from_fn(|| updates.try_recv().ok()).any(|update| matches!(update, LibraryUpdate::DownloadFailed { .. }));
            assert_eq!(alerted, origin == TransferOrigin::UserInitiated);
            assert_eq!(executor.database.book_download_origin(&hash).unwrap(), if origin == TransferOrigin::Background { Some(origin) } else { None });
            assert!(executor.database.remote_asset_hashes(BlobKind::Book).unwrap().contains(&hash));
        }
    }
}
