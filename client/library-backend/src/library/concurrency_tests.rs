use super::*;
use std::time::Duration;

#[derive(Clone, Debug)]
struct Account(account_client::Session);
impl account_client::SessionPersistence for Account {
    fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
        Ok(Some(self.0.clone()))
    }
    fn save_account_session(&self, _: &account_client::Session) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_account_session(&self) -> std::io::Result<()> {
        Ok(())
    }
}

fn fixture(root: &std::path::Path, url: account_client::ServerUrl) -> (LibraryActor, LibraryHandle, LibraryEventReceiver, LibraryEventSender, async_channel::Receiver<LibraryActorCommand>) {
    let account = library_runtime::account::LibraryAccountSession::load(Account(account_client::Session::try_new(url.clone(), "user".into(), "reader@example.com".into(), "a".repeat(43)).unwrap()), url.clone()).unwrap();
    let mut session = LibrarySession::open(super::super::LibraryRuntimeSpec {
        config: super::super::LibraryOpenConfig {
            id: LibraryId::new_v4(),
            display_name: "Concurrency".into(),
            root: root.to_string_lossy().into_owned(),
            database_locator: root.join("library.sqlite").to_string_lossy().into_owned(),
            default_sync_server_url: url,
            metadata_server_url: "http://127.0.0.1:1".into(),
            asset_storage_enabled: true,
            reset_cloud_presence: false,
        },
        account: account.library_account(),
        notification_interest: Default::default(),
        remote_changes: Default::default(),
    })
    .unwrap();
    let events = session.event_rx.take().unwrap();
    let event_tx = session.event_tx.clone();
    let (commands, inbox) = async_channel::unbounded();
    let handle = LibraryHandle { id: session.id, commands, subscriptions: session.subscriptions.clone() };
    let actor = LibraryActor::new(session);
    (actor, handle, events, event_tx, inbox)
}

fn enqueue(handle: &LibraryHandle, command: LibraryCommand) -> async_channel::Receiver<Result<client_runtime::reply::Reply, BackendError>> {
    let (reply, receiver) = async_channel::bounded(1);
    handle.commands.try_send(LibraryActorCommand::Ui { command, reply }).unwrap_or_else(|_| panic!("actor open"));
    receiver
}

#[tokio::test]
async fn stalled_http_does_not_block_queries_mutations_events_scans_or_retirement() {
    for download in [false, true] {
        let started = Arc::new(tokio::sync::Notify::new());
        let notify = started.clone();
        let service = axum::Router::new().fallback(move || {
            let notify = notify.clone();
            async move {
                notify.notify_one();
                std::future::pending::<axum::http::StatusCode>().await
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap()).parse().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let (mut actor, handle, events, event_tx, inbox) = fixture(root.path(), url);
        let local = tokio::task::LocalSet::new();
        let _entered = local.enter();
        std::rc::Rc::get_mut(&mut actor.session).unwrap().start_assets();
        let hash = ContentHash::new(&format!("{:064x}", 1));
        crate::test_database::raw(&actor.session.db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'Remote','epub')", [hash.as_str()]).unwrap();
        crate::test_database::raw(&actor.session.db).execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,'remote.epub','',0 FROM book", [sync_common::ROOT_DIR_ID.to_string()]).unwrap();
        let (release_scan, scan) = async_channel::bounded::<()>(1);
        actor.session.filesystem_scanning.set(true);
        actor.scan_job.task = Some(Box::pin(async move {
            scan.recv().await.unwrap();
            Ok(Some(library_database::ScanApplyResult::Applied))
        }));
        let updates = handle.subscribe_updates();
        let work = enqueue(&handle, if download { LibraryCommand::DownloadBook { content_hash: hash } } else { LibraryCommand::RenewRemoteAudio { content_hash: hash } });
        let event_commands = handle.commands.clone();
        let exercise = async {
            tokio::select! {
                _ = started.notified() => {},
                result = work.recv(), if !download => panic!("request ended before HTTP was held: {}", result.unwrap().err().unwrap()),
            }
            if download {
                assert!(matches!(work.recv().await.unwrap().unwrap().take::<DownloadState>().unwrap(), DownloadState::Queued));
            } else {
                assert!(work.try_recv().is_err());
            }
            let count = handle.dispatch(LibraryCommand::BookCount).await.unwrap().take::<usize>().unwrap();
            assert_eq!(count, 1);
            // Both writes are admitted before the read; they must execute in
            // receive order, not completion order in the operation set.
            let first = enqueue(&handle, LibraryCommand::UpdateReadingPosition { content_hash: hash, position: "epubcfi(/6/2)".into(), progress: None, entry: None });
            let second = enqueue(&handle, LibraryCommand::UpdateReadingPosition { content_hash: hash, position: "epubcfi(/6/4)".into(), progress: None, entry: None });
            let position = enqueue(&handle, LibraryCommand::ReadingPosition { content_hash: hash });
            assert!(first.recv().await.unwrap().is_ok());
            assert!(second.recv().await.unwrap().is_ok());
            assert_eq!(position.recv().await.unwrap().unwrap().take::<Option<String>>().unwrap().as_deref(), Some("epubcfi(/6/4)"));
            event_tx.try_send(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued }).unwrap();
            while !matches!(updates.recv().await.unwrap(), library_model::LibraryUpdate::Download(_)) {}
            release_scan.send(()).await.unwrap();
            while !matches!(updates.recv().await.unwrap(), library_model::LibraryUpdate::Scanning(false)) {}
            if !download {
                assert!(work.try_recv().is_err(), "HTTP must remain blocked throughout the responsiveness checks");
            }
            handle.shutdown();
            if !download {
                assert!(work.recv().await.unwrap().is_err());
            }
            while updates.recv().await.is_ok() {}
            assert!(handle.dispatch(LibraryCommand::BookCount).await.is_err());
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            local.run_until(async {
                tokio::join!(actor.run(inbox, events, event_commands), exercise);
            }),
        )
        .await;
        server.abort();
        outcome.expect("held HTTP must never prevent unrelated work or shutdown");
    }
}

#[tokio::test]
async fn folder_replay_runs_while_enrichment_is_stalled() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, handle, events, _, inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    actor.session.filesystem_scanning.set(false);
    let folder = actor.session.create_directory(&sync_common::ROOT_DIR_ID, "Before").unwrap();
    actor.session.assets.placement().prepare_directory_with_database(&actor.session.db, folder.id).await.unwrap();
    assert!(root.path().join("Before").is_dir());
    // A held enrichment batch must not be replaced, completed, or awaited
    // before either the first replay or a subsequent wake can make progress.
    actor.enrichment_job.task = Some(Box::pin(std::future::pending()));
    let event_commands = handle.commands.clone();
    let exercise = async {
        for name in ["After", "Again"] {
            handle.dispatch(LibraryCommand::RenameDirectory { directory_id: folder.id, name: name.into() }).await.unwrap();
            while !root.path().join(name).is_dir() {
                tokio::task::yield_now().await;
            }
        }
        assert!(!root.path().join("Before").exists());
        assert!(!root.path().join("After").exists());
        handle.shutdown();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(actor.run(inbox, events, event_commands), exercise);
    })
    .await
    .expect("physical renames must not wait for enrichment");
}

#[tokio::test]
async fn retiring_before_first_poll_discards_queued_commands_and_replies() {
    let root = tempfile::tempdir().unwrap();
    let (actor, handle, events, _, inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let response = enqueue(&handle, LibraryCommand::CreateDirectory { parent_id: sync_common::ROOT_DIR_ID, name: "Must not run".into() });
    let database = actor.session.db.clone();
    handle.shutdown();
    actor.run(inbox, events, handle.commands.clone()).await;
    assert!(response.recv().await.is_err());
    assert_eq!(crate::test_database::raw(&database).query_row("SELECT count(*) FROM dir WHERE name='Must not run'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn unpolled_staged_preparation_does_not_leak_import_activity() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let work = super::super::commands::prepare_library_request(
        actor.session.clone(),
        LibraryCommand::PrepareStagedDirectoryImport { activity: uuid::Uuid::new_v4(), job: uuid::Uuid::new_v4().to_string(), parent_id: sync_common::ROOT_DIR_ID, name: Some("Unstarted".into()), directories: vec![] },
    )
    .unwrap();
    assert!(actor.session.runtime.has_directory_imports());
    drop(work);
    assert!(!actor.session.runtime.has_directory_imports());
}

#[tokio::test]
async fn staged_preparation_hands_off_only_after_success() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    for valid in [false, true] {
        let activity = uuid::Uuid::new_v4();
        let work = super::super::commands::prepare_library_request(
            actor.session.clone(),
            LibraryCommand::PrepareStagedDirectoryImport { activity, job: if valid { uuid::Uuid::new_v4().to_string() } else { "invalid journal".into() }, parent_id: sync_common::ROOT_DIR_ID, name: None, directories: vec![] },
        )
        .unwrap();
        let PreparedRequest::Pending(_, operation) = work else { panic!("preparation must run beside commands") };
        assert_eq!(operation.await.is_ok(), valid);
        assert_eq!(actor.session.runtime.has_directory_imports(), valid);
        actor.session.finish_directory_import(activity);
        assert!(!actor.session.runtime.has_directory_imports());
    }
}

#[tokio::test(start_paused = true)]
async fn local_retry_runs_without_an_external_wake_and_clears_file_error() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, handle, events, _, inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    actor.session.db.create_directory(&sync_common::ROOT_DIR_ID, &"Retry folder".to_owned()).unwrap();
    while events.try_recv().is_ok() {}
    // Keep enrichment held: it must not prevent retrying local file work.
    actor.enrichment_job.task = Some(Box::pin(std::future::pending()));
    actor.finish_file_work(Err(BackendError::message("storage temporarily unavailable")));
    let snapshot = actor.session.transfer_snapshot().unwrap();
    assert!(snapshot.file_work_pending);
    assert_eq!(snapshot.file_work_error.as_deref(), Some("storage temporarily unavailable"));
    assert!(actor.local_retry.task.is_some());
    let session = actor.session.clone();
    let commands = handle.commands.clone();
    let exercise = async {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(!root.path().join("Retry folder").exists());
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..1000 {
            if !session.pending_file_jobs().unwrap() && session.file_work_error.borrow().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(root.path().join("Retry folder").is_dir());
        let snapshot = session.transfer_snapshot().unwrap();
        assert!(!snapshot.file_work_pending);
        assert!(snapshot.file_work_error.is_none());
        handle.shutdown();
    };
    tokio::join!(actor.run(inbox, events, commands), exercise);
}

#[tokio::test(start_paused = true)]
async fn enrichment_failure_retries_without_restarting_upload_admission() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    actor.finish_enrichment(Err(BackendError::message("temporary enrichment failure")));
    assert!(actor.local_retry.task.is_none(), "enrichment retries must not restart file replay or upload admission");
    let retry = actor.enrichment_retry.task.as_mut().unwrap();
    assert!(futures_util::poll!(retry.as_mut()).is_pending());
    tokio::time::advance(Duration::from_secs(20)).await;
    actor.finish_enrichment(Err(BackendError::message("another failure")));
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(futures_util::poll!(actor.enrichment_retry.task.as_mut().unwrap().as_mut()).is_ready(), "another failure must not postpone the retry");
}

#[tokio::test]
async fn deferred_thumbnail_work_schedules_retry_after_a_successful_pass() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let hash = ContentHash::new(&"b".repeat(64));
    crate::test_database::raw(&actor.session.db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'Deferred','epub')", [hash.as_str()]).unwrap();
    crate::test_database::raw(&actor.session.db).execute("INSERT INTO local_thumbnail_work(content_hash,state,retry_after) VALUES(?1,'pending',unixepoch()+300)", [hash.as_str()]).unwrap();
    actor.finish_enrichment(Ok(()));
    assert!(actor.enrichment_retry.task.is_some(), "a pass can succeed while thumbnails await their retry deadline");
}

#[tokio::test]
async fn create_folder_materializes_before_returning_without_queued_work() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    std::fs::create_dir(root.path().join("Shelf")).unwrap();
    std::fs::write(root.path().join("Shelf/keep.txt"), "existing data").unwrap();
    let folder = actor.session.create_directory(&sync_common::ROOT_DIR_ID, "Shelf").unwrap();
    let relative = actor.session.db.directory_relative_path_string(&folder.id).unwrap().unwrap();
    assert_ne!(relative, "Shelf");
    assert_eq!(std::fs::read_to_string(root.path().join(&relative).join(".biblos_uuid")).unwrap(), folder.id.to_string());
    let child = actor.session.create_directory(&folder.id, "Child").unwrap();
    assert_eq!(std::fs::read_to_string(root.path().join(&relative).join("Child/.biblos_uuid")).unwrap(), child.id.to_string());
    assert!(!actor.session.pending_file_jobs().unwrap());
    assert_eq!(std::fs::read_to_string(root.path().join("Shelf/keep.txt")).unwrap(), "existing data");
    // An unavailable parent must fail before creating a database record.
    std::fs::rename(root.path().join(&relative), root.path().join("Moved externally")).unwrap();
    assert!(actor.session.create_directory(&folder.id, "Cannot create").is_err());
    assert_eq!(crate::test_database::raw(&actor.session.db).query_row("SELECT count(*) FROM dir WHERE name='Cannot create'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert!(!actor.session.pending_file_jobs().unwrap());
}

#[cfg(feature = "scanner")]
#[tokio::test]
async fn scan_recovers_folder_created_before_database_registration() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let id = DirId::new_v4();
    actor.session.assets.placement().create_directory("Recovered", id).unwrap();
    assert!(actor.session.db.directory_relative_path_string(&id).unwrap().is_none());
    actor.begin_scan();
    let result = actor.scan_job.task.as_mut().unwrap().await;
    actor.finish_scan(result);
    assert_eq!(actor.session.db.directory_relative_path_string(&id).unwrap().as_deref(), Some("Recovered"));
    assert!(!actor.session.pending_file_jobs().unwrap());
}

#[cfg(feature = "scanner")]
#[tokio::test]
async fn invalid_book_finishes_scan_with_a_file_error() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    std::fs::write(root.path().join("Broken.epub"), b"not a zip archive").unwrap();
    actor.begin_scan();
    let result = actor.scan_job.task.as_mut().unwrap().await;
    actor.finish_scan(result);
    assert!(!actor.session.scanner_running());
    assert!(!actor.session.db.needs_scan().unwrap());
    let status = actor.session.db.operation_status(library_database::OperationActivity::default()).unwrap().operation.unwrap();
    assert_eq!(status.state, library_replica::LibraryOperationState::Completed);
    assert!(status.waiting_reason.is_none());
    assert_eq!(status.scan_failures.len(), 1);
    assert_eq!(status.scan_failures[0].0, "Broken.epub");
    assert!(!status.scan_failures[0].1.is_empty());
}

#[tokio::test]
async fn reader_waits_for_scheduler_and_cancellation_keeps_the_request() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let hash = ContentHash::new(&"e".repeat(64));
    crate::test_database::raw(&actor.session.db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'Remote','epub')", [hash.as_str()]).unwrap();
    crate::test_database::raw(&actor.session.db).execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,'remote.epub','',0 FROM book", [sync_common::ROOT_DIR_ID.to_string()]).unwrap();
    let mut reader = Box::pin(actor.session.request_download(hash));
    assert!(futures_util::poll!(reader.as_mut()).is_pending(), "reader must wait, not make its own HTTP request");
    assert!(actor.operations.is_empty());
    assert_eq!(actor.session.sync().unwrap().transfer_statuses()[0].total_items, 1);
    drop(reader);
    assert!(actor.session.db.is_book_download_requested(&hash).unwrap());
    let mut reader = Box::pin(actor.session.request_download(hash));
    assert!(futures_util::poll!(reader.as_mut()).is_pending());
    assert_eq!(actor.session.sync().unwrap().transfer_statuses()[0].total_items, 1, "repeated requests share one queued transfer");
    std::fs::write(root.path().join("remote.epub"), b"downloaded bytes").unwrap();
    crate::test_database::raw(&actor.session.db).execute("UPDATE book_dir SET local_hash=?1,is_downloaded=1", [hash.as_str()]).unwrap();
    actor.session.db.complete_local_book_request(hash).unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), reader).await.unwrap().unwrap(), DownloadState::Downloaded);
}

#[tokio::test]
async fn stalled_enrichment_does_not_hold_new_upload_admission() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, _, _, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    actor.enrichment_job.task = Some(Box::pin(std::future::pending()));
    let hash = ContentHash::new(&"f".repeat(64));
    crate::test_database::raw(&actor.session.db).execute("INSERT INTO book(content_hash,title,format) VALUES(?1,'New book','epub')", [hash.as_str()]).unwrap();
    actor.admit_and_enrich();
    assert_eq!(crate::test_database::raw(&actor.session.db).query_row("SELECT COUNT(*) FROM local_upload_preparation WHERE content_hash=?1", [hash.as_str()], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert!(actor.enrichment_job.task.is_some());
    assert!(actor.enrichment_job.pending);
}
