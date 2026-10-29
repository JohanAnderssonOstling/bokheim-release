//! Exercise the real multipart handler, client transport, ZIP plan and decoder.

use std::{
    io::{Cursor, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    routing::get,
    Router,
};
use book_access::{remote_file, AsyncBookSource, FetchFuture, RemoteFileError, SparseBookSource};
use book_content::{Book, BookFormat, EpubOpeningPlan, ReadingBook};
use server_asset_store::{AssetKind, StoredAsset};
use sync_common::BookId;

use super::{stream_asset, TransferGovernor};

#[derive(Clone)]
pub(super) struct Fixture {
    pub(super) bytes: Arc<[u8]>,
    pub(super) requests: Arc<Mutex<Vec<(String, u64)>>>,
    pub(super) hash: BookId,
    pub(super) legacy: bool,
    pub(super) delay_ms: u64,
}

pub(super) async fn serve(State(fixture): State<Fixture>, headers: HeaderMap) -> Result<axum::response::Response, StatusCode> {
    if fixture.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(fixture.delay_ms)).await;
    }
    if fixture.legacy && headers[header::RANGE].to_str().unwrap().contains(',') {
        fixture.requests.lock().unwrap().push((headers[header::RANGE].to_str().unwrap().to_owned(), 0));
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let asset = StoredAsset { reader: Box::new(Cursor::new(fixture.bytes.clone())), total_len: fixture.bytes.len() as u64, offset: 0, content_hash: fixture.hash };
    let response = stream_asset(asset, AssetKind::Book, &headers, TransferGovernor::new(Default::default()), "test".into()).await?;
    fixture.requests.lock().unwrap().push((headers[header::RANGE].to_str().unwrap().to_owned(), response.headers()[header::CONTENT_LENGTH].to_str().unwrap().parse().unwrap()));
    Ok(response)
}

fn epub(unusual_name: bool, max_comment: bool) -> Vec<u8> {
    let chapter = if unusual_name { "chapter.xml" } else { "chapter.xhtml" };
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opf = format!(r#"<package><manifest><item id="chapter" href="{chapter}" media-type="application/xhtml+xml"/><item id="css" href="style.css" media-type="text/css"/></manifest><spine><itemref idref="chapter"/></spine></package>"#);
    let entries = [
        ("META-INF/container.xml", b"<container><rootfiles><rootfile full-path=\"book.opf\"/></rootfiles></container>".to_vec()),
        ("book.opf", opf.into_bytes()),
        ("style.css", b"body { color: black; }".to_vec()),
        ("font.bin", vec![5; 128 * 1024]),
        (chapter, b"<html><body>Exact remote chapter</body></html>".to_vec()),
        ("image.bin", vec![7; 256 * 1024]),
    ];
    for (name, bytes) in entries {
        archive.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        archive.write_all(&bytes).unwrap();
    }
    if max_comment {
        archive.set_comment("x".repeat(u16::MAX as usize)).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

async fn open(bytes: Vec<u8>) -> Vec<(String, u64)> {
    let fixture = Fixture { bytes: bytes.into(), requests: Arc::default(), hash: BookId::new(&"d".repeat(64)), legacy: false, delay_ms: 0 };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    run(fixture, listener).await
}

async fn run(fixture: Fixture, listener: tokio::net::TcpListener) -> Vec<(String, u64)> {
    let (reading, requests, _) = prepare(fixture, listener, true).await;
    assert_eq!(reading.document_uris().len(), 1);
    assert_eq!(reading.document_weights.iter().sum::<u64>(), b"<html><body>Exact remote chapter</body></html>".len() as u64);
    assert_eq!(reading.resources.read_bytes(&reading.document_uris()[0]).unwrap(), b"<html><body>Exact remote chapter</body></html>");
    assert_eq!(reading.resources.read_bytes("style.css").unwrap(), b"body { color: black; }");
    requests
}

async fn prepare(fixture: Fixture, listener: tokio::net::TcpListener, optimized: bool) -> (ReadingBook, Vec<(String, u64)>, Duration) {
    let url = format!("http://{}/book", listener.local_addr().unwrap());
    let app = Router::new().route("/book", get(serve)).with_state(fixture.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let started = Instant::now();
    let (file, seed) = if optimized { remote_file::describe_tail(&client, url.parse().unwrap(), "test", fixture.hash).await.unwrap() } else { remote_file::describe(&client, url.parse().unwrap(), "test", fixture.hash).await.unwrap() };
    let read_client = client.clone();
    let read_url = url.clone();
    let read_file = file.clone();
    let range_fetch = move |offset, count| {
        let (client, url, file) = (read_client.clone(), read_url.clone(), read_file.clone());
        Box::pin(async move { remote_file::fetch_range(&client, url.parse().unwrap(), &file, "test", offset, count).await }) as FetchFuture<'static, Result<Vec<u8>, RemoteFileError>>
    };
    let source = if optimized {
        let sparse = SparseBookSource::remote(file.length, file.length - seed.len() as u64, seed, range_fetch).unwrap();
        let plan_source = sparse.clone();
        let plan = EpubOpeningPlan::read(move || plan_source.reader()).await.unwrap();
        let fetch = move |ranges| {
            let (client, url, file) = (client.clone(), url.clone(), file.clone());
            Box::pin(async move { remote_file::fetch_ranges(&client, url.parse().unwrap(), &file, "test", ranges).await }) as FetchFuture<'static, Result<Vec<u8>, RemoteFileError>>
        };
        sparse.prefetch(&plan.initial_ranges().unwrap(), &fetch).await.unwrap();
        let plan_source = sparse.clone();
        let required = plan.required_ranges(move || plan_source.reader()).await.unwrap();
        sparse.prefetch(&required, fetch).await.unwrap();
        AsyncBookSource::from(sparse)
    } else {
        AsyncBookSource::remote(file.length, seed, range_fetch).unwrap()
    };
    let reading = Book::open(BookFormat::Epub, move || source.reader()).await.unwrap().prepare_reading().await.unwrap();
    let elapsed = started.elapsed();
    server.abort();
    let requests = fixture.requests.lock().unwrap().clone();
    (reading, requests, elapsed)
}

#[tokio::test]
async fn desktop_epub_uses_tail_then_one_multipart_request() {
    let requests = open(epub(false, false)).await;
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0].0, "bytes=-65557");
    assert!(requests[1].0.contains(','));
    assert!(requests.iter().map(|(_, count)| count).sum::<u64>() < 68 * 1024);
}

#[tokio::test]
async fn manifest_adds_html_with_an_unusual_filename() {
    let requests = open(epub(true, false)).await;
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(requests.iter().map(|(_, count)| count).sum::<u64>() < 68 * 1024);
}

#[tokio::test]
async fn maximum_zip_comment_fetches_the_directory_without_redownloading_the_tail() {
    let requests = open(epub(false, true)).await;
    assert_eq!(requests[0].0, "bytes=-65557");
    // A maximum comment fills the tail window. ZIP64-locator probing and the
    // directory occupy separate preceding intervals, then contents are batched.
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert!(requests.iter().map(|(_, count)| count).sum::<u64>() < 70 * 1024);
}

#[tokio::test]
async fn older_servers_fall_back_to_exact_single_ranges() {
    let fixture = Fixture { bytes: epub(false, false).into(), requests: Arc::default(), hash: BookId::new(&"d".repeat(64)), legacy: true, delay_ms: 0 };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let requests = run(fixture, listener).await;
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert_eq!(requests.iter().filter(|(_, bytes)| *bytes == 0).count(), 1);
}

#[tokio::test]
#[ignore = "set BOKHEIM_EPUB_FIXTURE and BOKHEIM_EPUB_PROFILE_OUTPUT for a desktop loopback HTTP benchmark"]
async fn profile_remote_epub_fixture() {
    let bytes: Arc<[u8]> = std::fs::read(std::env::var_os("BOKHEIM_EPUB_FIXTURE").expect("BOKHEIM_EPUB_FIXTURE")).unwrap().into();
    let expected = Book::open_reader(BookFormat::Epub, Cursor::new(bytes.clone())).unwrap().prepare_reading().await.unwrap();
    let mut csv = String::from("mode,delay_ms,iteration,total_ms,requests,response_body_bytes,documents,spine_bytes\n");
    for delay_ms in [0, 20, 50] {
        for iteration in 0..5 {
            // Pair trials and alternate their order to limit load/order bias.
            let modes = if iteration % 2 == 0 { [false, true] } else { [true, false] };
            for optimized in modes {
                let fixture = Fixture { bytes: bytes.clone(), requests: Arc::default(), hash: BookId::new(&"d".repeat(64)), legacy: false, delay_ms };
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let (reading, requests, elapsed) = prepare(fixture, listener, optimized).await;
                assert_eq!(reading.document_uris(), expected.document_uris());
                assert_eq!(reading.document_weights, expected.document_weights);
                for uri in expected.document_uris() {
                    assert_eq!(reading.resources.read_bytes(uri).unwrap(), expected.resources.read_bytes(uri).unwrap());
                }
                let mode = if optimized { "zip_ranges" } else { "range_baseline" };
                csv.push_str(&format!(
                    "{mode},{delay_ms},{iteration},{:.6},{},{},{},{}\n",
                    elapsed.as_secs_f64() * 1000.0,
                    requests.len(),
                    requests.iter().map(|(_, count)| count).sum::<u64>(),
                    reading.document_uris().len(),
                    reading.document_weights.iter().sum::<u64>()
                ));
            }
        }
    }
    let output = std::env::var_os("BOKHEIM_EPUB_PROFILE_OUTPUT").expect("BOKHEIM_EPUB_PROFILE_OUTPUT");
    std::fs::write(output, &csv).unwrap();
    println!("{csv}");
}

#[tokio::test]
#[ignore = "set BOKHEIM_EPUB_CORPUS_LIST to a newline-separated list of local EPUB paths"]
async fn remote_corpus_matches_local_eager_preparation() {
    let list = std::fs::read_to_string(std::env::var_os("BOKHEIM_EPUB_CORPUS_LIST").expect("BOKHEIM_EPUB_CORPUS_LIST")).unwrap();
    let mut checked = 0;
    let mut skipped = 0;
    for path in list.lines().filter(|line| !line.is_empty()) {
        let bytes: Arc<[u8]> = std::fs::read(path).unwrap().into();
        let expected = match Book::open_reader(BookFormat::Epub, Cursor::new(bytes.clone())) {
            Ok(book) => book.prepare_reading().await,
            Err(error) => Err(error),
        };
        let expected = match expected {
            Ok(reading) => reading,
            Err(error) => {
                skipped += 1;
                println!("CORPUS_SKIP local_reader_error={error} {path}");
                continue;
            }
        };
        println!("CORPUS_START {path}");
        let fixture = Fixture { bytes: bytes.clone(), requests: Arc::default(), hash: BookId::new(&"d".repeat(64)), legacy: false, delay_ms: 0 };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (actual, requests, _) = prepare(fixture, listener, true).await;
        assert_eq!(actual.document_uris(), expected.document_uris(), "{path}");
        assert_eq!(actual.document_weights, expected.document_weights, "{path}");
        let zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        for name in zip.file_names() {
            if let Ok(expected) = expected.resources.read_bytes(name) {
                assert_eq!(actual.resources.read_bytes(name).unwrap(), expected, "{path}: {name}");
            }
        }
        checked += 1;
        println!("CORPUS {checked} requests={} body_bytes={} {path}", requests.len(), requests.iter().map(|(_, n)| n).sum::<u64>());
    }
    assert!(checked > 0);
    println!("CORPUS_RESULT checked={checked} skipped_local_reader_errors={skipped}");
}
