use super::*;
use book_model::{AnnotationAnchor, AnnotationState, AnnotationStyle, BookFormat};
use library_replica::{BookLifecycleState, ReadingPositionState};
use library_replica::{MutationBody, StateMutation};
use std::io::Write;
use std::path::Path;
use sync_common::{LibraryRevision, MutationRejection, MutationRejectionReason, ReplicaSeq, ServerMutation, SyncCursor};
use test_support::{fixture_content_hash, fixture_dir_id, fixture_library_id, fixture_replica_id};

const TEST_CONTENT_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEST_AUTH_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
use crate::BlobKind;
use asset_transfer::TransferJob;
use asset_transfer::should_download_thumbnail;
use library_database::CURSOR_RECOVERY_PENDING_KEY;
use library_replica::coalesce_mutations_for_push;
use library_replica::prepare_push;
use sync_common::SyncExchangeResponse;
use sync_common::{PullStateResponse, PushMutationsResponse};
use sync_transport::{PushResponseError, validate_pull_batch, validate_push_response};

mod accounts;
mod assets;
mod cancellation;
mod convergence;
mod metadata;
mod protocol;
mod recovery;

fn test_credentials(server_url: &ServerUrl) -> sync_transport::SyncCredentials {
    sync_transport::SyncCredentials::new(server_url.clone(), TEST_AUTH_TOKEN)
}

fn test_shared_credentials(server_url: &ServerUrl) -> library_runtime::account::SharedSyncCredentials {
    shared_credentials(test_credentials(server_url))
}

fn shared_credentials(credentials: sync_transport::SyncCredentials) -> library_runtime::account::SharedSyncCredentials {
    std::sync::Arc::new(std::sync::RwLock::new(Some(credentials)))
}

/// The transfer stack speaks replica values; fixtures speak session values.
/// Conversion is field-identical and test-only.
fn db_placement(placement: &crate::BookPlacement) -> library_database::BookPlacement {
    library_database::BookPlacement::new(placement.content_hash, library_database::RelativeBookPath::parse(placement.rel_path.as_str()).unwrap())
}

fn session_credentials(session: &account_client::Session) -> sync_transport::SyncCredentials {
    sync_transport::SyncCredentials::new(session.server_url().clone(), session.token())
}

fn test_content_hash() -> ContentHash {
    ContentHash::new(TEST_CONTENT_HASH)
}

fn test_epub(payload: &[u8]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let compressed = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    archive.start_file("mimetype", stored).unwrap();
    archive.write_all(b"application/epub+zip").unwrap();
    archive.start_file("META-INF/container.xml", compressed).unwrap();
    archive
        .write_all(br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#)
        .unwrap();
    archive.start_file("OEBPS/content.opf", compressed).unwrap();
    archive.write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#).unwrap();
    archive.start_file("OEBPS/chapter.xhtml", compressed).unwrap();
    archive.write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><pre>"#).unwrap();
    archive.write_all(payload).unwrap();
    archive.write_all(b"</pre></body></html>").unwrap();
    archive.finish().unwrap().into_inner()
}

fn local_seq(value: u64) -> ReplicaSeq {
    ReplicaSeq::new(value).unwrap()
}

fn millis(value: u64) -> sync_common::UnixMillis {
    value
}

fn pulled_change(server_sequence: u64, body: MutationBody) -> ServerMutation {
    ServerMutation {
        mutation: StateMutation { origin: None, mutation_id: sync_common::MutationId::new(), body, changed_at: millis(10), replica_seq: local_seq(server_sequence) }.to_wire().unwrap(),
        replica_id: test_support::fixture_replica_id("remote-device"),
        revision: LibraryRevision::new(server_sequence).unwrap(),
    }
}

fn deleted(content_hash: u64) -> MutationBody {
    MutationBody::BookLifecycle { content_hash: fixture_content_hash(content_hash), value: BookLifecycleState::Deleted { origin_folder_id: None } }
}

fn added(content_hash: u64, added_at: u64) -> MutationBody {
    MutationBody::BookLifecycle { content_hash: fixture_content_hash(content_hash), value: BookLifecycleState::Present }
}

fn cursor(revision: Option<LibraryRevision>) -> SyncCursor {
    SyncCursor { state_revision: revision, reading_revision: revision }
}

fn pull_response(mutations: Vec<ServerMutation>, next_revision: Option<LibraryRevision>, has_more: bool) -> PullStateResponse {
    let next_cursor = if has_more {
        let mut cursor = SyncCursor::default();
        for mutation in &mutations {
            if mutation.mutation.kind == sync_common::mutation_kind::READING_POSITION {
                cursor.reading_revision = Some(mutation.revision);
            } else {
                cursor.state_revision = Some(mutation.revision);
            }
        }
        cursor
    } else {
        cursor(next_revision)
    };
    PullStateResponse { book_creations: crate::sync::fixture_book_creations(&mutations), mutations, next_cursor, has_more }
}

fn binary_sync_response<T: serde::Serialize>(value: &T) -> axum::response::Response {
    use axum::response::IntoResponse;

    let encoded = sync_common::transport::encode(value).unwrap();
    let mut response = encoded.into_response();
    response.headers_mut().insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static(sync_common::transport::MEDIA_TYPE));
    response
}

fn decode_sync_exchange(headers: &axum::http::HeaderMap, body: &[u8]) -> sync_common::SyncExchangeRequest {
    assert!(headers.get(axum::http::header::CONTENT_ENCODING).is_none());
    sync_common::transport::decode(body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap()
}

/// A deferred mutation means "ask again later", not "this exchange
/// failed". Failing the whole synchronization strands everything the
/// exchange did accomplish -- notably asset transfers and the completion
/// checkpoint -- behind one mutation the server will not take yet, which
/// for a device whose clock is ahead never resolves on its own.
fn reading_change(sequence: u64, content_hash: u64, cfi: &str, changed_at: u64) -> StateMutation {
    StateMutation {
        origin: None,
        mutation_id: sync_common::MutationId::new(),
        body: MutationBody::ReadingPosition { content_hash: fixture_content_hash(content_hash), value: ReadingPositionState { location: book_model::ReadingPosition::parse(cfi).unwrap(), progress: 0.0 } },
        changed_at: millis(changed_at),
        replica_seq: local_seq(sequence),
    }
}

fn transmittable(changes: Vec<StateMutation>) -> sync_common::PushBatcher {
    let queued = prepare_push(changes).unwrap();
    assert!(queued.untransmittable().is_empty(), "this fixture holds nothing larger than a batch");
    queued
}

fn annotation_change(sequence: u64, text: String) -> StateMutation {
    StateMutation {
        origin: None,
        mutation_id: sync_common::MutationId::new(),
        body: MutationBody::Annotation {
            annotation_id: uuid::Uuid::new_v4().to_string(),
            value: AnnotationState {
                content_hash: fixture_content_hash(sequence),
                anchor: AnnotationAnchor::EpubCfi { cfi: "epubcfi(/6/2)".to_owned() },
                exact_text: text.clone(),
                style: AnnotationStyle::Highlight,
                color: "yellow".to_owned(),
                note: text,
                created_at: millis(1),
                modified_at: millis(1),
                deleted: false,
                toc_ordinal: None,
                progress: None,
            },
        },
        changed_at: millis(sequence),
        replica_seq: local_seq(sequence),
    }
}

/// Test handle pairing the session-owned database with its sync worker.
/// Method calls dereference to `LibrarySync`; storage travels explicitly.
struct TestManager {
    sync: LibrarySync,
    database: Database,
}

impl TestManager {
    /// Session hands the worker its library file, mirroring production.
    fn transfer_worker(&self) -> asset_transfer::transfer::TransferWorker {
        self.sync.transfer_worker()
    }

    fn request_download(&self, content_hash: ContentHash) -> crate::sync::error::SyncResult<()> {
        self.sync.request_download(content_hash)
    }

    fn request_downloads(&self, content_hashes: &[ContentHash]) -> crate::sync::error::SyncResult<()> {
        self.sync.request_downloads(content_hashes)
    }

    fn prepare_requested_download(&self, hash: &ContentHash) -> crate::sync::error::SyncResult<Option<asset_transfer::TransferJob>> {
        self.sync.prepare_requested_download(hash)
    }

    async fn synchronize_state(&self) -> crate::sync::error::SyncResult<sync_engine::SyncOutcome> {
        self.sync.synchronize_state().await
    }

    async fn sync_once(&self) -> crate::sync::error::SyncResult<()> {
        self.sync.sync_once().await
    }

    async fn reconcile_assets(&self, targeted: Vec<ContentHash>) -> crate::sync::error::SyncResult<()> {
        self.sync.reconcile_assets(targeted).await
    }

    fn queue_outbox_reading_change(&self, mutation: &StateMutation, sequence: u64) {
        // These fixtures exercise edits of books already present on the device.
        library_database::configure_fixture_connection(&self.database, |conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("UPDATE sync_metadata SET change_origin='remote'", [])?;
            tx.execute("INSERT OR IGNORE INTO book(content_hash) VALUES(?1)", [fixture_content_hash(sequence).as_str()])?;
            tx.execute("UPDATE sync_metadata SET change_origin='local'", [])?;
            tx.commit()
        }).unwrap();
        self.database.enqueue_outbox_reading_change(&mutation.mutation_id.to_string(), fixture_content_hash(sequence).as_str(), &sync_common::wire::encode(&mutation.body).unwrap(), i64::try_from(mutation.changed_at).unwrap()).unwrap();
    }
}

impl std::ops::Deref for TestManager {
    type Target = LibrarySync;
    fn deref(&self) -> &Self::Target {
        &self.sync
    }
}

impl std::ops::DerefMut for TestManager {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sync
    }
}

fn test_manager(root: &Path) -> TestManager {
    test_manager_with_replica(root, "client")
}

fn test_manager_with_replica(root: &Path, replica_id: &str) -> TestManager {
    let db_path = root.join(crate::APP_HIDDEN_LIBRARY_DB_PATH);
    let database = Database::open(&db_path).unwrap();
    database.initialize_library().unwrap();
    let sync = LibrarySync::new(LibrarySyncConfig {
        database_path: db_path.clone(),
        assets: crate::asset_store::open(root.to_str().unwrap()).unwrap().with_book_paths(move |hash| Database::book_paths_at(&db_path, hash).map_err(crate::asset_store::AssetStoreError::operation)),
        replica_id: fixture_replica_id(replica_id),
        server_url: ServerUrl::parse("http://127.0.0.1:9").unwrap(),
        library_id: fixture_library_id("library"),
        event_tx: None,
        transfer_queue: Arc::new(TransferQueue::default()),
        credentials: Arc::new(std::sync::RwLock::new(None)),
        account: None,
        notification_interest: Default::default(),
        cpu: crate::default_cpu_host(),
    });
    TestManager { sync, database }
}

// Asset scheduling refreshes cover revisions even when book bytes are local.
async fn empty_thumbnail_presence(body: axum::body::Bytes) -> axum::response::Response {
    let _: sync_common::api::assets::ThumbnailPresenceRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
    binary_sync_response(&sync_common::api::assets::ThumbnailPresenceResponse { present: Vec::new(), revisions: Vec::new() })
}

struct CoverPresenceServer(tokio::task::JoinHandle<()>);
impl Drop for CoverPresenceServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn serve_empty_cover_presence(manager: &mut TestManager) -> CoverPresenceServer {
    let app = axum::Router::new().route("/api/libraries/:library_id/thumbnails/presence", axum::routing::post(empty_thumbnail_presence));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    manager.server_url = ServerUrl::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    manager.credentials = test_shared_credentials(&manager.server_url);
    CoverPresenceServer(tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }))
}
