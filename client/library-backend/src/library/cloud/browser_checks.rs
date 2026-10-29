//! Feature-gated runtime checks executed in a real browser worker with OPFS.

use super::*;
use crate::integration::LibraryAccountSession;
use crate::library::{LibraryOpenConfig, LibraryRuntimeSpec};
use cloud_storage::{
    connections::{CloudConnections, ConnectionPersistence, GoogleConnection},
    oauth::{GoogleAccount, GoogleOAuth, GoogleOAuthConfig, GoogleTokens},
};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
use sync_common::storage::{CloudProvider, LibraryStorage, ProviderScope};

const EPUB: &[u8] = include_bytes!("../../../tests/fixtures/cloud-cover.epub");

async fn check_staging(assets: &library_files::AssetStore, cpu: &dyn cpu_host::CpuHost) -> Result<(), BackendError> {
    use library_model::RelativeBookPath;
    use std::io::Cursor;

    // A Read implementation may return fewer bytes or a transient interruption.
    // The CPU bridge must still fill its requested range before replying.
    struct FragmentedReader<R> {
        inner: R,
        interrupt: bool,
    }
    impl<R: Read> Read for FragmentedReader<R> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupt {
                self.interrupt = false;
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let length = output.len().min(31);
            self.inner.read(&mut output[..length])
        }
    }
    impl<R: Seek> Seek for FragmentedReader<R> {
        fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(from)
        }
    }

    let digest = blake3::hash(EPUB).into();
    let length = EPUB.len() as u64;
    let (_, reader) = assets.copy_import(RelativeBookPath::parse("staging.epub").map_err(BackendError::operation)?, Box::new(Cursor::new(EPUB)), Vec::new(), Default::default()).await.map_err(BackendError::operation)?;
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(EPUB));
    let file = web_sys::File::new_with_u8_array_sequence(&parts, "staging.epub").map_err(|error| BackendError::message(format!("{error:?}")))?;
    let selected = assets.stage_browser_file(&file).await.map_err(BackendError::operation)?;
    let transferred = assets.stage_transfer_chunks(length, Some(digest), |offset, count| async move { Ok(EPUB[offset as usize..offset as usize + count].to_vec()) }).await.map_err(BackendError::operation)?;
    let thumbnails = cpu.generate_thumbnail("epub".into(), cpu_host::as_cpu_reader(FragmentedReader { inner: reader.open_reader().map_err(BackendError::operation)?, interrupt: true })).await?;
    assert!(thumbnails.is_some(), "the shared CPU thumbnail dispatch must work through the browser host");
    for staged in [reader, selected, transferred] {
        assert_eq!(staged.content_hash().content_hash(), Some(digest));
        assert_eq!(staged.size_bytes(), length);
        let mut bytes = Vec::new();
        staged.open_reader().map_err(BackendError::operation)?.read_to_end(&mut bytes)?;
        assert_eq!(bytes, EPUB);
    }
    assert!(assets.copy_import(RelativeBookPath::parse("empty.epub").map_err(BackendError::operation)?, Box::new(Cursor::new(Vec::<u8>::new())), Vec::new(), Default::default()).await.is_err());
    assert!(assets.stage_transfer_chunks(length, Some(digest), |_, _| async { Ok(Vec::new()) }).await.is_err());
    assert!(assets.stage_transfer_chunks(length, Some(blake3::hash(b"wrong checksum").into()), |offset, count| async move { Ok(EPUB[offset as usize..offset as usize + count].to_vec()) }).await.is_err());
    assert!(!assets.exists(library_model::BlobKind::Book, &digest.into()).map_err(BackendError::operation)?, "staging and failed verification must not publish bytes");
    Ok(())
}

// Exercise selected-folder imports against the actual browser File and OPFS
// adapters, including failures that must leave later files importable.
async fn check_selected_directory(session: &LibrarySession) -> Result<(), BackendError> {
    use crate::{DirectoryImport, DirectoryImportFile};
    use library_import::ImportSource;
    use std::cell::Cell;

    fn file(path: &str, valid: bool) -> DirectoryImportFile {
        DirectoryImportFile {
            path: path.split('/').map(str::to_owned).collect(),
            source: ImportSource::from_export(Some(Box::new(move || {
                if !valid {
                    return wasm_bindgen::JsValue::UNDEFINED;
                }
                let parts = js_sys::Array::new();
                parts.push(&js_sys::Uint8Array::from(EPUB));
                web_sys::File::new_with_u8_array_sequence(&parts, "selected.epub").unwrap().into()
            }))),
        }
    }
    fn directory(files: Vec<DirectoryImportFile>) -> DirectoryImport {
        DirectoryImport { name: "Picked".into(), directories: vec![vec!["Shelf".into(), "Nested".into()], vec!["Missing".into(), "Child".into()], vec!["Shelf".into()], vec!["Shelf".into()]], files }
    }

    let selected = directory(vec![file("Shelf/Nested/book.epub", true), file("Shelf/bad.epub", false), file("Absent/book.epub", true), file("ignored.txt", false)]);
    let failures = session.import.import_selected_directory(ROOT_DIR_ID, selected, true, &|| false).await?;
    assert_eq!(failures.len(), 3, "folder, source, and missing-parent failures must be retained");
    let root = session.db.child_directory(&ROOT_DIR_ID, "Picked")?.unwrap();
    let shelf = session.db.child_directory(&root, "Shelf")?.unwrap();
    let nested = session.db.child_directory(&shelf, "Nested")?.unwrap();
    assert!(session.db.has_book_named(&nested, "book.epub")?, "an earlier failure cannot stop later files");
    assert!(!session.db.has_book_named(&shelf, "bad.epub")?);
    // Invalid sources for existing names must be skipped before opening them.
    let mut selected = directory(vec![file("Shelf/Nested/book.epub", false)]);
    selected.directories.retain(|path| path[0] == "Shelf");
    assert!(session.import.import_selected_directory(ROOT_DIR_ID, selected, true, &|| false).await?.is_empty());
    assert_eq!(session.db.child_directory(&ROOT_DIR_ID, "Picked")?, Some(root));

    let checks = Cell::new(0);
    let cancelled = || {
        checks.set(checks.get() + 1);
        checks.get() == 3
    };
    let selected = DirectoryImport { name: "Unused".into(), directories: Vec::new(), files: vec![file("02.epub", true), file("01.epub", true)] };
    let error = session.import.import_selected_directory(root, selected, false, &cancelled).await.unwrap_err();
    assert!(error.to_string().contains("directory import cancelled"));
    assert!(session.db.has_book_named(&root, "01.epub")?, "files must import in path order");
    assert!(!session.db.has_book_named(&root, "02.epub")?, "cancellation must stop before opening the next file");
    assert!(session.db.child_directory(&root, "Unused")?.is_none(), "importing contents must reuse the requested parent");

    let selected = DirectoryImport { name: "Cancelled".into(), directories: Vec::new(), files: Vec::new() };
    assert!(session.import.import_selected_directory(ROOT_DIR_ID, selected, true, &|| true).await.is_err());
    assert!(session.db.child_directory(&ROOT_DIR_ID, "Cancelled")?.is_none());
    Ok(())
}

struct BrowserLog;
impl log::Log for BrowserLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            web_sys::console::log_1(&format!("{}: {}", record.level(), record.args()).into());
        }
    }
    fn flush(&self) {}
}
static LOGGER: BrowserLog = BrowserLog;

#[derive(Clone, Debug)]
struct NoBokheimAccount;
impl account_client::SessionPersistence for NoBokheimAccount {
    fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
        Ok(None)
    }
    fn save_account_session(&self, _: &account_client::Session) -> std::io::Result<()> {
        panic!("provider storage cannot sign in to Bokheim")
    }
    fn clear_account_session(&self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Tokens;
impl ConnectionPersistence for Tokens {
    fn load_google(&self, _: &ProviderId) -> Result<Option<GoogleConnection>, CloudError> {
        Ok(Some(GoogleConnection {
            account: GoogleAccount { id: ProviderId::new("fixture-account").unwrap(), email: "reader@example.com".into(), name: None },
            tokens: GoogleTokens { access_token: "fixture-token".into(), refresh_token: None, expires_at: u64::MAX },
        }))
    }
    fn save_google(&self, _: &GoogleConnection, _: Option<&str>) -> Result<bool, CloudError> {
        panic!("fixture token must not refresh")
    }
}

pub async fn install_taxonomy(endpoint: &str, cpu: bool) -> Result<(), BackendError> {
    log::set_logger(&LOGGER).map_err(BackendError::operation)?;
    log::set_max_level(log::LevelFilter::Info);
    std::panic::set_hook(Box::new(|info| web_sys::console::error_1(&info.to_string().into())));
    if cpu {
        client_platform_web::sqlite::initialize_reference_storage().await.map_err(BackendError::message)?;
    } else {
        client_platform_web::web_storage::initialize().await.map_err(|error| BackendError::message(error.to_string()))?;
    }
    let snapshot = reqwest::get(format!("{endpoint}/taxonomy.sqlite3")).await.map_err(BackendError::operation)?.error_for_status().map_err(BackendError::operation)?.bytes().await.map_err(BackendError::operation)?;
    let mut db = rusqlite::Connection::open_in_memory()?;
    db.deserialize_read_exact("main", snapshot.as_ref(), snapshot.len(), false)?;
    let release: String = db.query_row("SELECT value FROM taxonomy_meta WHERE key='release_id'", [], |row| row.get(0))?;
    let image = subject_projection::installed::prepare_image(&snapshot, (release.parse().map_err(BackendError::operation)?, 1), "browser cloud fixture").map_err(BackendError::message)?;
    let path = std::path::Path::new("taxonomy-cloud-fixture.sqlite3");
    client_platform_web::sqlite::import_reference_database(path.to_str().unwrap(), &image).map_err(BackendError::message)?;
    subject_projection::installed::activate(path).map_err(BackendError::message)
}

pub async fn run(endpoint: &str, writable: bool) -> Result<String, BackendError> {
    let id = uuid::Uuid::new_v4();
    let scope = ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("fixture-account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() };
    let root = CloudLibraryRoot { scope: scope.clone(), folder_id: ProviderId::new("folder").unwrap() };
    let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: "fixture-client".into(), client_secret: None }).map_err(BackendError::operation)?;
    let cloud = CloudConnections::new(oauth, Arc::new(Tokens)).with_test_google_drive_endpoint(scope.clone(), endpoint.parse().map_err(BackendError::operation)?).map_err(BackendError::operation)?;
    let account = LibraryAccountSession::load(NoBokheimAccount, "http://127.0.0.1:1".parse().unwrap()).map_err(BackendError::operation)?;
    let config = LibraryOpenConfig {
        id,
        display_name: "Drive fixture".into(),
        root: id.to_string(),
        database_locator: format!("cloud-{id}.sqlite"),
        default_sync_server_url: "http://127.0.0.1:1".parse().unwrap(),
        metadata_server_url: "http://127.0.0.1:1".into(),
        storage: LibraryStorage::External(root.clone()),
        reset_cloud_presence: false,
    };
    let session = LibrarySession::open(LibraryRuntimeSpec { config: config.clone(), account: account.library_account(), cloud: cloud.clone(), notification_interest: Default::default() })?;
    check_staging(&session.assets, &*session.cpu).await?;
    session.run_cloud_pass().await?;
    assert_eq!(session.db.book_count()?, 1);
    let (book, file) = session.db.provider_remote_books()?.pop().unwrap();
    assert!(book.is_external());
    assert_eq!(session.db.book_format(&book)?, Some(BookFormat::Epub));
    assert_eq!(session.db.provider_cover_cached(&book)?, Some(true));
    assert!(session.assets.thumbnail_set_exists(&book).map_err(BackendError::operation)?);
    let pair = session.db.provider_thumbnails(&book)?;
    assert_eq!(pair.is_some(), writable);
    if let Some(pair) = &pair {
        assert_ne!(pair.full.identity.file_id, pair.browse.identity.file_id);
    }
    assert!(session.db.pending_cover_uploads(None, 10)?.is_empty());
    let connection = session.cloud.google_drive(scope).map_err(BackendError::operation)?;
    if writable {
        session.assets.remove(library_model::BlobKind::Thumbnail, &book).map_err(BackendError::operation)?;
        session.cloud_thumbnails(&connection, &root).await?;
        assert!(session.assets.thumbnail_set_exists(&book).map_err(BackendError::operation)?);
        assert_eq!(session.db.provider_thumbnails(&book)?, pair);
    }
    session.db.sync_request_book_downloads(&[book], TransferOrigin::UserInitiated)?;
    session.download_cloud_book(&connection, book).await?;
    assert_eq!(session.persistent_download_state(book)?, DownloadState::Downloaded);
    let mut pinned = session.assets.playback_reader(&book).await.map_err(BackendError::operation)?.unwrap();
    let mut bytes = Vec::new();
    pinned.read_to_end(&mut bytes)?;
    assert_eq!(bytes, EPUB);
    assert_eq!(pinned.content_hash().map_err(BackendError::message)?, Some(book));
    // Subscription ownership must pin capabilities, reject stale releases, and
    // close every remaining capability when the actor's resource table drops.
    use crate::{book::BookSource, library::book::browser::BookResources};
    use client_platform_web::range_reader::read_range;
    let resources = BookResources::default();
    let (opened, lease) = session.prepare_book_for_reader(book).await?;
    let BookSource::Local { capability: first, .. } = opened.source else { panic!("downloaded book must have a local capability") };
    let first_id = resources.next_id().map_err(BackendError::message)?;
    resources.insert(first_id, lease);
    assert_eq!(read_range(&first, 0, EPUB.len()).unwrap(), EPUB);
    drop(resources.take(first_id));
    assert!(read_range(&first, 0, 1).is_err());

    let (opened, lease) = session.prepare_book_for_reader(book).await?;
    let BookSource::Local { capability: second, .. } = opened.source else { panic!("downloaded book must have a local capability") };
    let second_id = resources.next_id().map_err(BackendError::message)?;
    assert_ne!(first_id, second_id);
    resources.insert(second_id, lease);
    assert!(resources.take(first_id).is_none());
    assert_eq!(read_range(&second, 0, EPUB.len()).unwrap(), EPUB);
    drop(resources);
    assert!(read_range(&second, 0, 1).is_err());

    // The browser fixture injects the actual DOMException at the OPFS flush
    // boundary. A failed replacement must retain the published generation.
    let mut full = session.assets.begin_provider_write(&file).await.map_err(BackendError::operation)?;
    full.write_all(EPUB).await.map_err(BackendError::operation)?;
    js_sys::Reflect::set(&js_sys::global(), &"__cloudFixtureStorageFault".into(), &"flush".into()).map_err(|_| BackendError::message("could not inject OPFS quota failure"))?;
    let error = match full.publish().await {
        Err(error) => BackendError::operation(error),
        Ok(_) => return Err(BackendError::message("OPFS quota injection did not fail the publication")),
    };
    assert_eq!(error.failure(), Some(client_runtime::Failure::StorageFull));
    assert_eq!(session.persistent_download_state(book)?, DownloadState::Downloaded);
    let mut retained = session.assets.playback_reader(&book).await.map_err(BackendError::operation)?.unwrap();
    let mut retained_bytes = Vec::new();
    retained.read_to_end(&mut retained_bytes)?;
    assert_eq!(retained_bytes, EPUB);
    drop(retained);

    let mut bad = file.clone();
    bad.content_hash = Some(blake3::hash(b"incorrect fixture digest").into());
    let mut writer = session.assets.begin_provider_write(&bad).await.map_err(BackendError::operation)?;
    writer.write_all(EPUB).await.map_err(BackendError::operation)?;
    assert!(writer.publish().await.is_err());
    let mut first = session.assets.begin_provider_write(&file).await.map_err(BackendError::operation)?;
    first.write_all(EPUB).await.map_err(BackendError::operation)?;
    let obsolete = first.publish().await.map_err(BackendError::operation)?;
    let mut second = session.assets.begin_provider_write(&file).await.map_err(BackendError::operation)?;
    second.write_all(EPUB).await.map_err(BackendError::operation)?;
    let _current = second.publish().await.map_err(BackendError::operation)?;
    obsolete.discard().map_err(BackendError::message)?;
    assert!(session.assets.playback_reader(&book).await.map_err(BackendError::operation)?.is_some());
    assert_eq!(session.remove_local_book_copy(book)?, DownloadState::NotDownloaded);
    assert!(session.assets.playback_reader(&book).await.map_err(BackendError::operation)?.is_none());
    pinned.seek(SeekFrom::Start(0))?;
    bytes.clear();
    pinned.read_to_end(&mut bytes)?;
    assert_eq!(bytes, EPUB, "an open reader must survive cache replacement and eviction");
    assert!(session.db.remote_asset_hashes(library_model::BlobKind::Book)?.contains(&book));
    session.db.sync_request_book_downloads(&[book], TransferOrigin::UserInitiated)?;
    session.run_cloud_pass().await?;
    assert_eq!(session.persistent_download_state(book)?, DownloadState::Downloaded);
    assert!(session.db.pending_book_uploads(0, 10)?.is_empty());
    session.remove_local_book_copy(book)?;
    session.db.update_reading_position(&book, "epubcfi(/6/2)", Some(42.0), None)?;
    let original = session.db.book_transfer_state(book)?;
    let copy_id = uuid::Uuid::new_v4();
    let copy = LibrarySession::open(LibraryRuntimeSpec {
        config: LibraryOpenConfig {
            id: copy_id,
            display_name: "Local copy".into(),
            root: copy_id.to_string(),
            database_locator: format!("cloud-copy-{copy_id}.sqlite"),
            default_sync_server_url: "http://127.0.0.1:1".parse().unwrap(),
            metadata_server_url: "http://127.0.0.1:1".into(),
            storage: LibraryStorage::Local,
            reset_cloud_presence: false,
        },
        account: account.library_account(),
        cloud: Default::default(),
        notification_interest: Default::default(),
    })?;
    js_sys::Reflect::set(&js_sys::global(), &"__cloudFixtureStorageFault".into(), &"flush".into()).map_err(|_| BackendError::message("could not inject copy quota failure"))?;
    let resource = session.export_transfer_book(book).await?;
    let error = copy.import_transfer_resource(ROOT_DIR_ID, "Book.epub".into(), resource).await.unwrap_err();
    assert_eq!(error.failure(), Some(client_runtime::Failure::StorageFull));
    assert_eq!(copy.db.book_count()?, 0);
    assert_eq!(session.db.book_count()?, 1);
    let resource = session.export_transfer_book(book).await?;
    let copied = copy.import_transfer_resource(ROOT_DIR_ID, "Book.epub".into(), resource).await?;
    assert_eq!(copied.content_hash, BookId::from(blake3::hash(EPUB)));
    assert!(!copied.content_hash.is_external());
    assert_eq!(copy.db.book_transfer_state(copied.content_hash)?.reading, original.clone().retarget(copied.content_hash)?.reading);
    assert!(session.assets.playback_reader(&book).await.map_err(BackendError::operation)?.is_none(), "copying cannot populate or change the source cache");
    assert_eq!(session.db.storage_policy()?, LibraryStorage::External(root.clone()));
    drop(pinned);
    Box::pin(check_uploads(session, &copy, copied.content_hash, original, config, cloud, &account, writable)).await?;
    Box::pin(check_selected_directory(&copy)).await?;
    Ok(serde_json::json!({"scanned":1,"provider_thumbnails":writable,"local_thumbnails":true,"downloaded":true,"local_reader":true,"quota_preserved_download":true,"checksum_rejected":true,"new_generation_preserved":true,"evicted_and_downloaded_again":true,"open_reader_preserved":true,"storage_copy_quota_retry":true,"browser_upload_retained":true,"browser_upload_recovered":writable,"browser_copy_to_drive":writable}).to_string())
}

async fn check_uploads(
    session: LibrarySession, copy: &LibrarySession, copied: BookId, original: library_database::transfer::BookTransferState, config: LibraryOpenConfig, cloud: CloudConnections, account: &LibraryAccountSession<NoBokheimAccount>,
    writable: bool,
) -> Result<(), BackendError> {
    let storage = config.storage.clone();
    // Exercise actual browser import staging, durable retry, and publication.
    js_sys::Reflect::set(&js_sys::global(), &"__cloudFixtureStorageFault".into(), &"flush".into()).unwrap();
    let error = Box::pin(session.import.import_downloaded_book(ROOT_DIR_ID, "Quota.epub", EPUB.to_vec())).await.unwrap_err();
    assert_eq!(error.failure(), Some(client_runtime::Failure::StorageFull));
    assert!(session.assets.retained_provider_uploads().map_err(BackendError::operation)?.is_empty());
    assert_eq!(session.db.pending_provider_upload_count()?, 0);
    let error = Box::pin(session.import.import_downloaded_book(ROOT_DIR_ID, "New Book.epub", EPUB.to_vec())).await.unwrap_err();
    assert_eq!(error.failure(), Some(if writable { client_runtime::Failure::CloudStorageFull } else { client_runtime::Failure::PermissionDenied }));
    assert_eq!(session.db.book_count()?, 1, "unconfirmed uploads cannot publish library metadata");
    assert_eq!(session.db.pending_provider_upload_count()?, 1);
    let retained = session.assets.retained_provider_uploads().map_err(BackendError::operation)?;
    assert_eq!(retained.len(), 1);
    let retry = session.import.run_provider_uploads().await.unwrap_err();
    assert_eq!(retry.failure(), Some(if writable { client_runtime::Failure::CloudStorageFull } else { client_runtime::Failure::PermissionDenied }));
    let pending_id = retained[0].0;
    let reserved = session.db.provider_upload(pending_id)?.unwrap().file_id.unwrap();
    if writable {
        // Same source is idempotent. The fixture commits the upload but loses
        // its final reply and subsequently expires the resumable session.
        let retry = Box::pin(session.import.import_downloaded_book(ROOT_DIR_ID, "New Book.epub", EPUB.to_vec())).await.unwrap_err();
        assert!(retry.to_string().contains("503"), "the same inspected book must reach the saved provider upload: {retry}");
        assert_eq!(session.db.pending_provider_upload_count()?, 1);
        assert_eq!(session.assets.retained_provider_uploads().map_err(BackendError::operation)?.len(), 1);
        assert_eq!(session.db.provider_upload(pending_id)?.unwrap().file_id, Some(reserved.clone()));
        assert_eq!(session.db.book_count()?, 1);
        drop(session);
        let reopened = LibrarySession::open(LibraryRuntimeSpec { config, account: account.library_account(), cloud, notification_interest: Default::default() })?;
        // Final cache promotion reuses the retained generation even when a
        // second book-sized OPFS allocation would fail.
        js_sys::Reflect::set(&js_sys::global(), &"__cloudFixtureStorageFault".into(), &"book_create".into()).unwrap();
        reopened.import.run_provider_uploads().await?;
        js_sys::Reflect::delete_property(&js_sys::global(), &"__cloudFixtureStorageFault".into()).unwrap();
        assert_eq!(reopened.db.book_count()?, 2);
        assert_eq!(reopened.db.pending_provider_upload_count()?, 0);
        assert!(reopened.assets.retained_provider_uploads().map_err(BackendError::operation)?.is_empty());
        let uploaded = reopened.db.provider_remote_books()?.into_iter().find(|(_, file)| file.identity.file_id == reserved).unwrap().0;
        assert!(uploaded.is_external());
        assert_eq!(reopened.persistent_download_state(uploaded)?, DownloadState::Downloaded);
        let mut reader = reopened.assets.playback_reader(&uploaded).await.map_err(BackendError::operation)?.unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        assert_eq!(bytes, EPUB);
        let resource = copy.export_transfer_book(copied).await?;
        let transferred = Box::pin(reopened.import_transfer_resource(ROOT_DIR_ID, "Transferred Book".into(), resource)).await?;
        assert!(transferred.content_hash.is_external());
        assert_eq!(reopened.persistent_download_state(transferred.content_hash)?, DownloadState::Downloaded);
        assert_eq!(reopened.db.book_transfer_state(transferred.content_hash)?.reading, original.retarget(transferred.content_hash)?.reading);
        assert_eq!(copy.db.book_count()?, 1, "copying to Drive keeps the source library");
        assert!(copy.assets.playback_reader(&copied).await.map_err(BackendError::operation)?.is_some());
        assert!(reopened.assets.retained_provider_uploads().map_err(BackendError::operation)?.is_empty());
        assert_eq!(reopened.db.storage_policy()?, storage);
    }
    Ok(())
}
