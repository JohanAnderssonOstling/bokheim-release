//! Desktop remote EPUB opening through the production range reader.
//!
//! cargo run --offline --release -p book-access --example remote_open_profile -- BOOK.epub RESULTS.json [RUNS]
//!
//! Bytes come from memory with a fixed simulated delay per request. This measures
//! request dependencies and transfer amplification, not a live server, bandwidth,
//! TLS, authentication, library lookup, images, or HTML-engine rendering.

use std::{
    env, fs,
    hint::black_box,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use book_access::AsyncBookSource;
use book_content::{Book, BookFormat};
use serde_json::{json, Value};

#[derive(Clone)]
struct Request {
    phase: usize,
    offset: u64,
    length: usize,
    started_ms: f64,
    elapsed_ms: f64,
}

struct Transfers {
    bytes: Arc<[u8]>,
    delay: Duration,
    started: Instant,
    phase: AtomicUsize,
    inflight: AtomicUsize,
    peak: AtomicUsize,
    requests: Mutex<Vec<Request>>,
}

impl Transfers {
    async fn fetch(&self, offset: u64, length: usize) -> Vec<u8> {
        let phase = self.phase.load(Ordering::Relaxed);
        let started = Instant::now();
        let active = self.inflight.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak.fetch_max(active, Ordering::Relaxed);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let bytes = self.bytes[offset as usize..offset as usize + length].to_vec();
        self.inflight.fetch_sub(1, Ordering::Relaxed);
        self.requests.lock().unwrap().push(Request { phase, offset, length, started_ms: ms(started.duration_since(self.started)), elapsed_ms: ms(started.elapsed()) });
        bytes
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn unique_bytes(requests: &[Request]) -> u64 {
    let mut intervals: Vec<_> = requests.iter().map(|r| (r.offset, r.offset + r.length as u64)).collect();
    intervals.sort_unstable();
    let mut end = 0;
    let mut total = 0;
    for (start, next_end) in intervals {
        total += next_end.saturating_sub(start.max(end));
        end = end.max(next_end);
    }
    total
}

async fn sample(bytes: Arc<[u8]>, delay_ms: u64) -> Value {
    let started = Instant::now();
    let transfers = Arc::new(Transfers { bytes, delay: Duration::from_millis(delay_ms), started, phase: AtomicUsize::new(0), inflight: AtomicUsize::new(0), peak: AtomicUsize::new(0), requests: Mutex::new(Vec::new()) });
    // Production remote description fetches the first 64 KiB before Book::open.
    let prefix = transfers.fetch(0, transfers.bytes.len().min(64 * 1024)).await;
    let prefix_ms = ms(started.elapsed());
    let fetch = transfers.clone();
    let source = AsyncBookSource::remote(transfers.bytes.len() as u64, prefix, move |offset, length| {
        let fetch = fetch.clone();
        Box::pin(async move { Ok(fetch.fetch(offset, length).await) })
    })
    .unwrap();
    let readers = Arc::new(AtomicUsize::new(0));
    let reader_count = readers.clone();
    transfers.phase.store(1, Ordering::Relaxed);
    let opened = Instant::now();
    let book = Book::open(BookFormat::Epub, move || {
        reader_count.fetch_add(1, Ordering::Relaxed);
        source.reader()
    })
    .await
    .expect("open remote EPUB");
    let open_ms = ms(opened.elapsed());
    transfers.phase.store(2, Ordering::Relaxed);
    let prepared = Instant::now();
    let reading = book.prepare_reading().await.expect("eagerly prepare remote EPUB");
    let preparation_ms = ms(prepared.elapsed());
    let total_ms = ms(started.elapsed());
    black_box(&reading);
    let requests = transfers.requests.lock().unwrap();
    let transferred: usize = requests.iter().map(|r| r.length).sum();
    let unique = unique_bytes(&requests);
    let phases: Vec<_> = ["prefix", "container_open", "eager_preparation"]
        .into_iter()
        .enumerate()
        .map(|(phase, name)| {
            let phase_requests: Vec<_> = requests.iter().filter(|r| r.phase == phase).cloned().collect();
            json!({"phase": name, "requests": phase_requests.len(), "transferred_bytes": phase_requests.iter().map(|r| r.length).sum::<usize>(), "unique_bytes": unique_bytes(&phase_requests)})
        })
        .collect();
    let trace: Vec<_> = requests.iter().map(|r| json!({"phase": r.phase, "offset": r.offset, "length": r.length, "started_ms": r.started_ms, "elapsed_ms": r.elapsed_ms})).collect();
    json!({"simulated_request_delay_ms": delay_ms, "total_ms": total_ms, "prefix_ms": prefix_ms, "container_open_ms": open_ms, "eager_preparation_ms": preparation_ms,
        "requests": requests.len(), "transferred_bytes": transferred, "unique_bytes": unique, "repeated_bytes": transferred as u64 - unique,
        "peak_concurrent_requests": transfers.peak.load(Ordering::Relaxed), "readers_created": readers.load(Ordering::Relaxed),
        "documents": reading.document_uris().len(), "spine_bytes": reading.document_weights.iter().sum::<u64>(), "phases": phases, "trace": trace})
}

#[tokio::main]
async fn main() {
    let mut args = env::args_os().skip(1);
    let book = PathBuf::from(args.next().expect("BOOK.epub"));
    let output = PathBuf::from(args.next().expect("RESULTS.json"));
    let runs: usize = args.next().map(|v| v.to_str().unwrap().parse().unwrap()).unwrap_or(5);
    assert!(runs > 0);
    let bytes: Arc<[u8]> = fs::read(&book).expect("read input EPUB").into();
    let mut samples = Vec::new();
    for delay_ms in [0, 20, 50] {
        for iteration in 0..runs {
            let mut result = sample(bytes.clone(), delay_ms).await;
            result["iteration"] = json!(iteration);
            eprintln!("delay={delay_ms}ms run={iteration} total={:.2}ms requests={} bytes={}", result["total_ms"].as_f64().unwrap(), result["requests"], result["transferred_bytes"]);
            samples.push(result);
        }
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(output, serde_json::to_vec_pretty(&json!({"book": book, "file_bytes": bytes.len(), "method": "Production AsyncBookSource::remote and eager EPUB preparation; fixed per-request latency simulation; no bandwidth/TLS/auth/library/HTML engine/image requests", "samples": samples})).unwrap()).unwrap();
}
