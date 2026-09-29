use super::*;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Method, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

#[test]
fn only_transient_http_statuses_are_retryable() {
    for status in [408, 429, 500, 502, 503, 504] {
        assert!(matches!(RemoteFileError::status(reqwest::StatusCode::from_u16(status).unwrap()), RemoteFileError::Retryable(_)));
    }
    for status in [200, 400, 403, 404, 412, 416] {
        assert!(matches!(RemoteFileError::status(reqwest::StatusCode::from_u16(status).unwrap()), RemoteFileError::Failed(_)));
    }
    assert!(matches!(RemoteFileError::status(reqwest::StatusCode::UNAUTHORIZED), RemoteFileError::Unauthorized));
}

fn epub() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in [
        ("META-INF/container.xml", br#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#.as_slice()),
        ("content.opf", br#"<package><metadata/><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="one"/><itemref idref="two"/></spine></package>"#),
        ("one.xhtml", b"<html><body>First chapter</body></html>"),
    ] {
        zip.start_file(name, options).unwrap(); zip.write_all(content).unwrap();
    }
    zip.start_file("unused.bin", options).unwrap();
    zip.write_all(&vec![7; 4 * 1024 * 1024]).unwrap();
    zip.start_file("two.xhtml", options).unwrap();
    zip.write_all(b"<html><body>Second chapter</body></html>").unwrap();
    zip.finish().unwrap().into_inner()
}

#[derive(Clone)]
struct Server {
    bytes: Arc<Vec<u8>>,
    etag: String,
    requests: Arc<Mutex<Vec<(u64, u64)>>>,
    mode: Arc<Mutex<&'static str>>,
}

async fn serve(State(state): State<Server>, method: Method, headers: HeaderMap) -> Response {
    if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer test-token") {
        return Response::builder().status(401).body(Body::empty()).unwrap();
    }
    let mode = *state.mode.lock().unwrap();
    let mut builder = Response::builder().header(header::ETAG, if mode == "etag" { "\"changed\"" } else { &state.etag }).header(header::ACCEPT_RANGES, "bytes");
    if method == Method::HEAD {
        return builder.header(header::CONTENT_LENGTH, state.bytes.len()).body(Body::empty()).unwrap();
    }
    let range = headers.get(header::RANGE).unwrap().to_str().unwrap().strip_prefix("bytes=").unwrap();
    let (start, end) = range.split_once('-').unwrap();
    let (start, end) = (start.parse::<u64>().unwrap(), end.parse::<u64>().unwrap().min(state.bytes.len() as u64 - 1));
    state.requests.lock().unwrap().push((start, end));
    if let Some(revision) = headers.get(header::IF_RANGE) {
        assert_eq!(revision.to_str().unwrap(), state.etag);
    } else {
        assert_eq!((start, end), (0, (BLOCK_BYTES as u64).min(state.bytes.len() as u64) - 1));
    }
    builder = builder.status(if mode == "full" { StatusCode::OK } else { StatusCode::PARTIAL_CONTENT });
    let actual_start = if mode == "bounds" { start + 1 } else { start };
    builder = builder.header(header::CONTENT_RANGE, format!("bytes {actual_start}-{end}/{}", if mode == "total" { "0".to_owned() } else { state.bytes.len().to_string() }));
    let mut bytes = state.bytes[start as usize..=end as usize].to_vec();
    if mode == "long" {
        bytes.push(42);
    }
    if mode == "short" {
        bytes.pop();
    }
    builder.body(Body::from(bytes)).unwrap()
}

async fn fixture() -> (Server, reqwest::Url, tokio::task::JoinHandle<()>) {
    fixture_with_bytes(epub()).await
}

async fn fixture_with_bytes(bytes: Vec<u8>) -> (Server, reqwest::Url, tokio::task::JoinHandle<()>) {
    let hash = test_support::fixture_content_hash(991);
    let state = Server { bytes: Arc::new(bytes), etag: format!("\"{hash}\""), requests: Default::default(), mode: Arc::new(Mutex::new("ok")) };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = reqwest::Url::parse(&format!("http://{}/book", listener.local_addr().unwrap())).unwrap();
    let app = Router::new().route("/book", get(serve)).with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (state, url, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_epub_opens_and_reads_chapters_without_downloading_the_archive() {
    let (state, url, server) = fixture().await;
    let (source, initial_bytes) = describe(url.clone(), "test-token".into(), test_support::fixture_content_hash(991)).await.unwrap();
    assert_eq!(*state.requests.lock().unwrap(), vec![(0, BLOCK_BYTES as u64 - 1)]);
    let file = source.clone();
    let reader = native_reader(source.length, initial_bytes, move |offset, length| {
        let url = url.clone();
        let file = file.clone();
        Box::pin(async move { fetch_range(&reqwest::Client::new(), url, &file, "test-token", offset, length).await.map_err(|error| error.to_string()) })
    })
    .unwrap();
    let requests = state.requests.clone();
    tokio::task::spawn_blocking(move || {
        let provider = epub_provider::EpubProvider::try_from_reader(reader).unwrap();
        assert!(provider.read_string("one.xhtml").unwrap().contains("First chapter"));
        assert!(provider.read_string("two.xhtml").unwrap().contains("Second chapter"));
        let count = requests.lock().unwrap().len();
        assert!(provider.read_string("one.xhtml").unwrap().contains("First chapter"));
        assert_eq!(requests.lock().unwrap().len(), count, "repeated reads use cached blocks");
    })
    .await
    .unwrap();
    let fetched: u64 = state.requests.lock().unwrap().iter().map(|(a, b)| b - a + 1).sum();
    assert!(fetched < state.bytes.len() as u64 / 4, "read {fetched} bytes from {}", state.bytes.len());
    assert!(!format!("{source:?}").contains("test-token"));
    server.abort();
}

#[tokio::test]
async fn remote_epub_rejects_full_stale_misaligned_and_incomplete_responses() {
    let (state, url, server) = fixture().await;
    let (source, initial_bytes) = describe(url.clone(), "test-token".into(), test_support::fixture_content_hash(991)).await.unwrap();
    assert_eq!(initial_bytes, state.bytes[..BLOCK_BYTES]);
    let client = reqwest::Client::new();
    for mode in ["full", "etag", "bounds", "short", "long", "total"] {
        *state.mode.lock().unwrap() = mode;
        assert!(describe(url.clone(), "test-token".into(), source.hash).await.is_err(), "opening accepted {mode}");
        assert!(fetch_range(&client, url.clone(), &source, "test-token", 0, 128).await.is_err(), "accepted {mode}");
    }
    *state.mode.lock().unwrap() = "ok";
    assert_eq!(fetch_range(&client, url.clone(), &source, "test-token", 0, MAX_RANGE_BYTES).await.unwrap(), state.bytes[..MAX_RANGE_BYTES]);
    let before = state.requests.lock().unwrap().len();
    for (offset, length) in [(0, 0), (0, MAX_RANGE_BYTES + 1), (source.length, 1), (u64::MAX, 2)] {
        assert!(fetch_range(&client, url.clone(), &source, "test-token", offset, length).await.is_err());
    }
    assert_eq!(state.requests.lock().unwrap().len(), before, "invalid ranges are rejected before HTTP");
    assert!(fetch_range(&client, url.clone(), &source, "wrong-token", 0, 128).await.is_err());
    server.abort();
}

#[test]
fn remote_epub_seeks_are_lazy_and_cache_is_bounded() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let record = requests.clone();
    let length = (CACHE_BLOCKS + 2) as u64 * BLOCK_BYTES as u64;
    let mut reader = RangeReader::new(
        length,
        Box::new(move |start, count| {
            record.lock().unwrap().push(start);
            Ok(vec![42; count])
        }),
    );
    assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), length);
    assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
    assert!(requests.lock().unwrap().is_empty());
    assert!(reader.seek(SeekFrom::Start(0)).is_ok());
    assert!(reader.seek(SeekFrom::Current(-1)).is_err());
    for i in 0..=CACHE_BLOCKS {
        reader.seek(SeekFrom::Start(i as u64 * BLOCK_BYTES as u64)).unwrap();
        reader.read_exact(&mut [0; 1]).unwrap();
    }
    assert_eq!(reader.cache.len(), CACHE_BLOCKS);
    reader.rewind().unwrap();
    reader.read_exact(&mut [0; 1]).unwrap();
    assert_eq!(requests.lock().unwrap().iter().filter(|&&offset| offset == 0).count(), 2);
}

#[test]
fn remote_epub_batches_sequential_reads_and_resets_after_seeking() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let record = requests.clone();
    let length = 2 * 1024 * 1024;
    let mut reader = RangeReader::new(
        length,
        Box::new(move |offset, count| {
            record.lock().unwrap().push((offset, count));
            Ok((0..count).map(|i| ((offset + i as u64) % 251) as u8).collect())
        }),
    );
    let mut bytes = vec![0; 1024 * 1024];
    reader.read_exact(&mut bytes[..BLOCK_BYTES]).unwrap();
    // ZIP readers use this to ask their current position. It must not cancel
    // sequential batching as a real seek would.
    reader.stream_position().unwrap();
    reader.read_exact(&mut bytes[BLOCK_BYTES..]).unwrap();
    assert!(bytes.iter().enumerate().all(|(i, &byte)| byte == (i % 251) as u8));
    {
        let ranges = requests.lock().unwrap();
        assert_eq!(ranges.len(), 6, "six round trips replace sixteen for 1 MiB");
        assert_eq!(ranges[0].1, BLOCK_BYTES);
        assert_eq!(ranges[1].1, 2 * BLOCK_BYTES);
        assert!(ranges.iter().all(|(_, count)| *count <= MAX_RANGE_BYTES));
        assert!(ranges.iter().map(|(_, count)| count).sum::<usize>() < bytes.len() + MAX_RANGE_BYTES);
    }
    reader.seek(SeekFrom::Start(24 * BLOCK_BYTES as u64)).unwrap();
    reader.read_exact(&mut [0; 1]).unwrap();
    assert_eq!(requests.lock().unwrap().last().unwrap().1, BLOCK_BYTES, "random seeks reset read-ahead");
    reader.seek(SeekFrom::Start(0)).unwrap();
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(requests.lock().unwrap().len(), 7, "revisited data remains cached");
    assert!(reader.cache.allocated_bytes() <= CACHE_BLOCKS * BLOCK_BYTES);
}

#[test]
fn remote_epub_batches_stop_before_cached_blocks_and_at_eof() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let record = requests.clone();
    let length = 5 * BLOCK_BYTES + 17;
    let mut reader = RangeReader::new(
        length as u64,
        Box::new(move |offset, count| {
            record.lock().unwrap().push((offset, count));
            Ok(vec![42; count])
        }),
    );
    reader.seek(SeekFrom::Start(4 * BLOCK_BYTES as u64)).unwrap();
    reader.read_exact(&mut [0; 1]).unwrap();
    reader.rewind().unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, vec![42; length]);
    let ranges = requests.lock().unwrap();
    assert_eq!(ranges.last().unwrap().1, 17, "the final request is clipped at EOF");
    let mut sorted = ranges.clone();
    sorted.sort_unstable();
    for pair in sorted.windows(2) {
        assert_eq!(pair[0].0 + pair[0].1 as u64, pair[1].0, "no gaps or duplicate downloaded ranges");
    }
    assert_eq!(sorted.iter().map(|(_, count)| count).sum::<usize>(), length);
}

#[test]
fn remote_epub_failed_batch_preserves_position_and_cache() {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let failure = fail.clone();
    let mut reader = RangeReader::new(
        8 * BLOCK_BYTES as u64,
        Box::new(move |offset, count| {
            if offset == BLOCK_BYTES as u64 && failure.swap(false, std::sync::atomic::Ordering::Relaxed) {
                return Ok(vec![42; count - 1]);
            }
            Ok(vec![42; count])
        }),
    );
    reader.read_exact(&mut vec![0; BLOCK_BYTES]).unwrap();
    assert_eq!(reader.read(&mut [0; 1]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(reader.position, BLOCK_BYTES as u64);
    assert_eq!(reader.cache.len(), 1, "no part of a malformed batch enters the cache");
    let mut bytes = vec![0; 2 * BLOCK_BYTES];
    reader.read_exact(&mut bytes).unwrap();
    assert!(bytes.iter().all(|&byte| byte == 42));
    assert_eq!(reader.cache.len(), 3);
}

#[test]
fn remote_epub_large_batched_scan_retains_at_most_four_mib() {
    let length = 3 * CACHE_BLOCKS * BLOCK_BYTES + 17;
    let mut reader = RangeReader::new(length as u64, Box::new(|_, count| Ok(vec![42; count])));
    let mut buffer = [0; 8192];
    let mut consumed = 0;
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        consumed += count;
        assert!(reader.cache.len() <= CACHE_BLOCKS);
        assert!(reader.cache.allocated_bytes() <= CACHE_BLOCKS * BLOCK_BYTES);
    }
    assert_eq!(consumed, length);
}

#[test]
fn remote_epub_batch_lookahead_does_not_overflow_large_file_offsets() {
    let mut reader = RangeReader::new(u64::MAX, Box::new(|_, count| Ok(vec![42; count])));
    reader.seek(SeekFrom::End(-(3 * BLOCK_BYTES as i64))).unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, vec![42; 3 * BLOCK_BYTES]);
    assert_eq!(reader.position, u64::MAX);
}

#[tokio::test]
async fn remote_epub_small_file_is_served_entirely_from_opening_prefix() {
    for length in [1, BLOCK_BYTES - 1, BLOCK_BYTES] {
        let bytes = vec![42; length];
        let (state, url, server) = fixture_with_bytes(bytes.clone()).await;
        let (source, prefix) = describe(url, "test-token".into(), test_support::fixture_content_hash(991)).await.unwrap();
        assert_eq!(source.length, length as u64);
        assert_eq!(prefix, bytes);
        let mut reader = native_reader(source.length, prefix, |_, _| panic!("prefix must satisfy every read")).unwrap();
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, bytes);
        assert_eq!(*state.requests.lock().unwrap(), vec![(0, length as u64 - 1)]);
        server.abort();
    }
}

#[tokio::test]
async fn remote_file_pins_uploaded_revision_separately_from_book_identity() {
    let (state, url, server) = fixture().await;
    let identity = test_support::fixture_content_hash(992);
    let (source, prefix) = describe(url.clone(), "test-token".into(), identity).await.unwrap();
    assert_eq!(source.hash, identity);
    assert_eq!(source.checksum, test_support::fixture_content_hash(991));
    assert_eq!(prefix, state.bytes[..BLOCK_BYTES]);
    assert_eq!(fetch_range(&reqwest::Client::new(), url.clone(), &source, "test-token", BLOCK_BYTES as u64, 128).await.unwrap(), state.bytes[BLOCK_BYTES..BLOCK_BYTES + 128]);
    *state.mode.lock().unwrap() = "etag";
    assert!(fetch_range(&reqwest::Client::new(), url, &source, "test-token", BLOCK_BYTES as u64, 128).await.is_err());
    server.abort();
}

#[tokio::test]
async fn pdf_bundle_transport_uses_one_authenticated_post_and_checks_revision() {
    let checksum = test_support::fixture_content_hash(991);
    let source = RemoteFile { hash: checksum, checksum, length: 16 };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/blob/ranges",
        axum::routing::post(move |headers: HeaderMap, body: axum::body::Bytes| async move {
            assert_eq!(headers[header::AUTHORIZATION], "Bearer test");
            assert_eq!(headers[header::CONTENT_TYPE], sync_common::transport::MEDIA_TYPE);
            let request: sync_common::api::assets::BookRanges = sync_common::transport::decode(&body, 64 * 1024).unwrap();
            assert_eq!(request.ranges, vec![(1, 2), (8, 3)]);
            Response::builder().status(200).header(header::ETAG, format!("\"{checksum}\"")).body(Body::from(b"1289a".to_vec())).unwrap()
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let url = reqwest::Url::parse(&format!("http://{address}/blob")).unwrap();
    assert_eq!(fetch_bundle(&reqwest::Client::new(), url.clone(), &source, "test", vec![(1, 2), (8, 3)]).await.unwrap(), b"1289a");
    let mut changed = source.clone();
    changed.checksum = test_support::fixture_content_hash(992);
    assert!(matches!(fetch_bundle(&reqwest::Client::new(), url, &changed, "test", vec![(1, 2), (8, 3)]).await, Err(RemoteFileError::Failed(_))));
    server.abort();
}
