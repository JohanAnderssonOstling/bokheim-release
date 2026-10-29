//! Cancellation through the real HTTP client, including a partially sent body.

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use book_access::{pdf_bundle, remote_file, RemoteFile};
use futures_util::{stream, StreamExt};
use pdf_view_common::{PdfByteRange, PdfDependencyIndex, PdfPrefetchCancellation};
use tokio::sync::Notify;

use super::opening::{serve, Fixture};

const BLOCK: usize = 64 * 1024;

#[derive(Clone)]
struct StalledPdf {
    fixture: Fixture,
    requests: Arc<AtomicUsize>,
    started: Arc<Notify>,
    aborted: Arc<Notify>,
}

struct BodyLifetime(Arc<Notify>);
impl Drop for BodyLifetime {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

async fn serve_stalled(State(state): State<StalledPdf>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let response = serve(State(state.fixture.clone()), headers).await?;
    if state.requests.fetch_add(1, Ordering::SeqCst) != 1 {
        return Ok(response);
    }
    let (parts, _) = response.into_parts();
    // First prefetch batch completes normally. The second sends an incomplete,
    // deliberately incorrect block, then stalls forever unless HTTP is aborted.
    let partial = stream::once(async { Ok::<_, io::Error>(vec![99; BLOCK]) });
    let stalled = stream::unfold(BodyLifetime(state.aborted), move |owner| {
        let started = state.started.clone();
        async move {
            started.notify_one();
            std::future::pending::<()>().await;
            Some((Ok::<_, io::Error>(Vec::<u8>::new()), owner))
        }
    });
    Ok(Response::from_parts(parts, Body::from_stream(partial.chain(stalled))))
}

#[tokio::test]
async fn cancelling_inflight_pdf_prefetch_aborts_http_preserves_cache_and_allows_demand() {
    let fixture = Fixture { bytes: (0..160 * BLOCK).map(|position| (position / BLOCK) as u8).collect::<Vec<_>>().into(), requests: Arc::default(), hash: book_access::BookId::new(&"d".repeat(64)), legacy: false, delay_ms: 0 };
    let started = Arc::new(Notify::new());
    let aborted = Arc::new(Notify::new());
    let state = StalledPdf { fixture: fixture.clone(), requests: Arc::default(), started: started.clone(), aborted: aborted.clone() };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: reqwest::Url = format!("http://{}/book", listener.local_addr().unwrap()).parse().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, Router::new().route("/book", get(serve_stalled)).with_state(state)).await.unwrap() });
    let client = reqwest::Client::new();
    let file = RemoteFile { hash: fixture.hash, length: fixture.bytes.len() as u64, revision: None };
    let fetch = remote_file::native_bundle_fetch(move |ranges| {
        let (client, url, file) = (client.clone(), url.clone(), file.clone());
        Box::pin(async move { remote_file::fetch_ranges(&client, url, &file, "test", ranges).await })
    })
    .unwrap();
    let range = |block, count| PdfByteRange { offset: block * BLOCK as u64, length: count * BLOCK as u64 };
    let index = PdfDependencyIndex { length: fixture.bytes.len() as u64, startup: vec![], pages: vec![vec![range(1, 128)], vec![range(150, 1)]] };
    let (mut reader, hook) = pdf_bundle::open(index, vec![0; BLOCK], fetch).unwrap();
    let cancellation = PdfPrefetchCancellation::default();
    let pending = cancellation.clone();
    let prefetch = tokio::task::spawn_blocking(move || {
        hook.prefetch_pages_cancellable(&[0], &pending).unwrap();
        (reader, hook)
    });
    tokio::time::timeout(Duration::from_secs(3), started.notified()).await.unwrap();
    cancellation.cancel();
    let (returned_reader, hook) = tokio::time::timeout(Duration::from_secs(3), prefetch).await.expect("cancellation must free the parser without waiting for the 30-second HTTP timeout").unwrap();
    reader = returned_reader;
    tokio::time::timeout(Duration::from_secs(3), aborted.notified()).await.expect("dropping the HTTP future must close the server's stalled response body");
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    let requests = fixture.requests.clone();
    tokio::task::spawn_blocking(move || {
        for block in [1, 64] {
            reader.seek(SeekFrom::Start((block * BLOCK) as u64)).unwrap();
            let mut byte = [0];
            reader.read_exact(&mut byte).unwrap();
            assert_eq!(byte, [block as u8]);
        }
        assert_eq!(requests.lock().unwrap().len(), 2, "completed batches must remain cached");
        hook.prepare_page(1).unwrap();
        reader.seek(SeekFrom::Start((150 * BLOCK) as u64)).unwrap();
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [150], "new-page demand must succeed after cancellation");
        reader.seek(SeekFrom::Start((65 * BLOCK) as u64)).unwrap();
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [65], "the aborted partial block must be fetched again, never cached as 99");
    })
    .await
    .unwrap();
    assert_eq!(fixture.requests.lock().unwrap().len(), 4);
    server.abort();
}
