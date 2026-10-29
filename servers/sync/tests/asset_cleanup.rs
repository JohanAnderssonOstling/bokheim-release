use server_account::{AccountTokenKey, PostgresAccountService};
use server_asset_store::{AssetKind, FsAssetStore};
use server_postgres::{AssetApplication, PostgresDatabase, PostgresSyncRepository};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{io::Cursor, sync::Arc, time::Duration};
use sync_common::api::libraries::LibraryNameRequest;
use sync_common::{BookId, ConflictRank, LibraryId, OpaquePublication, OpaqueStateEntry, ReplicaId, ReplicaSeq, StateAddress, SyncCursor, SyncExchangeRequest, VersionKey, Versioned};
use tempfile::TempDir;
use tokio::{io::AsyncWriteExt, time::timeout};

const PASSWORD: &str = "asset-cleanup-test-password";

struct Fixture {
    database: PostgresDatabase,
    admin: PgPool,
    schema: String,
    root: TempDir,
    accounts: Arc<PostgresAccountService>,
    sync: PostgresSyncRepository,
    assets: Arc<AssetApplication>,
    store: Arc<FsAssetStore>,
    alice: String,
    bob: String,
    alice_library: LibraryId,
    bob_library: LibraryId,
    hash: BookId,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("SYNC_E2E_DATABASE_URL").unwrap();
        let admin = PgPool::connect(&url).await.unwrap();
        let schema = format!("asset_cleanup_{}", LibraryId::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await.unwrap();
        let search_path = format!("SET search_path TO {schema}");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let statement = search_path.clone();
                Box::pin(async move { sqlx::query(&statement).execute(connection).await.map(|_| ()) })
            })
            .connect(&url)
            .await
            .unwrap();
        let database = PostgresDatabase::from_pool(pool);
        server_postgres::migrate(&database).await.unwrap();
        let accounts = Arc::new(PostgresAccountService::new(database.clone(), AccountTokenKey::from_base64("WlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlo=").unwrap()));
        let alice = accounts.ensure_configured_user("alice@example.com", PASSWORD).await.unwrap().user_id;
        let bob = accounts.ensure_configured_user("bob@example.com", PASSWORD).await.unwrap().user_id;
        let sync = PostgresSyncRepository::new(database.clone());
        let alice_library = LibraryId::new_v4();
        let bob_library = LibraryId::new_v4();
        for (user, library) in [(&alice, alice_library), (&bob, bob_library)] {
            sync.create_library(user, &LibraryNameRequest { library_id: library, library_name: "Library".into(), storage: Default::default() }).await.unwrap();
        }
        let root = TempDir::new().unwrap();
        let store = Arc::new(FsAssetStore::new(root.path().join("books"), root.path().join("thumbnails")));
        store.initialize().await.unwrap();
        let assets = Arc::new(AssetApplication::new(database.clone(), store.clone()));
        let hash = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85".parse().unwrap();
        Self { database, admin, schema, root, accounts, sync, assets, store, alice, bob, alice_library, bob_library, hash }
    }

    async fn lifecycle(&self, user: &str, library: LibraryId, sequence: u64, rank: ConflictRank) {
        let replica = ReplicaId::new_v4();
        let seq = ReplicaSeq::new(sequence).unwrap();
        let response = self
            .sync
            .exchange(
                user,
                &SyncExchangeRequest {
                    library_id: library,
                    replica_id: replica,
                    cursor: SyncCursor::default(),
                    mutations: vec![OpaquePublication {
                        publisher_seq: seq,
                        state: Versioned {
                            value: OpaqueStateEntry { address: StateAddress { kind: "book_lifecycle".into(), entity_key: self.hash.into(), entity_subkey: String::new() }, book: Some(self.hash), bytes: vec![0] },
                            version: VersionKey::new(sequence, rank, replica, seq),
                        },
                    }],
                },
            )
            .await
            .unwrap();
        assert!(response.push.rejected.is_empty());
    }

    async fn upload(&self, user: &str, library: LibraryId) {
        self.assets.put_book(user, &library, &self.hash, 3, Box::new(Cursor::new(b"abc"))).await.unwrap();
    }

    async fn finish(self) {
        self.database.pool().close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema)).execute(&self.admin).await.unwrap();
        self.admin.close().await;
    }
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn cleanup_keeps_shared_and_trashed_books_and_deletes_the_last_owners_files() {
    let f = Fixture::new().await;
    let second_library = LibraryId::new_v4();
    f.sync.create_library(&f.alice, &LibraryNameRequest { library_id: second_library, library_name: "Second".into(), storage: Default::default() }).await.unwrap();
    for (user, library) in [(&f.alice, f.alice_library), (&f.alice, second_library), (&f.bob, f.bob_library)] {
        f.lifecycle(user, library, 1, ConflictRank::Update).await;
        f.upload(user, library).await;
        f.assets.put_thumbnail_pair(user, &library, &f.hash, b"cover", b"browse").await.unwrap();
    }
    assert_eq!(f.store.book_hashes().await.unwrap(), vec![f.hash], "one physical copy across both users and all libraries");
    assert_eq!(f.assets.collect_unused_assets().await.unwrap().books, 0);
    f.sync.delete_library(&f.alice, &f.alice_library).await.unwrap();
    f.assets.collect_unused_assets().await.unwrap();
    assert!(f.store.thumbnail_pair(&f.alice, &f.hash).await.unwrap().is_some(), "another library still needs this account's cover");

    f.lifecycle(&f.alice, second_library, 2, ConflictRank::Delete).await;
    f.assets.collect_unused_assets().await.unwrap();
    assert!(f.store.size(&f.alice, AssetKind::Book, &f.hash).await.unwrap().is_some(), "trash is restorable");
    let signal = f.database.asset_cleanup_signal();
    signal.notify_one();
    signal.notified().await;
    f.lifecycle(&f.alice, second_library, 3, ConflictRank::Purge).await;
    timeout(Duration::from_secs(1), signal.notified()).await.expect("permanent book deletion wakes cleanup");
    f.assets.collect_unused_assets().await.unwrap();
    assert!(f.store.thumbnail_pair(&f.alice, &f.hash).await.unwrap().is_none());
    assert!(f.assets.open_from(&f.bob, &f.bob_library, AssetKind::Book, &f.hash, 0).await.unwrap().is_some(), "Bob's copy survives Alice's purge");

    // Exercise the real account deletion and event-driven worker, not just a
    // direct collector call. No minute-long timer is needed for deletion.
    let signal = f.database.asset_cleanup_signal();
    signal.notify_one();
    signal.notified().await;
    f.accounts.delete_account("bob@example.com", PASSWORD).await.unwrap();
    timeout(Duration::from_secs(1), signal.notified()).await.expect("account deletion wakes cleanup");
    f.assets.spawn_cleanup_worker();
    timeout(Duration::from_secs(5), async {
        loop {
            let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blob_object").fetch_one(f.database.pool()).await.unwrap();
            if recorded == 0 && f.store.size(&f.bob, AssetKind::Book, &f.hash).await.unwrap().is_none() && f.store.thumbnail_pair(&f.bob, &f.hash).await.unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blob_object").fetch_one(f.database.pool()).await.unwrap();
    assert_eq!(recorded, 0);
    f.finish().await;
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn cleanup_and_a_slow_upload_do_not_lose_a_new_owners_file() {
    let f = Fixture::new().await;
    f.lifecycle(&f.alice, f.alice_library, 1, ConflictRank::Update).await;
    f.upload(&f.alice, f.alice_library).await;
    let (mut writer, reader) = tokio::io::duplex(8);
    let assets = f.assets.clone();
    let bob = f.bob.clone();
    let library = f.bob_library;
    let hash = f.hash;
    let upload = tokio::spawn(async move { assets.put_book(&bob, &library, &hash, 3, Box::new(reader)).await });
    writer.write_all(b"a").await.unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            let reserved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_upload_reservation WHERE user_id=$1").bind(&f.bob).fetch_one(f.database.pool()).await.unwrap();
            if reserved == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.accounts.delete_account("alice@example.com", PASSWORD).await.unwrap();
    timeout(Duration::from_secs(2), f.assets.collect_unused_assets()).await.expect("cleanup must not wait for the network upload").unwrap();
    assert!(f.store.size(&f.alice, AssetKind::Book, &f.hash).await.unwrap().is_none());
    writer.write_all(b"bc").await.unwrap();
    drop(writer);
    let (collected, uploaded) = tokio::join!(f.assets.collect_unused_assets(), upload);
    collected.unwrap();
    uploaded.unwrap().unwrap();
    f.assets.collect_unused_assets().await.unwrap();
    assert!(f.assets.open_from(&f.bob, &f.bob_library, AssetKind::Book, &f.hash, 0).await.unwrap().is_some());

    // A failed/rolled-back upload can leave bytes without a blob_object row.
    // A recovery sweep finds these from disk, while ignoring active temp files.
    f.accounts.delete_account("bob@example.com", PASSWORD).await.unwrap();
    sqlx::query("DELETE FROM blob_object").execute(f.database.pool()).await.unwrap();
    let temporary = f.root.path().join("books").join("64").join("active.tmp-upload");
    std::fs::write(&temporary, b"partial").unwrap();
    f.assets.collect_unused_assets().await.unwrap();
    assert!(f.store.size("", AssetKind::Book, &f.hash).await.unwrap().is_none());
    assert!(temporary.exists());
    f.finish().await;
}

#[tokio::test]
#[ignore = "requires SYNC_E2E_DATABASE_URL"]
async fn an_upload_cannot_publish_after_its_account_is_deleted() {
    let f = Fixture::new().await;
    let (mut writer, reader) = tokio::io::duplex(8);
    let assets = f.assets.clone();
    let user = f.alice.clone();
    let library = f.alice_library;
    let hash = f.hash;
    let upload = tokio::spawn(async move { assets.put_book(&user, &library, &hash, 3, Box::new(reader)).await });
    writer.write_all(b"a").await.unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            let reserved: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_upload_reservation WHERE user_id=$1").bind(&f.alice).fetch_one(f.database.pool()).await.unwrap();
            if reserved == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.accounts.delete_account("alice@example.com", PASSWORD).await.unwrap();
    f.assets.collect_unused_assets().await.unwrap();
    writer.write_all(b"bc").await.unwrap();
    drop(writer);
    assert!(upload.await.unwrap().is_err());
    assert!(f.store.size("", AssetKind::Book, &f.hash).await.unwrap().is_none());
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM book_upload_reservation").fetch_one(f.database.pool()).await.unwrap();
    assert_eq!(pending, 0);
    let shard = f.root.path().join("books").join("64");
    assert_eq!(std::fs::read_dir(shard).unwrap().count(), 0, "failed authorization removes the staged bytes too");
    f.finish().await;
}
