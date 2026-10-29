#![cfg(not(target_arch = "wasm32"))]

use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use book_model::{BookDetails, BookFormat, BookRecord, Contributor, Identifier, Scheme, Scope, AUTHOR_MARC_RELATOR_CODE};
use image::{DynamicImage, ImageFormat, RgbImage};
use library_database::{Database, ImportCommit};
use library_files::asset_store::AssetStore;
use library_model::{BlobKind, PublishedStagedBook};
use library_runtime::{events::LibraryEventRouter, scheduler::SyncScheduler};
use std::{
    collections::{BTreeMap, HashSet},
    io::Cursor,
    sync::{Arc, Mutex},
};
use sync_common::{BookId, ROOT_DIR_ID};

use super::{isbn_key, isbns};
use crate::{CoverClient, LibraryEnrichment, MetadataClient};

struct Provider {
    origin: String,
    requests: Mutex<Vec<String>>,
    isbn_status: StatusCode,
    isbn_image: Vec<u8>,
    search_docs: Mutex<Vec<serde_json::Value>>,
    fallback_image: Vec<u8>,
}

fn artwork(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, image::Rgb(color))).write_to(&mut bytes, ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn record() -> BookRecord {
    let mut record = BookRecord { metadata: BookDetails { title: "Utilitarianism".into(), ..Default::default() }, description: String::new() };
    record.contributors.push(Contributor::new("John Stuart Mill", AUTHOR_MARC_RELATOR_CODE).unwrap());
    record.book.identifiers.push(Identifier::new("9780306406157", Scheme::Isbn, Scope::Book).unwrap());
    record
}

async fn serve(State(provider): State<Arc<Provider>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    provider.requests.lock().unwrap().push(request.uri().to_string());
    match path.as_str() {
        "/metadata/v3/books" => {
            let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
            let request: metadata_contract::books::BookEnrichmentRequest = serde_json::from_slice(&bytes).unwrap();
            assert!(!request.books[0].needs_cover, "the metadata server must not fetch artwork");
            axum::Json(serde_json::json!({"policy":"books_v5","results":[{"key":request.books[0].key,"status":"no_match","record":null,"cover":null}]})).into_response()
        }
        "/ol/b/isbn/9780306406157-L.jpg" => {
            if !provider.isbn_status.is_success() {
                return provider.isbn_status.into_response();
            }
            ([("content-type", "image/png")], provider.isbn_image.clone()).into_response()
        }
        "/ol/search.json" => axum::Json(serde_json::json!({"docs":*provider.search_docs.lock().unwrap()})).into_response(),
        "/ol/b/id/42-L.jpg" => ([("content-type", "image/png")], provider.fallback_image.clone()).into_response(),
        _ => Response::builder().status(404).body(Body::empty()).unwrap(),
    }
}

async fn provider(status: StatusCode, width: u32) -> (CoverClient, Arc<Provider>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let provider = Arc::new(Provider {
        origin: origin.clone(),
        requests: Default::default(),
        isbn_status: status,
        isbn_image: artwork(width, width * 3 / 2, [240, 100, 0]),
        search_docs: Mutex::new(vec![serde_json::json!({"title":"Utilitarianism","author_name":["John Stuart Mill"],"cover_i":42})]),
        fallback_image: artwork(300, 450, [0, 100, 240]),
    });
    let app = Router::new().fallback(any(serve)).with_state(provider.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut client = CoverClient::default();
    client.openlibrary_origin = format!("{origin}/ol/").parse().unwrap();
    client.openlibrary_covers = client.openlibrary_origin.clone();
    client.interval_ms = 0;
    (client, provider, task)
}

#[tokio::test]
async fn openlibrary_isbn_artwork_is_downloaded_directly() {
    let (client, provider, task) = provider(StatusCode::OK, 600).await;
    assert_eq!(client.book_cover(&record()).await.unwrap().unwrap(), provider.isbn_image);
    assert_eq!(*provider.requests.lock().unwrap(), ["/ol/b/isbn/9780306406157-L.jpg?default=false"]);
    task.abort();
}

#[tokio::test]
async fn missing_isbn_artwork_falls_back_to_title_and_author_search() {
    let (client, provider, task) = provider(StatusCode::NOT_FOUND, 600).await;
    assert_eq!(client.book_cover(&record()).await.unwrap().unwrap(), provider.fallback_image);
    let paths = provider.requests.lock().unwrap().clone();
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[0], "/ol/b/isbn/9780306406157-L.jpg?default=false");
    let search = url::Url::parse(&format!("{}{}", provider.origin, paths[1])).unwrap();
    assert_eq!(search.path(), "/ol/search.json");
    let params = search.query_pairs().collect::<BTreeMap<_, _>>();
    assert_eq!(params.get("title").unwrap(), "Utilitarianism");
    assert_eq!(params.get("author").unwrap(), "John Stuart Mill");
    assert_eq!(paths[2], "/ol/b/id/42-L.jpg?default=false");
    task.abort();
}

#[tokio::test]
async fn missing_author_keeps_isbn_lookup() {
    let (client, provider, task) = provider(StatusCode::OK, 600).await;
    let mut book = record();
    book.contributors.clear();
    assert_eq!(client.book_cover(&book).await.unwrap().unwrap(), provider.isbn_image);
    assert_eq!(*provider.requests.lock().unwrap(), ["/ol/b/isbn/9780306406157-L.jpg?default=false"]);
    task.abort();
}

#[tokio::test]
async fn undersized_isbn_artwork_falls_back_to_search() {
    let (client, provider, task) = provider(StatusCode::OK, 50).await;
    assert_eq!(client.book_cover(&record()).await.unwrap().unwrap(), provider.fallback_image);
    assert_eq!(provider.requests.lock().unwrap().len(), 3);
    task.abort();
}

#[tokio::test]
async fn title_search_rejects_unrelated_authors_and_works() {
    let (client, provider, task) = provider(StatusCode::NOT_FOUND, 600).await;
    *provider.search_docs.lock().unwrap() = vec![
        serde_json::json!({"title":"Utilitarianism","author_name":["Other Author"],"cover_i":42}),
        serde_json::json!({"title":"Different Work","author_name":["John Stuart Mill"],"cover_i":42}),
        serde_json::json!({"title":"Utilitarianism: On Liberty and Other Essays","author_name":["John Stuart Mill"],"cover_i":42}),
    ];
    assert!(client.book_cover(&record()).await.unwrap().is_none());
    let paths = provider.requests.lock().unwrap().clone();
    assert_eq!(paths.len(), 2);
    assert!(paths.iter().all(|path| !path.starts_with("/ol/b/id/")));
    task.abort();
}

#[test]
fn isbn_lookup_normalizes_identifiers_and_respects_scope() {
    assert_eq!(isbn_key("0-306-40615-2"), isbn_key("9780306406157"));
    let mut book = record();
    assert_eq!(isbns(&book), ["9780306406157"]);
    book.book.identifiers[0].scope = Scope::Edition;
    assert!(isbns(&book).is_empty());
    book.book.identifiers.push(Identifier::new("B000FC1FIU", Scheme::Asin, Scope::Edition).unwrap());
    assert_eq!(isbns(&book), ["9780306406157"]);
}

#[tokio::test]
async fn metadata_miss_still_downloads_direct_artwork_and_preserves_newer_local_cover() {
    let (covers, provider, task) = provider(StatusCode::OK, 600).await;
    let temporary = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(":memory:").unwrap());
    db.initialize_library().unwrap();
    let hash = BookId::new(&"a".repeat(64));
    db.commit_import(ImportCommit {
        parent_id: ROOT_DIR_ID,
        file_name: "book.epub".into(),
        format: BookFormat::Epub,
        content_hash: hash,
        size_bytes: 10,
        inspection_version: 1,
        inspection: Some(book_inspection::InspectedBook { metadata: record(), pdf: None, audiobook: None, toc: Vec::new() }),
        published: PublishedStagedBook { name: "book.epub".into(), relative_path: "/book.epub".into(), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: Vec::new(),
    })
    .unwrap();
    db.complete_thumbnail_work(&hash, false).unwrap();
    let (sync, _) = SyncScheduler::new();
    let (assets_wake, _) = SyncScheduler::new();
    let (files, _) = SyncScheduler::new();
    let (enrichment, _) = SyncScheduler::new();
    let (events, _) = LibraryEventRouter::new(sync, assets_wake, files, enrichment);
    let assets = AssetStore::open(temporary.path().to_str().unwrap()).unwrap();
    let lane = LibraryEnrichment { database: db.clone(), assets: assets.clone(), client: MetadataClient::new(&format!("{}/metadata/", provider.origin)).unwrap(), covers, cpu: Arc::new(client_platform_native::cpu_host::NativeHost), events };
    assert_eq!(lane.enrich_books(&HashSet::from([hash])).await.unwrap(), 2);
    let original = assets.read_bytes(BlobKind::Thumbnail, &hash).await.unwrap().unwrap();
    assert!(!original.is_empty());
    assert!(db.enrichment_candidates(&[hash], 0).unwrap().is_empty());
    assert!(provider.requests.lock().unwrap().iter().all(|path| !path.contains("v3/covers")));
    let state = db.thumbnail_work_state(&hash).unwrap();
    let other = cpu_host::cover(&*lane.cpu, provider.fallback_image.clone()).await.unwrap();
    assert_eq!(lane.apply_enriched_cover(hash, &state, Some(&other)).await.unwrap(), None);
    assert_eq!(assets.read_bytes(BlobKind::Thumbnail, &hash).await.unwrap().unwrap(), original);
    task.abort();
}
