use crate::{AssetApplication, PostgresDatabase, PostgresSyncRepository};
use server_asset_store::{AssetError, FsAssetStore};
use std::{io::Cursor, sync::Arc};
use sync_common::*;

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn metadata_waits_for_the_audiobook_upload() {
    let fixture = AudiobookFixture::new().await;
    let request = fixture.book();

    let response = fixture.exchange(&request).await;

    assert_deferred(&response, request.mutations.len());
    assert!(response.pull.mutations.is_empty());
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn an_old_reference_is_not_an_upload_receipt() {
    let fixture = AudiobookFixture::new().await;
    sqlx::query("INSERT INTO user_blob_reference(user_id,content_hash,reference_count) VALUES($1,$2,1)")
        .bind(&fixture.user).bind(fixture.identity.as_str()).execute(&fixture.pool).await.unwrap();

    let response = fixture.exchange(&fixture.book()).await;

    assert_deferred(&response, 4);
    assert!(response.pull.mutations.is_empty());
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn interrupted_upload_does_not_publish_metadata() {
    let fixture = AudiobookFixture::new().await;
    let truncated = fixture.audio[..fixture.audio.len() / 2].to_vec();

    assert!(fixture.upload(truncated).await.is_err());
    let response = fixture.exchange(&fixture.book()).await;

    assert_deferred(&response, 4);
    assert!(response.pull.mutations.is_empty());
    assert_eq!(fixture.stored_blob_count().await, 0);
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn completed_upload_releases_the_same_pending_mutations() {
    let fixture = AudiobookFixture::new().await;
    let request = fixture.book();
    assert_deferred(&fixture.exchange(&request).await, 4);

    fixture.upload(fixture.audio.clone()).await.unwrap();
    let published = fixture.exchange(&request).await;

    assert!(published.push.rejected.is_empty());
    assert_eq!(published.push.accepted.len(), 4);
    assert_eq!(published.pull.mutations.len(), 4);
    let retried = fixture.exchange(&request).await;
    assert_eq!(retried.push.accepted, published.push.accepted);
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn reading_position_requires_a_book_and_creation_requires_upload() {
    let fixture = AudiobookFixture::new().await;
    let alone = fixture.request(&[mutation_kind::READING_POSITION]);
    let ignored = fixture.exchange(&alone).await;
    assert_eq!(ignored.push.accepted, vec![alone.mutations[0].mutation_id]);
    assert!(ignored.pull.mutations.is_empty());
    let request = fixture.request(&[mutation_kind::READING_POSITION, mutation_kind::BOOK_LIFECYCLE]);
    assert_deferred(&fixture.exchange(&request).await, 2);
    fixture.upload(fixture.audio.clone()).await.unwrap();
    let accepted = fixture.exchange(&request).await;
    assert!(accepted.push.rejected.is_empty());
    assert_eq!(accepted.pull.mutations.len(), 2);
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn another_accounts_upload_does_not_authorize_book() {
    let mut fixture = AudiobookFixture::new().await;
    fixture.upload(fixture.audio.clone()).await.unwrap();
    (fixture.user, fixture.library) = create_account(&fixture.pool).await;

    let response = fixture.exchange(&fixture.book()).await;

    assert_deferred(&response, 4);
    assert!(response.pull.mutations.is_empty());
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn missing_upload_does_not_block_an_uploaded_book_in_the_same_batch() {
    let fixture = AudiobookFixture::new().await;
    fixture.upload(fixture.audio.clone()).await.unwrap();
    let mut request = fixture.request(&[mutation_kind::BOOK_LIFECYCLE, mutation_kind::METADATA, mutation_kind::BOOK_LIFECYCLE, mutation_kind::DESCRIPTION]);
    request.mutations[2].entity_key = "a".repeat(64);
    request.mutations[2].blob_reference = Some(sync_common::DeclaredBlobReference { present: true, content_hash: Some(ContentHash::new(&"a".repeat(64))) });
    request.mutations[3].entity_key = "a".repeat(64);
    let response = fixture.exchange(&request).await;
    assert_eq!(response.push.accepted, vec![request.mutations[0].mutation_id, request.mutations[1].mutation_id]);
    assert_eq!(response.push.rejected.len(), 2);
    for index in [2, 3] {
        assert!(response.push.rejected.contains(&MutationRejection { mutation_id: request.mutations[index].mutation_id, reason: MutationRejectionReason::MissingDependency }));
    }
    assert_eq!(response.pull.mutations.len(), 2);
}

fn assert_deferred(response: &SyncExchangeResponse, count: usize) {
    assert!(response.push.accepted.is_empty());
    assert_eq!(response.push.rejected.len(), count);
    for rejection in &response.push.rejected {
        assert_eq!(rejection.reason, MutationRejectionReason::MissingDependency);
    }
}

struct AudiobookFixture {
    pool: sqlx::PgPool,
    repository: PostgresSyncRepository,
    assets: AssetApplication,
    user: String,
    library: LibraryId,
    identity: ContentHash,
    checksum: ContentHash,
    audio: Vec<u8>,
    _disk: tempfile::TempDir,
}

impl AudiobookFixture {
    async fn new() -> Self {
        let pool = crate::test_support::isolated_pool("upload_book", 4)
            .await.expect("test database required");
        let database = PostgresDatabase::from_pool(pool.clone());
        let disk = tempfile::tempdir().unwrap();
        let source = disk.path().join("audio.m4b");
        std::fs::write(&source, include_bytes!("../../../../../client/app/tests/fixtures/embedded-cover.m4b")).unwrap();
        let identity = book_identity::ensure(&source).unwrap();
        let audio = std::fs::read(source).unwrap();
        let checksum = ContentHash::new(blake3::hash(&audio).to_hex().as_str());
        assert_ne!(identity, checksum, "exercise book identity separately from stored bytes");

        let store = Arc::new(FsAssetStore::new(disk.path().join("books"), disk.path().join("thumbnails")));
        store.initialize().await.unwrap();
        let assets = AssetApplication::new(database.clone(), store);
        let repository = PostgresSyncRepository::new(database);
        let (user, library) = create_account(&pool).await;
        Self { pool, repository, assets, user, library, identity, checksum, audio, _disk: disk }
    }

    fn book(&self) -> SyncExchangeRequest {
        self.request(&[
            mutation_kind::BOOK_LIFECYCLE,
            mutation_kind::METADATA,
            mutation_kind::PLACEMENT,
            mutation_kind::DESCRIPTION,
        ])
    }

    fn request(&self, kinds: &[&str]) -> SyncExchangeRequest {
        let mutations = kinds.iter().enumerate().map(|(index, kind)| {
            let sequence = index as u64 + 1;
            WireMutation {
                origin: None,
                mutation_id: MutationId::new(),
                kind: (*kind).into(),
                entity_key: self.identity.to_string(),
                entity_subkey: String::new(),
                value: Vec::new(),
                conflict_rank: 0,
                changed_at: 1000 + sequence,
                replica_seq: ReplicaSeq::new(sequence).unwrap(),
                // Match older clients, which only declare lifecycle references.
                blob_reference: (*kind == mutation_kind::BOOK_LIFECYCLE).then_some(DeclaredBlobReference {
                    present: true,
                    content_hash: Some(self.identity),
                }),
            }
        }).collect();
        SyncExchangeRequest {
            library_id: self.library,
            replica_id: ReplicaId::new_v4(),
            cursor: SyncCursor::default(),
            mutations,
        }
    }

    async fn exchange(&self, request: &SyncExchangeRequest) -> SyncExchangeResponse {
        self.repository.exchange(&self.user, request).await.unwrap()
    }

    async fn upload(&self, bytes: Vec<u8>) -> Result<(), AssetError> {
        self.assets.put_book_revision(
            &self.user, &self.library, &self.identity, &self.checksum,
            self.audio.len() as u64, Box::new(Cursor::new(bytes)),
        ).await
    }

    async fn stored_blob_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM blob_object").fetch_one(&self.pool).await.unwrap()
    }
}

async fn create_account(pool: &sqlx::PgPool) -> (String, LibraryId) {
    let user = uuid::Uuid::new_v4().to_string();
    let library = LibraryId::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1)")
        .bind(&user).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO libraries(id,user_id,name) VALUES($1,$2,'Audio')")
        .bind(library.to_string()).bind(&user).execute(pool).await.unwrap();
    (user, library)
}
