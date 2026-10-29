use super::*;
use crate::library::handle::concurrency_tests::fixture;
use librqbit::{CreateTorrentOptions, SessionOptions};
use std::fs;
use std::sync::atomic::{AtomicU16, Ordering};
use sync_common::{BookId, ROOT_DIR_ID};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const EPUB: &[u8] = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
const OTHER: &[u8] = include_bytes!("../../crates/book-inspection/tests/fixtures/malformed-language.epub");

fn queued_record() -> TorrentRecord {
    TorrentRecord {
        info_hash: "a".repeat(40), metadata: Vec::new(), source: None,
        staging_path: ".bokheim-torrents/queued/Books".into(), output_path: "Books".into(),
        imported: false, publishing: false, keep_seeding: true, folder_id: None, error: None,
    }
}

#[tokio::test]
async fn an_offline_download_request_waits_without_an_engine_and_remains_cancellable() {
    use futures_util::poll;
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let (_connection, status) = watch::channel(false);
    library.torrents.set_connectivity(status);
    let request = library.torrents.begin_request("offline");
    let input = TorrentInput::Magnet(format!("magnet:?xt=urn:btih:{}", "a".repeat(40)));
    let mut adding = Box::pin(library.torrents.add_request(&library.db, root.path(), input, request));
    assert!(poll!(&mut adding).is_pending());
    assert!(library.torrents.engine.lock().await.is_none());
    assert_eq!(library.torrents.active_adds.load(Ordering::Acquire), 0);
    library.torrents.cancel_request("offline");
    assert!(adding.await.is_err());
    assert!(library.torrents.engine.lock().await.is_none());
}

#[tokio::test]
async fn failed_downloads_use_retry_delays_instead_of_active_progress_polling() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let record = queued_record();
    library.db.save_torrent(&record, &[]).unwrap();
    let policy = TransferPolicy::default();
    assert_eq!(library.torrents.next_wake(library, &policy, Duration::from_secs(900)).unwrap(), (Some(Duration::from_secs(2)), false));
    library.db.set_torrent_error(&record.info_hash, Some("download unavailable")).unwrap();
    for seconds in [30, 60, 900] {
        let delay = Duration::from_secs(seconds);
        assert_eq!(library.torrents.next_wake(library, &policy, delay).unwrap(), (Some(delay), true));
    }
}

#[tokio::test(start_paused = true)]
async fn offline_pending_downloads_have_no_progress_timer_or_network_engine() {
    use futures_util::task::{waker_ref, ArcWake};
    use std::future::Future;
    use std::task::Context;
    struct Wakes(AtomicUsize);
    impl ArcWake for Wakes {
        fn wake_by_ref(waker: &Arc<Self>) { waker.0.fetch_add(1, Ordering::Relaxed); }
    }
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    library.db.save_torrent(&queued_record(), &[]).unwrap();
    let policy = TransferPolicy::default();
    let (connection, status) = watch::channel(false);
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    let waker = waker_ref(&wakes);
    let mut context = Context::from_waker(&waker);
    let mut worker = Box::pin(library.torrents.run_with_network(library, &policy, Connectivity::new(status)));
    assert!(worker.as_mut().poll(&mut context).is_pending());
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
    assert!(library.torrents.engine.lock().await.is_none());
    connection.send_replace(true);
    assert!(wakes.0.load(Ordering::Relaxed) > 0, "reconnection wakes the pending download without a timer");
}

async fn local_engine(path: &Path) -> Arc<Session> {
    Session::new_with_opts(path.into(), SessionOptions { dht: None, listen: Some(Default::default()), disable_local_service_discovery: true, ..Default::default() }).await.unwrap()
}

async fn tracker(port: Arc<AtomicU16>) -> (String, tokio::task::JoinHandle<()>) {
    tracker_with_limit(port, None).await
}

async fn tracker_with_limit(port: Arc<AtomicU16>, peer_responses: Option<usize>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/announce", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut responses = 0;
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 8192];
            stream.read(&mut request).await.unwrap();
            let mut body = if peer_responses.is_none_or(|limit| responses < limit) {
                let mut body = b"d8:intervali1e5:peers6:".to_vec();
                body.extend([127, 0, 0, 1]);
                body.extend(port.load(Ordering::Relaxed).to_be_bytes());
                body
            } else {
                b"d8:intervali1e5:peers0:".to_vec()
            };
            responses += 1;
            body.push(b'e');
            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        }
    });
    (url, task)
}

async fn exercise(magnet: bool) {
    let seed_root = tempfile::tempdir().unwrap();
    let payload = seed_root.path().join("Books");
    fs::create_dir_all(payload.join("Nested")).unwrap();
    fs::write(payload.join("Novel.epub"), EPUB).unwrap();
    fs::write(payload.join("Nested/Other.epub"), OTHER).unwrap();
    fs::write(payload.join("notes.txt"), b"retain every file").unwrap();
    let seeder = local_engine(seed_root.path()).await;
    let port = Arc::new(AtomicU16::new(seeder.listen_addr().unwrap().port()));
    let (tracker_url, tracker_task) = tracker(port.clone()).await;
    let (metadata, seeded) = seeder.create_and_serve_torrent(&payload, CreateTorrentOptions { trackers: vec![tracker_url], ..Default::default() }).await.unwrap();
    seeded.wait_until_initialized().await.unwrap();
    assert!(seeded.stats().finished);
    let bytes = metadata.as_bytes().unwrap();
    let hash = metadata.info_hash().as_string();
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    *library.torrents.engine.lock().await = Some(local_engine(root.path()).await);
    let input = if magnet {
        // A magnet's selection hint must not prevent the remaining files
        // from downloading under Bokheim's all-files policy.
        TorrentInput::Magnet(format!("{}&so=0", metadata.as_magnet()))
    } else {
        TorrentInput::File(bytes.to_vec().into())
    };
    assert_eq!(library.add_torrent(input).await.unwrap(), hash);
    let policy = TransferPolicy::default();
    policy.set_network_cost(transfers::NetworkCost::Metered);
    assert!(library.torrents.next_check(library, &policy).unwrap().is_some(), "explicit torrents keep progressing on metered connections");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            library.torrents.pass_with_policy(library, &policy).await.unwrap();
            if library.db.torrents().unwrap()[0].imported {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("local torrent must download and publish");
    library.scan_folder().await.unwrap();
    let original = library.db.torrents().unwrap().remove(0);
    assert_eq!(fs::read(root.path().join("Books/Novel.epub")).unwrap(), EPUB);
    assert_eq!(fs::read(root.path().join("Books/Nested/Other.epub")).unwrap(), OTHER);
    assert_eq!(fs::read(root.path().join("Books/notes.txt")).unwrap(), b"retain every file");
    assert!(!root.path().join(&original.staging_path).exists(), "payload is moved, never copied");
    for book in [BookId::from(blake3::hash(EPUB)), BookId::from(blake3::hash(OTHER))] {
        let links = library.db.book_torrents(&book).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].info_hash, hash);
    }
    let folder = original.folder_id.expect("published torrent binds to its imported directory");
    let shelf = library.placement.create_directory(&ROOT_DIR_ID, "Shelf").unwrap().id;
    library.placement.queue_directory_move(&folder, Some(&shelf), Some("Moved books")).await.unwrap();
    library.run_file_jobs().await.unwrap();
    assert!(!root.path().join("Books").exists());
    assert_eq!(fs::read(root.path().join("Shelf/Moved books/notes.txt")).unwrap(), b"retain every file");
    library.torrents.pass(library).await.unwrap();
    let record = library.db.torrents().unwrap().remove(0);
    assert_eq!(library.db.torrent_output_path(&record).unwrap().as_deref(), Some("/Shelf/Moved books"));
    assert_eq!(library.db.book_torrents(&BookId::from(blake3::hash(EPUB))).unwrap()[0].info_hash, hash);
    let engine = library.torrents.engine.lock().await.as_ref().unwrap().clone();
    let handle = engine.get(torrent_id(&hash).unwrap()).unwrap();
    assert!(handle.stats().finished, "moved bytes are verified before seeding");
    assert_eq!(handle.output_folder(), root.path().join("Shelf/Moved books"));

    // Prove that seeding reads the relocated payload: remove the original
    // seed from the swarm and download into a fresh client's empty folder.
    seeder.stop().await;
    port.store(engine.listen_addr().unwrap().port(), Ordering::Relaxed);
    let receiver_root = tempfile::tempdir().unwrap();
    let receiver = local_engine(receiver_root.path()).await;
    let received = receiver.add_torrent(AddTorrent::from_bytes(bytes), None).await.unwrap().into_handle().unwrap();
    tokio::time::timeout(Duration::from_secs(30), received.wait_until_completed()).await.unwrap().unwrap();
    assert_eq!(fs::read(received.output_folder().join("Nested/Other.epub")).unwrap(), OTHER);
    receiver.stop().await;
    drop(handle);
    drop(engine);

    // Resume state and book associations survive closing and reopening.
    drop(actor);
    let (reopened, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &reopened.session;
    *library.torrents.engine.lock().await = Some(local_engine(root.path()).await);
    library.torrents.pass(library).await.unwrap();
    assert!(library.torrents.engine.lock().await.as_ref().unwrap().get(torrent_id(&hash).unwrap()).unwrap().stats().finished);
    assert_eq!(library.db.book_torrents(&BookId::from(blake3::hash(EPUB))).unwrap()[0].info_hash, hash);
    library.db.keep_torrent_seeding(&hash, false).unwrap();
    library.torrents.pass(library).await.unwrap();
    assert!(library.torrents.engine.lock().await.is_none(), "no peer or discovery work remains without active torrents");
    library.db.keep_torrent_seeding(&hash, true).unwrap();
    library.torrents.pass(library).await.unwrap();

    // Missing or changed library content must never be recreated or repaired
    // by the torrent engine when seeding resumes.
    drop(library.torrents.before_moves().await.unwrap());
    let notes = root.path().join("Shelf/Moved books/notes.txt");
    fs::remove_file(&notes).unwrap();
    library.torrents.pass(library).await.unwrap();
    assert!(!notes.exists());
    assert!(library.db.torrents().unwrap()[0].error.as_deref().unwrap().contains("missing"));
    fs::write(&notes, b"user edit").unwrap();
    library.torrents.pass(library).await.unwrap();
    assert!(!library.db.torrents().unwrap()[0].keep_seeding);
    assert_eq!(fs::read(&notes).unwrap(), b"user edit");
    fs::write(&notes, b"RETAIN EVERY FILE").unwrap();
    library.db.keep_torrent_seeding(&hash, true).unwrap();
    library.torrents.pass(library).await.unwrap();
    assert!(!library.db.torrents().unwrap()[0].keep_seeding, "same-size edits fail hash verification");
    assert_eq!(fs::read(&notes).unwrap(), b"RETAIN EVERY FILE");

    library.db.forget_torrent(&hash).unwrap();
    library.torrents.pass(library).await.unwrap();
    assert!(library.torrents.engine.lock().await.is_none(), "no peer or discovery work remains without active torrents");
    assert!(library.db.book_torrents(&BookId::from(blake3::hash(EPUB))).unwrap().is_empty());
    assert_eq!(fs::read(root.path().join("Shelf/Moved books/Novel.epub")).unwrap(), EPUB);
    tracker_task.abort();
}

#[tokio::test]
async fn torrent_file_downloads_import_links_move_seed_and_restart() {
    exercise(false).await;
}

#[tokio::test]
async fn magnet_downloads_every_file_despite_selection_hint() {
    exercise(true).await;
}

#[tokio::test]
async fn magnet_reuses_metadata_peers_when_the_tracker_stops_returning_them() {
    let seed_root = tempfile::tempdir().unwrap();
    let payload = seed_root.path().join("Novel.epub");
    fs::write(&payload, EPUB).unwrap();
    let seeder = local_engine(seed_root.path()).await;
    let (metadata, seeded) = seeder.create_and_serve_torrent(&payload, Default::default()).await.unwrap();
    seeded.wait_until_initialized().await.unwrap();
    let port = Arc::new(AtomicU16::new(seeder.listen_addr().unwrap().port()));
    // The metadata lookup gets the only tracker response containing a peer.
    // Payload transfer must reuse that peer without discovering it again.
    let (tracker_url, tracker_task) = tracker_with_limit(port, Some(1)).await;
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let engine = local_engine(root.path()).await;
    *library.torrents.engine.lock().await = Some(engine.clone());
    let hash = library.add_torrent(TorrentInput::Magnet(format!("{}&tr={tracker_url}", metadata.as_magnet()))).await.unwrap();
    let handle = engine.get(torrent_id(&hash).unwrap()).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(10), handle.wait_until_completed()).await;
    engine.stop().await;
    seeder.stop().await;
    tracker_task.abort();
    completed.expect("payload must download using the metadata peer").unwrap();
    assert_eq!(fs::read(handle.output_folder().join("Novel.epub")).unwrap(), EPUB);
}

#[tokio::test]
async fn cancelling_a_request_interrupts_engine_waiting() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let guard = library.torrents.engine.lock().await;
    let request = library.torrents.begin_request("pending-magnet");
    let (result, ()) = tokio::join!(library.add_torrent_request(TorrentInput::Magnet("magnet:?xt=urn:btih:0123456789012345678901234567890123456789".into()), request), async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(library.torrents.cancel_request("pending-magnet").is_none());
    });
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(library.torrents.requests.lock().unwrap().is_empty());
    assert!(library.db.torrents().unwrap().is_empty());
    drop(guard);
}

#[tokio::test]
async fn cancelling_payload_removes_staging_and_preserves_library_books() {
    let seed_root = tempfile::tempdir().unwrap();
    let payload = seed_root.path().join("Novel.epub");
    fs::write(&payload, EPUB).unwrap();
    let seeder = local_engine(seed_root.path()).await;
    let (metadata, _) = seeder.create_and_serve_torrent(&payload, Default::default()).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Existing.epub"), OTHER).unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let engine = local_engine(root.path()).await;
    *library.torrents.engine.lock().await = Some(engine.clone());
    let hash = library.add_torrent(TorrentInput::File(metadata.as_bytes().unwrap().to_vec().into())).await.unwrap();
    let record = library.db.torrents().unwrap().remove(0);
    library.cancel_torrent(&hash).await.unwrap();
    assert!(library.db.torrents().unwrap().is_empty());
    assert!(engine.get(torrent_id(&hash).unwrap()).is_none());
    assert!(!root.path().join(record.staging_path).exists());
    assert_eq!(fs::read(root.path().join("Existing.epub")).unwrap(), OTHER);
    library.torrents.pass(library).await.unwrap();
    engine.stop().await;
    seeder.stop().await;
}

#[tokio::test]
async fn interrupted_publication_recovers_destination_without_recreating_staging() {
    let root = tempfile::tempdir().unwrap();
    let payload = root.path().join("Books");
    fs::create_dir(&payload).unwrap();
    fs::write(payload.join("Novel.epub"), EPUB).unwrap();
    let engine = local_engine(root.path()).await;
    let (metadata, _) = engine.create_and_serve_torrent(&payload, Default::default()).await.unwrap();
    engine.delete(TorrentIdOrHash::Hash(metadata.info_hash()), false).await.unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    *library.torrents.engine.lock().await = Some(engine);
    let hash = metadata.info_hash().as_string();
    let record = TorrentRecord {
        info_hash: hash.clone(),
        metadata: metadata.as_bytes().unwrap().to_vec(),
        source: None,
        staging_path: ".bokheim-torrents/staged/Books".into(),
        output_path: "Books".into(),
        imported: false,
        publishing: false,
        keep_seeding: true,
        folder_id: None,
        error: None,
    };
    library.db.save_torrent(&record, &[(0, "Novel.epub".into())]).unwrap();
    // The rename finished, but the application closed before its DB ack.
    library.db.torrent_publish_intent(&hash, "Books").unwrap();
    library.torrents.pass(library).await.unwrap();
    let recovered = library.db.torrents().unwrap().remove(0);
    assert!(recovered.imported);
    assert!(!recovered.publishing);
    assert!(!root.path().join(&record.staging_path).exists());
    library.torrents.pass(library).await.unwrap();
    assert_eq!(fs::read(payload.join("Novel.epub")).unwrap(), EPUB);
    assert!(!root.path().join(&record.staging_path).exists());

    library.scan_folder().await.unwrap();
    let policy = client_platform_runtime::transfers::TransferPolicy::default();
    policy.set_network_cost(client_platform_runtime::transfers::NetworkCost::Metered);
    library.torrents.pass_with_policy(library, &policy).await.unwrap();
    assert!(library.torrents.engine.lock().await.is_none(), "tracker, discovery and peer traffic stop together");
    let waiting = library.db.torrents().unwrap().remove(0);
    assert!(waiting.imported && waiting.keep_seeding, "automatic suspension does not change the user's preference");
    let book = BookId::from(blake3::hash(EPUB));
    assert_eq!(library.db.book_torrents(&book).unwrap()[0].info_hash, hash);
    assert_eq!(fs::read(payload.join("Novel.epub")).unwrap(), EPUB);

    policy.set_network_cost(client_platform_runtime::transfers::NetworkCost::Unmetered);
    *library.torrents.engine.lock().await = Some(local_engine(root.path()).await);
    library.torrents.pass_with_policy(library, &policy).await.unwrap();
    assert!(library.torrents.engine.lock().await.as_ref().unwrap().get(torrent_id(&hash).unwrap()).unwrap().stats().finished);
    // Kobo disables background seeding independently of network-cost overrides.
    policy.set_allow_seeding(false);
    policy.set_allow_metered(true);
    library.torrents.pass_with_policy(library, &policy).await.unwrap();
    assert!(library.torrents.engine.lock().await.is_none());
    assert_eq!(library.torrents.next_check(library, &policy).unwrap(), None);
    assert!(library.db.torrents().unwrap()[0].keep_seeding, "device power policy preserves the stored preference");
}

#[tokio::test(start_paused = true)]
async fn empty_worker_has_no_periodic_wakes_and_observes_policy_changes() {
    use futures_util::task::{waker_ref, ArcWake};
    use std::future::Future;
    use std::task::Context;

    struct Wakes(AtomicUsize);
    impl ArcWake for Wakes {
        fn wake_by_ref(waker: &Arc<Self>) {
            waker.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let policy = TransferPolicy::default();
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    let waker = waker_ref(&wakes);
    let mut context = Context::from_waker(&waker);
    let mut worker = Box::pin(library.torrents.run_with_network(library, &policy, Connectivity::unrestricted()));
    assert!(worker.as_mut().poll(&mut context).is_pending());
    tokio::time::advance(Duration::from_secs(120)).await;
    assert_eq!(wakes.0.load(Ordering::Relaxed), 0, "an empty library has no torrent timer");
    policy.set_network_cost(transfers::NetworkCost::Metered);
    assert!(wakes.0.load(Ordering::Relaxed) > 0);
    assert!(worker.as_mut().poll(&mut context).is_pending());
    let previous = wakes.0.load(Ordering::Relaxed);
    tokio::time::advance(Duration::from_secs(120)).await;
    assert_eq!(wakes.0.load(Ordering::Relaxed), previous, "blocked torrents also wait for notifications");
    library.torrents.wake();
    assert!(wakes.0.load(Ordering::Relaxed) > previous, "commands wake an idle worker immediately");
}
