use super::*;
use crate::library::{LibraryCommand, LibraryDirectory, LibraryNotifications, ReadyCommand};
use library_runtime::interest::{InterestState, NotificationInterest};
use std::sync::Arc;
use std::time::Duration;
use sync_common::api::notifications::NotificationMessage;
use web_time::Instant;

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

pub(in crate::library) fn fixture(root: &std::path::Path, url: account_client::Url) -> (LibraryActor, LibraryHandle, async_channel::Receiver<LibraryActorCommand>) {
    let account = library_runtime::account::LibraryAccountSession::load(Account(account_client::Session::try_new(url.clone(), "user".into(), "reader@example.com".into(), "a".repeat(43)).unwrap()), url.clone()).unwrap();
    let session = LibrarySession::open(super::super::LibraryRuntimeSpec {
        config: super::super::LibraryOpenConfig {
            id: LibraryId::new_v4(),
            display_name: "Concurrency".into(),
            root: root.to_string_lossy().into_owned(),
            database_locator: root.join("library.sqlite").to_string_lossy().into_owned(),
            default_sync_server_url: url,
            metadata_server_url: "http://127.0.0.1:1".into(),
            storage: sync_common::storage::LibraryStorage::Bokheim,
            reset_cloud_presence: false,
        },
        account: account.library_account(),
        cloud: Default::default(),
        notification_interest: Default::default(),
        root_failures: None,
    })
    .unwrap();
    // Network fixtures use local peers/HTTP independently of device Wi-Fi.
    session.torrents.set_connectivity(tokio::sync::watch::channel(true).1);
    let (commands, inbox) = async_channel::unbounded();
    let handle = LibraryHandle { id: session.id, commands };
    let actor = LibraryActor::new(session);
    (actor, handle, inbox)
}

#[cfg(feature = "scanner")]
#[tokio::test]
async fn failed_scan_releases_busy_state_and_can_scan_again() {
    use library_model::LibraryUpdate;

    let root = tempfile::tempdir().unwrap();
    let (mut actor, _handle, _inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let updates = actor.coordination.subscribe_updates();
    let session = Rc::get_mut(&mut actor.session).unwrap();
    let scanner = session._scanner.clone();
    session._scanner = Arc::new(LibraryScanner { root: root.path().join("offline").to_string_lossy().into_owned() });

    assert!(actor.session.scan_folder().await.is_err());
    assert!(!actor.session.scanner_running(), "a failed scan must not block file replay");
    actor.coordination.drain(&actor.session.events);
    let mut observed = Vec::new();
    while let Ok(update) = updates.try_recv() {
        observed.push(update);
    }
    assert!(observed.contains(&LibraryUpdate::Scanning(true)));
    assert!(observed.contains(&LibraryUpdate::Scanning(false)));

    Rc::get_mut(&mut actor.session).unwrap()._scanner = scanner;
    actor.session.scan_folder().await.unwrap();
    assert!(!actor.session.scanner_running());
}

#[tokio::test]
async fn saving_a_reading_position_refreshes_the_library_before_delayed_sync() {
    use crate::library::commands::prepare_library_request;
    use library_model::{BrowseBookSort, BrowseChipSort, BrowseLocation, HomeShelf, LibraryBrowseQuery};
    use library_runtime::scheduler::SyncRequest;
    use sync_common::ROOT_DIR_ID;

    let root = tempfile::tempdir().unwrap();
    let (mut actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let hash = BookId::from(blake3::hash(b"reading-position wake"));
    actor
        .session
        .db
        .commit_import(library_database::ImportCommit {
            parent_id: ROOT_DIR_ID,
            file_name: "Book.epub".into(),
            format: book_model::BookFormat::Epub,
            content_hash: hash,
            size_bytes: 100,
            inspection_version: 1,
            inspection: Some(book_inspection::InspectedBook {
                pdf: None,
                audiobook: None,
                metadata: book_model::BookRecord { metadata: book_model::BookDetails { title: "Book".into(), ..Default::default() }, description: String::new() },
                toc: Vec::new(),
            }),
            published: library_model::PublishedStagedBook { name: "Book.epub".into(), relative_path: "/Book.epub".into(), published: false, fingerprint: None },
            request_thumbnail: false,
            restore_paths: Vec::new(),
        })
        .unwrap();
    let query = LibraryBrowseQuery {
        location: BrowseLocation::Home(Some(HomeShelf::RecentlyRead)),
        search: String::new(),
        file_types: Vec::new(),
        chip_sort: BrowseChipSort::DefaultOrder,
        book_sort: BrowseBookSort::DefaultOrder,
        languages: Vec::new(),
        hide_finished: false,
    };
    assert!(actor.session.db.browse_contents(&query, false).unwrap().books.is_empty());
    actor.coordination.drain(&actor.session.events);
    let asset_requests = actor.requests.as_ref().unwrap().assets.len();
    let file_requests = actor.requests.as_ref().unwrap().files.len();
    let requests = actor.requests.as_ref().unwrap().sync.clone();
    let updates = actor.coordination.subscribe_updates();
    assert!(requests.is_empty());
    prepare_library_request(&mut actor, LibraryCommand::Ready(ReadyCommand::UpdateReadingPosition { content_hash: hash, position: "epubcfi(/6/2)".into(), progress: Some(10.0), entry: None })).unwrap();
    let SyncRequest::WakeAfter(delay) = requests.try_recv().unwrap() else { panic!("position changes should schedule their own publication deadline") };
    assert!(delay <= Duration::from_secs(5) && delay > Duration::from_secs(4));
    assert_eq!(actor.session.db.reading_position(&hash).unwrap().as_deref(), Some("epubcfi(/6/2)"));
    actor.coordination.drain(&actor.session.events);
    assert_eq!(updates.try_recv().unwrap(), library_model::LibraryUpdate::Contents);
    let recent = actor.session.db.browse_contents(&query, false).unwrap().books;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].content_hash(), Some(hash));
    assert_eq!(recent[0].progress, 10.0);
    assert!(updates.is_empty());
    assert_eq!(actor.requests.as_ref().unwrap().assets.len(), asset_requests, "saving progress must not start asset work");
    assert_eq!(actor.requests.as_ref().unwrap().files.len(), file_requests, "saving progress must not start file replay");
    let mut outbox = actor.session.db.sync_prepare_outbox_excluding_positions(actor.session.events.reading_positions.borrow().waiting(web_time::Instant::now())).unwrap();
    assert!(actor.session.db.sync_next_outbox_page(&mut outbox).unwrap().iter().all(|p| p.state.value.kind() != "reading_position"));

    // Updates within a chapter must refresh too, without needing a completed chapter.
    prepare_library_request(&mut actor, LibraryCommand::Ready(ReadyCommand::UpdateReadingPosition { content_hash: hash, position: "epubcfi(/6/4)".into(), progress: Some(20.0), entry: None })).unwrap();
    actor.coordination.drain(&actor.session.events);
    assert_eq!(updates.try_recv().unwrap(), library_model::LibraryUpdate::Contents);
    assert_eq!(actor.session.db.browse_contents(&query, false).unwrap().books[0].progress, 20.0);
    prepare_library_request(&mut actor, LibraryCommand::Ready(ReadyCommand::UpdateReadingPosition { content_hash: hash, position: "epubcfi(/6/4)".into(), progress: Some(20.0), entry: None })).unwrap();
    actor.coordination.drain(&actor.session.events);
    assert!(updates.is_empty(), "an unchanged position should not reload the library");
}

#[tokio::test]
async fn signing_in_does_not_make_missing_local_only_books_browsable() {
    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    assert!(!actor.session.downloaded_books_only().unwrap(), "a connected cloud-backed library includes remote books");
    actor.session.db.set_asset_storage_policy(false, false).unwrap();
    assert!(actor.session.downloaded_books_only().unwrap(), "local-only libraries require local bytes even when signed in");
}

#[tokio::test]
async fn thumbnails_load_by_hash_without_book_records_or_current_card_variants() {
    use crate::ThumbnailResolution;
    use library_model::BlobKind;

    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let session = &actor.session;
    let hash = BookId::from(blake3::hash(b"existing book"));
    let master = b"existing cover";
    session.assets.write(BlobKind::Thumbnail, &hash, master).await.unwrap();
    for resolution in [ThumbnailResolution::Browse, ThumbnailResolution::CardHighDensity, ThumbnailResolution::HighDensity] {
        assert_eq!(session.thumbnail(hash, resolution).await.unwrap().as_deref(), Some(master.as_slice()));
    }
    assert_eq!(session.db.thumbnail_work_state(&hash).unwrap(), None);

    session.assets.write_thumbnail_set(&hash, master, b"browse card", b"dense card").await.unwrap();
    assert_eq!(session.thumbnail(hash, ThumbnailResolution::Browse).await.unwrap().as_deref(), Some(b"browse card".as_slice()));
    assert_eq!(session.thumbnail(hash, ThumbnailResolution::CardHighDensity).await.unwrap().as_deref(), Some(b"dense card".as_slice()));
    assert_eq!(session.thumbnail(hash, ThumbnailResolution::HighDensity).await.unwrap().as_deref(), Some(master.as_slice()));
}

#[tokio::test]
async fn provider_cover_cache_is_local_and_stale_revisions_are_hidden() {
    use book_model::{BookFormat, BookRecord};
    use library_replica::{BookAddition, BookFacts, EnrichmentResult, ExternalBook};
    use sync_common::{ROOT_DIR_ID, storage::{CloudLibraryRoot, CloudProvider, LibraryStorage, ProviderFile, ProviderFileIdentity, ProviderId, ProviderScope}};

    let root = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let library = &actor.session;
    let scope = ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() };
    library.db.set_storage_policy(&LibraryStorage::External(CloudLibraryRoot { scope: scope.clone(), folder_id: ProviderId::new("folder").unwrap() }), false).unwrap();
    let file = ProviderFile { identity: ProviderFileIdentity { scope, file_id: ProviderId::new("book").unwrap() }, revision: ProviderId::new("1").unwrap(), size_bytes: 100, content_hash: None };
    let addition = BookAddition { facts: BookFacts { added_at: 1, format: BookFormat::Epub }, record: BookRecord { metadata: Default::default(), description: String::new() }, navigation: None, pdf: None, recording: None, enrichment: Some(EnrichmentResult::NoMatch) };
    let mut value = ExternalBook { file: file.clone(), addition, thumbnails: None };
    let id = library.db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
    library.assets.write_thumbnail_set(&id, b"master", b"browse", b"dense").await.unwrap();
    assert!(library.thumbnail(id, crate::ThumbnailResolution::Browse).await.unwrap().is_none(), "unverified cache files must not be displayed");
    library.db.complete_provider_cover_cache(&file, None, true).unwrap();
    for (resolution, expected) in [(crate::ThumbnailResolution::Browse, b"browse".as_slice()), (crate::ThumbnailResolution::CardHighDensity, b"dense".as_slice()), (crate::ThumbnailResolution::HighDensity, b"master".as_slice())] {
        assert_eq!(library.thumbnail(id, resolution).await.unwrap().as_deref(), Some(expected));
    }
    value.file.revision = ProviderId::new("2").unwrap();
    library.db.admit_external_book(ROOT_DIR_ID, value.clone()).unwrap();
    assert!(library.thumbnail(id, crate::ThumbnailResolution::HighDensity).await.unwrap().is_none(), "a replaced book must not show its previous cover");
    library.db.complete_provider_cover_cache(&value.file, None, false).unwrap();
    assert!(library.thumbnail(id, crate::ThumbnailResolution::HighDensity).await.unwrap().is_none());
}

#[tokio::test]
async fn remote_notifications_route_to_the_requested_library_and_reconnect_refreshes_all() {
    use library_runtime::scheduler::SyncRequest;

    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let (mut first_actor, first, first_inbox) = fixture(first_root.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut second_actor, second, second_inbox) = fixture(second_root.path(), "http://127.0.0.1:1".parse().unwrap());
    let directory = Arc::new(LibraryDirectory::default());
    directory.insert(first.clone()).unwrap();
    directory.insert(second.clone()).unwrap();
    let notifications = LibraryNotifications::new(directory.clone(), first_actor.session.account.clone(), InterestState::default());

    notifications.receive(&NotificationMessage::LibraryChanged { library_id: first.id() }, true).unwrap();
    first_actor.handle_command(first_inbox.try_recv().unwrap());
    assert!(matches!(first_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
    assert!(second_inbox.is_empty(), "a library notification must not wake other libraries");
    assert!(second_actor.requests.as_ref().unwrap().sync.is_empty());

    let hello = NotificationMessage::Hello { heartbeat_seconds: 5, timeout_seconds: 15 };
    notifications.receive(&hello, false).unwrap();
    assert!(first_inbox.is_empty(), "reader-only connections must not reconcile on Hello");
    assert!(second_inbox.is_empty());
    for message in [hello, NotificationMessage::LibrariesChanged, NotificationMessage::Reconcile] {
        notifications.receive(&message, true).unwrap();
        first_actor.handle_command(first_inbox.try_recv().unwrap());
        second_actor.handle_command(second_inbox.try_recv().unwrap());
        assert!(matches!(first_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
        assert!(matches!(second_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
    }

    notifications.receive(&NotificationMessage::LibraryChanged { library_id: LibraryId::new_v4() }, true).unwrap();
    assert!(first_inbox.is_empty());
    assert!(second_inbox.is_empty());

    directory.retire(&first.id()).unwrap();
    notifications.receive(&NotificationMessage::LibraryChanged { library_id: first.id() }, true).unwrap();
    notifications.receive(&NotificationMessage::Reconcile, true).unwrap();
    assert!(directory.try_get(&first.id()).unwrap().is_none());
    assert!(first_actor.requests.as_ref().unwrap().sync.is_empty());
    second_actor.handle_command(second_inbox.try_recv().unwrap());
    assert!(matches!(second_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
}

#[tokio::test]
async fn foreground_resume_reconciles_libraries_without_a_socket_and_does_not_poll() {
    use library_runtime::scheduler::SyncRequest;

    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let (mut first_actor, first, first_inbox) = fixture(first_root.path(), "http://127.0.0.1:1".parse().unwrap());
    let (mut second_actor, second, second_inbox) = fixture(second_root.path(), "http://127.0.0.1:1".parse().unwrap());
    let account = first_actor.session.account.clone();
    let directory = Arc::new(LibraryDirectory::default());
    directory.insert(first).unwrap();
    directory.insert(second).unwrap();
    let interest = NotificationInterest::default();
    let activity = interest.subscribe();
    let mut notifications = LibraryNotifications::new(directory, account.clone(), *activity.borrow());
    let now = Instant::now();
    let view = interest.view(false);
    assert!(!activity.borrow().enabled(), "reader-only foreground activity needs no socket");
    assert!(!notifications.observe_activity_at(*activity.borrow(), now + Duration::from_secs(1)));
    assert!(first_inbox.is_empty());
    assert!(second_inbox.is_empty());

    // A deactivate/reactivate pair can coalesce into one activity update.
    drop(view);
    let view = interest.view(false);
    assert!(notifications.observe_activity_at(*activity.borrow(), now + Duration::from_secs(31)));
    first_actor.handle_command(first_inbox.try_recv().unwrap());
    second_actor.handle_command(second_inbox.try_recv().unwrap());
    assert!(matches!(first_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
    assert!(matches!(second_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
    assert!(!notifications.observe_activity_at(*activity.borrow(), now + Duration::from_secs(40)));

    drop(view);
    let view = interest.view(false);
    assert!(!notifications.observe_activity_at(*activity.borrow(), now + Duration::from_secs(45)), "rapid resumes are throttled");
    assert!(first_inbox.is_empty());
    assert!(second_inbox.is_empty());
    drop(view);
    let view = interest.view(false);
    assert!(notifications.observe_activity_at(*activity.borrow(), now + Duration::from_secs(65)));
    first_actor.handle_command(first_inbox.try_recv().unwrap());
    second_actor.handle_command(second_inbox.try_recv().unwrap());
    assert!(matches!(first_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));
    assert!(matches!(second_actor.requests.as_ref().unwrap().sync.try_recv(), Ok(SyncRequest::RefreshRemote)));

    let token = account.require().unwrap().token().to_owned();
    assert!(account.refresh_after_unauthorized(token).await.unwrap().is_none());
    drop(view);
    let _view = interest.view(false);
    assert!(!notifications.observe_activity_at(*activity.borrow(), now + Duration::from_secs(100)), "signed-out resumes do not trigger discovery or library refresh");
    assert!(first_inbox.is_empty());
    assert!(second_inbox.is_empty());
}

#[tokio::test]
async fn subscription_registers_with_initial_state_and_receives_callback_events() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, handle, inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let session = actor.session.clone();
    let _activity = session.directory_import_activity();
    let events = actor.session.events.clone();
    let hash = BookId::from_bytes([3; 32]);
    let exercise = async {
        let (updates, scanning) = handle.subscribe_updates().await.unwrap();
        assert!(scanning, "the subscription snapshot includes the active import");
        // Callback capabilities can cross threads without sharing actor state.
        std::thread::spawn(move || events.emit(LibraryEvent::ThumbnailGenerated { content_hash: hash })).join().unwrap();
        assert_eq!(updates.recv().await.unwrap(), library_model::LibraryUpdate::Covers(vec![hash]));
        handle.shutdown();
        assert!(updates.recv().await.is_err());
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(actor.serve(inbox), exercise);
    })
    .await
    .expect("subscription and event delivery must make progress");
}

#[tokio::test]
async fn retirement_stops_buffered_updates_without_polling_the_actor() {
    use crate::library::subscriptions::LibraryUpdates;
    let root = tempfile::tempdir().unwrap();
    let (mut actor, handle, _inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let (reply, result) = async_channel::bounded(1);
    actor.handle_command(LibraryActorCommand::SubscribeUpdates { reply });
    let (updates, _) = result.try_recv().unwrap().unwrap();
    let updates = LibraryUpdates::new(updates, handle.commands.clone());
    actor.session.emit(LibraryEvent::ContentsChanged);
    handle.shutdown();
    assert_eq!(updates.try_recv(), Err(async_channel::TryRecvError::Closed));
    assert!(updates.recv().await.is_err());
    assert!(handle.subscribe_updates().await.is_err());
}

#[tokio::test]
async fn retirement_cancels_a_subscription_still_waiting_in_the_inbox() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let root = tempfile::tempdir().unwrap();
    let (_actor, handle, inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let mut subscribing = std::pin::pin!(handle.subscribe_updates());
    assert!(matches!(subscribing.as_mut().poll(&mut Context::from_waker(Waker::noop())), Poll::Pending));
    assert_eq!(inbox.len(), 1);
    handle.shutdown();
    assert!(tokio::time::timeout(Duration::from_secs(1), subscribing).await.unwrap().is_err());
}

#[tokio::test]
async fn mutations_publish_queued_events_before_acknowledgement() {
    let root = tempfile::tempdir().unwrap();
    let (mut actor, _handle, _inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    let (reply, result) = async_channel::bounded(1);
    actor.handle_command(LibraryActorCommand::SubscribeUpdates { reply });
    let (updates, _) = result.try_recv().unwrap().unwrap();

    let (reply, result) = async_channel::bounded(1);
    actor.handle_command(LibraryActorCommand::Ui { command: LibraryCommand::Ready(ReadyCommand::CreateDirectory { parent_id: sync_common::ROOT_DIR_ID, name: "Shelf".into() }), reply });
    assert!(result.try_recv().unwrap().is_ok());
    assert_eq!(updates.try_recv().unwrap(), library_model::LibraryUpdate::Contents);

    let events = actor.session.events.clone();
    let (reply, result) = async_channel::bounded(1);
    actor.submit(OperationKind::Read, PendingReply::new(reply), async move {
        events.emit(LibraryEvent::ContentsChanged);
        Ok(())
    });
    let completion = actor.operations.next().await.unwrap();
    assert!(result.is_empty(), "workers return their result to the actor before replying");
    assert!(updates.is_empty(), "only the actor applies queued coordination events");
    actor.complete_operation(completion);
    assert!(result.try_recv().unwrap().is_ok());
    assert_eq!(updates.try_recv().unwrap(), library_model::LibraryUpdate::Contents);
}

#[tokio::test]
async fn folder_replay_follows_renames_while_the_actor_serves() {
    let root = tempfile::tempdir().unwrap();
    let (actor, handle, inbox) = fixture(root.path(), "http://127.0.0.1:1".parse().unwrap());
    actor.session.scanning.set(false);
    let folder = actor.session.placement.create_directory(&sync_common::ROOT_DIR_ID, "Before").unwrap();
    library_placement::prepare_directory(&actor.session.assets, &actor.session.db, folder.id).await.unwrap();
    assert!(root.path().join("Before").is_dir());
    let exercise = async {
        for name in ["After", "Again"] {
            handle.dispatch(LibraryCommand::Ready(ReadyCommand::RenameDirectory { directory_id: folder.id, name: name.into() })).await.unwrap();
            while !root.path().join(name).is_dir() {
                tokio::task::yield_now().await;
            }
        }
        assert!(!root.path().join("Before").exists());
        assert!(!root.path().join("After").exists());
        handle.shutdown();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(actor.run(inbox, async {}), exercise);
    })
    .await
    .expect("each rename reaches the disk");
}

#[tokio::test]
async fn browser_download_import_appears_on_home_before_enrichment() {
    use library_model::{BrowseLocation, HomeShelf, LibraryBrowseQuery, LibraryUpdate};
    use std::time::{SystemTime, UNIX_EPOCH};
    use sync_common::ROOT_DIR_ID;

    let library = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let source = downloads.path().join("download.epub");
    let bytes = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
    std::fs::write(&source, bytes).unwrap();
    let hash = BookId::from(blake3::hash(bytes));
    let (mut actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
    let updates = actor.coordination.subscribe_updates();
    let started = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap();

    let failures = actor.session.import_paths(ROOT_DIR_ID, vec![source.clone()], true).await.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert!(!source.exists());
    assert_eq!(std::fs::read(library.path().join("download.epub")).unwrap(), bytes);
    actor.coordination.drain(&actor.session.events);
    assert!(std::iter::from_fn(|| updates.try_recv().ok()).any(|update| update == LibraryUpdate::Contents));

    let mut query = LibraryBrowseQuery {
        location: BrowseLocation::Home(Some(HomeShelf::RecentlyAdded)),
        search: String::new(),
        file_types: Vec::new(),
        languages: Vec::new(),
        chip_sort: Default::default(),
        book_sort: Default::default(),
        hide_finished: false,
    };
    for downloaded_only in [false, true] {
        let page = actor.session.db.browse(&query, downloaded_only).unwrap();
        assert_eq!(page.contents.books.len(), 1);
        let book = &page.contents.books[0];
        assert_eq!(book.content_hash(), Some(hash));
        assert!(book.downloaded);
        assert!(book.added_at >= started);
    }
    query.location = BrowseLocation::Home(None);
    let home = actor.session.db.browse(&query, false).unwrap();
    let recent = home.sections.iter().find(|section| section.parent.id == BrowseLocation::Home(Some(HomeShelf::RecentlyAdded))).unwrap();
    assert_eq!(recent.books[0].content_hash(), Some(hash));
}

#[tokio::test]
async fn external_import_leaves_failed_imports_on_disk_for_retry() {
    for moving in [false, true] {
        let library = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("broken.epub");
        std::fs::write(&source, b"invalid EPUB").unwrap();
        let (actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
        let shelf = actor.session.placement.create_directory(&sync_common::ROOT_DIR_ID, "Shelf").unwrap();
        let failures = actor.session.import_paths(shelf.id, vec![source.clone()], moving).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(source.exists(), !moving);
        let moved = library.path().join("Shelf/broken.epub");
        assert_eq!(std::fs::read(&moved).unwrap(), b"invalid EPUB");
        let failures = actor.session.import_paths(shelf.id, vec![moved.clone()], true).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(std::fs::read(&moved).unwrap(), b"invalid EPUB");
        assert!(!library.path().join("Shelf/broken 2.epub").exists());
    }
}

#[tokio::test]
async fn external_import_places_nested_books_before_registration() {
    for moving in [false, true] {
        let library = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("Books");
        std::fs::create_dir_all(source.join("Nested")).unwrap();
        let bytes = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
        std::fs::write(source.join("Nested/book.epub"), bytes).unwrap();
        std::fs::write(source.join("notes.txt"), b"sidecar").unwrap();
        let (actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
        let shelf = actor.session.placement.create_directory(&sync_common::ROOT_DIR_ID, "Shelf").unwrap();
        let failures = actor.session.import_paths(shelf.id, vec![source.clone()], moving).await.unwrap();
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(source.exists(), !moving);
        assert_eq!(std::fs::read(library.path().join("Shelf/Books/Nested/book.epub")).unwrap(), bytes);
        assert_eq!(std::fs::read(library.path().join("Shelf/Books/notes.txt")).unwrap(), b"sidecar");
        let hash = BookId::from(blake3::hash(bytes));
        let mut paths: Vec<_> = actor.session.db.book_paths(&hash).unwrap().iter().map(|path| path.to_string()).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths, vec!["/Shelf/Books/Nested/book.epub"]);
        let failures = actor.session.import_paths(shelf.id, vec![library.path().join("Shelf/Books")], true).await.unwrap();
        assert!(failures.is_empty(), "{failures:?}");
        assert!(!library.path().join("Shelf/Books 2").exists());
    }
}

#[tokio::test]
async fn selected_import_copies_all_files_before_shared_inspection() {
    use crate::{DirectoryImport, DirectoryImportFile, DirectoryImportReader, ImportSource};
    use std::io::Cursor;

    struct InterruptedReader(bool);
    impl std::io::Read for InterruptedReader {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("copy interrupted"));
            }
            self.0 = true;
            let count = bytes.len().min(4);
            bytes[..count].copy_from_slice(&b"data"[..count]);
            Ok(count)
        }
    }

    let library = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
    let shelf = actor.session.placement.create_directory(&sync_common::ROOT_DIR_ID, "Shelf").unwrap();
    let bytes = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
    let destination = library.path().join("Shelf");
    let directory = DirectoryImport {
        name: "Selected books".into(),
        directories: vec![],
        files: vec![
            DirectoryImportFile { path: vec!["broken.epub".into()], source: ImportSource::new(Box::new(|| Box::pin(async { Ok(Box::new(Cursor::new(b"invalid EPUB")) as DirectoryImportReader) }))) },
            DirectoryImportFile {
                path: vec!["book.epub".into()],
                source: ImportSource::new(Box::new(move || {
                    Box::pin(async move {
                        assert!(!destination.join("broken.epub").exists(), "copy the whole selection before publication or inspection");
                        Ok(Box::new(Cursor::new(bytes)) as DirectoryImportReader)
                    })
                })),
            },
            DirectoryImportFile { path: vec!["interrupted.epub".into()], source: ImportSource::new(Box::new(|| Box::pin(async { Ok(Box::new(InterruptedReader(false)) as DirectoryImportReader) }))) },
        ],
    };
    let failures = actor.session.import_selected_directory(shelf.id, directory, false, &|| false).await.unwrap();
    assert_eq!(failures.len(), 2);
    assert_eq!(std::fs::read(library.path().join("Shelf/broken.epub")).unwrap(), b"invalid EPUB");
    assert_eq!(std::fs::read(library.path().join("Shelf/book.epub")).unwrap(), bytes);
    assert!(!library.path().join("Shelf/interrupted.epub").exists());
    let hash = BookId::from(blake3::hash(bytes));
    let paths = actor.session.db.book_paths(&hash).unwrap();
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|path| path.as_str() == "/Shelf/book.epub"));
    assert!(std::fs::read_dir(library.path().join("Shelf")).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().starts_with(".bokheim-copy-")));
}

#[tokio::test]
async fn selected_folder_import_survives_source_removal_and_library_reopen() {
    use crate::{DirectoryImport, DirectoryImportFile, DirectoryImportReader, ImportSource};
    use std::fs::{self, File};

    let library = tempfile::tempdir().unwrap();
    let provider = tempfile::tempdir().unwrap();
    let source = provider.path().join("book.epub");
    let bytes = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
    fs::write(&source, bytes).unwrap();
    let hash = BookId::from(blake3::hash(bytes));
    {
        let (actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
        let directory = DirectoryImport {
            name: "Selected books".into(),
            directories: vec![vec!["Nested".into()]],
            files: vec![DirectoryImportFile {
                path: vec!["Nested".into(), "book.epub".into()],
                source: ImportSource::new(Box::new(move || Box::pin(async move { Ok(Box::new(File::open(source).map_err(|error| error.to_string())?) as DirectoryImportReader) }))),
            }],
        };
        let failures = actor.session.import_selected_directory(sync_common::ROOT_DIR_ID, directory, false, &|| false).await.unwrap();
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(fs::read(provider.path().join("book.epub")).unwrap(), bytes, "importing must leave the selected source untouched");
    }
    // A completed picker import must need neither the provider nor its URI
    // grant after restart. Only the managed library is opened again.
    provider.close().unwrap();
    let (actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
    assert_eq!(fs::read(library.path().join("Nested/book.epub")).unwrap(), bytes);
    let paths = actor.session.db.book_paths(&hash).unwrap();
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|path| path.as_str() == "/Nested/book.epub"));
}

#[tokio::test]
async fn selected_folder_import_uses_shared_folder_collision_handling() {
    use crate::{DirectoryImport, DirectoryImportFile, DirectoryImportReader, ImportSource};
    use std::io::Cursor;

    let library = tempfile::tempdir().unwrap();
    let (actor, _, _) = fixture(library.path(), "http://127.0.0.1:1".parse().unwrap());
    let shelf = actor.session.placement.create_directory(&sync_common::ROOT_DIR_ID, "Shelf").unwrap();
    let existing = actor.session.placement.create_directory(&shelf.id, "Books").unwrap();
    let bytes = include_bytes!("../../crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
    let directory = DirectoryImport {
        name: "Books".into(),
        directories: vec![vec!["Nested".into()], vec!["Empty".into()]],
        files: vec![DirectoryImportFile { path: vec!["Nested".into(), "book.epub".into()], source: ImportSource::new(Box::new(move || Box::pin(async move { Ok(Box::new(Cursor::new(bytes)) as DirectoryImportReader) }))) }],
    };
    let failures = actor.session.import_selected_directory(shelf.id, directory, true, &|| false).await.unwrap();
    assert!(failures.is_empty(), "{failures:?}");
    assert!(library.path().join("Shelf/Books 2/Empty").is_dir());
    assert_eq!(std::fs::read(library.path().join("Shelf/Books 2/Nested/book.epub")).unwrap(), bytes);
    assert_eq!(actor.session.db.child_directory(&shelf.id, "Books").unwrap(), Some(existing.id));
    let hash = BookId::from(blake3::hash(bytes));
    let paths = actor.session.db.book_paths(&hash).unwrap();
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|path| path.as_str() == "/Shelf/Books 2/Nested/book.epub"));
}
