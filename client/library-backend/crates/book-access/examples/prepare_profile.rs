//! Desktop EPUB preparation without library startup or an HTML engine.
//!
//! cargo run --offline --release -p book-access --example prepare_profile -- BOOK.epub RESULTS.json [RUNS]
//!
//! Compares the production async local-file source with a memory-backed range
//! source using the same RangeReader and ZIP/decoder path. Each sample opens a
//! fresh container outside the preparation timer; filesystem caches stay warm.
//! Overall measurements run before diagnostics; first samples stay separate.
//! File/memory trials are paired, alternating their order between repetitions.
//! Linux caller CPU time excludes the blocking pool's file-read CPU time.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    hint::black_box,
    io::{self, SeekFrom},
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use book_access::AsyncBookSource;
use book_content::{Book, BookFormat, PreparationProfile, PreparationTiming, ReadingBook};
use futures_util::io::{AsyncBufRead, AsyncRead, AsyncSeek};
use serde_json::{json, Value};

#[derive(Default)]
struct SourceStats {
    poll_ns: AtomicU64,
    polls: AtomicU64,
    pending: AtomicU64,
    consumed_bytes: AtomicU64,
}

impl SourceStats {
    fn reset(&self) {
        for counter in [&self.poll_ns, &self.polls, &self.pending, &self.consumed_bytes] {
            counter.store(0, Ordering::Relaxed);
        }
    }

    fn record<T>(&self, started: Instant, result: &Poll<T>) {
        self.poll_ns.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        self.polls.fetch_add(1, Ordering::Relaxed);
        self.pending.fetch_add(u64::from(result.is_pending()), Ordering::Relaxed);
    }
}

struct ObservedReader<R, const PROFILE: bool> {
    inner: R,
    stats: Arc<SourceStats>,
}

impl<R: AsyncRead + Unpin, const PROFILE: bool> AsyncRead for ObservedReader<R, PROFILE> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &mut [u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if !PROFILE {
            return Pin::new(&mut this.inner).poll_read(cx, bytes);
        }
        let started = Instant::now();
        let result = Pin::new(&mut this.inner).poll_read(cx, bytes);
        this.stats.record(started, &result);
        if let Poll::Ready(Ok(count)) = &result {
            this.stats.consumed_bytes.fetch_add(*count as u64, Ordering::Relaxed);
        }
        result
    }
}

impl<R: AsyncBufRead + Unpin, const PROFILE: bool> AsyncBufRead for ObservedReader<R, PROFILE> {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<&[u8]>> {
        let this = self.get_mut();
        if !PROFILE {
            return Pin::new(&mut this.inner).poll_fill_buf(cx);
        }
        let started = Instant::now();
        let result = Pin::new(&mut this.inner).poll_fill_buf(cx);
        this.stats.record(started, &result);
        result
    }

    fn consume(self: Pin<&mut Self>, amount: usize) {
        let this = self.get_mut();
        if PROFILE {
            this.stats.consumed_bytes.fetch_add(amount as u64, Ordering::Relaxed);
        }
        Pin::new(&mut this.inner).consume(amount);
    }
}

impl<R: AsyncSeek + Unpin, const PROFILE: bool> AsyncSeek for ObservedReader<R, PROFILE> {
    fn poll_seek(self: Pin<&mut Self>, cx: &mut Context<'_>, position: SeekFrom) -> Poll<io::Result<u64>> {
        let this = self.get_mut();
        if !PROFILE {
            return Pin::new(&mut this.inner).poll_seek(cx, position);
        }
        let started = Instant::now();
        let result = Pin::new(&mut this.inner).poll_seek(cx, position);
        this.stats.record(started, &result);
        result
    }
}

#[cfg(target_os = "linux")]
fn thread_cpu() -> Option<Duration> {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `time` is a live, writable timespec; this clock belongs to the
    // calling thread. Tokio's main future stays on this thread across awaits.
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) }, 0);
    Some(Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
}

#[cfg(not(target_os = "linux"))]
fn thread_cpu() -> Option<Duration> {
    None
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn timing(t: &PreparationTiming) -> Value {
    json!({"elapsed_ms": ms(t.elapsed), "active_ms": ms(t.active), "suspended_ms": ms(t.suspended()), "polls": t.polls})
}

fn profile_json(profile: PreparationProfile, reading: &ReadingBook) -> Value {
    let stages: BTreeMap<_, _> = profile.stages.iter().map(|stage| (stage.name, timing(&stage.timing))).collect();
    let resources: Vec<_> = profile
        .resources
        .into_iter()
        .map(|resource| {
            let r = resource.timing;
            json!({"uri": resource.uri, "media_type": reading.resources.media_type(&resource.uri).unwrap(),
            "compressed_bytes": r.compressed_bytes, "output_bytes": r.output_bytes, "output_capacity": r.output_capacity,
            "zip_open": timing(&r.open), "buffer_allocation": timing(&r.allocate), "read_decode": timing(&r.decode)})
        })
        .collect();
    json!({"total": timing(&profile.total), "stages": stages, "resources": resources})
}

async fn sample<const DETAILED: bool>(path: &PathBuf, memory: Option<Arc<[u8]>>) -> Value {
    let source = if let Some(bytes) = memory {
        AsyncBookSource::from_ranges(bytes.len() as u64, Vec::new(), move |offset, length| {
            let result = bytes[offset as usize..offset as usize + length].to_vec();
            Box::pin(async move { Ok(result) })
        })
        .unwrap()
    } else {
        AsyncBookSource::from_file(File::open(path).unwrap()).unwrap()
    };
    let stats = Arc::new(SourceStats::default());
    let reader_stats = stats.clone();
    let book = Book::open(BookFormat::Epub, move || ObservedReader::<_, DETAILED> { inner: source.reader(), stats: reader_stats.clone() }).await.expect("open EPUB");
    stats.reset();
    let cpu_started = thread_cpu();
    let started = Instant::now();
    let (reading, profile) = if DETAILED {
        let (reading, profile) = book.prepare_reading_profiled().await.expect("profile preparation");
        (reading, Some(profile))
    } else {
        (book.prepare_reading().await.expect("prepare reading"), None)
    };
    let total_ms = ms(started.elapsed());
    let caller_cpu_ms = thread_cpu().zip(cpu_started).map(|(end, start)| ms(end.saturating_sub(start)));
    // Resource inspection and JSON conversion occur after stopping the timer.
    let spine_bytes: u64 = reading.document_weights.iter().sum();
    let profile = profile.map(|profile| profile_json(profile, &reading));
    black_box(&reading);
    let source = DETAILED.then(|| {
        json!({"poll_active_ms": stats.poll_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0,
        "polls": stats.polls.load(Ordering::Relaxed), "pending_polls": stats.pending.load(Ordering::Relaxed), "consumed_bytes": stats.consumed_bytes.load(Ordering::Relaxed)})
    });
    json!({"total_ms": total_ms, "caller_cpu_ms": caller_cpu_ms, "source": source, "documents": reading.document_uris().len(), "spine_bytes": spine_bytes, "profile": profile})
}

fn summary(samples: &[Value]) -> Value {
    let mut totals: Vec<_> = samples.iter().map(|sample| sample["total_ms"].as_f64().unwrap()).collect();
    totals.sort_by(f64::total_cmp);
    let n = totals.len();
    let median = if n % 2 == 0 { (totals[n / 2 - 1] + totals[n / 2]) / 2.0 } else { totals[n / 2] };
    json!({"median_ms": median, "min_ms": totals[0], "max_ms": totals[n - 1]})
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("BOOK.epub RESULTS.json [RUNS]"));
    let output = PathBuf::from(args.next().expect("results JSON path"));
    let runs: usize = args.next().map(|n| n.to_string_lossy().parse().expect("positive run count")).unwrap_or(50);
    assert!(runs > 0 && args.next().is_none());
    let bytes: Arc<[u8]> = fs::read(&path).expect("fixture").into();
    let mut result = json!({"book": path, "file_bytes": bytes.len(), "runs": runs, "debug_assertions": cfg!(debug_assertions),
        "conditions": "Desktop release; warm filesystem cache; fresh container per sample; container opening excluded; no library, import or HTML engine; 2 Tokio workers; memory control uses the same range-reader and ZIP path",
        "timing_notes": "Active time measures wall time inside poll calls, not CPU usage; suspended time includes I/O and scheduling. Linux caller_cpu_ms uses CLOCK_THREAD_CPUTIME_ID and excludes I/O-worker CPU. Nested scopes overlap. Diagnostics add clocks and bookkeeping; baseline has no internal clock sampling. File/memory trials are paired with alternating order, all baselines before diagnostics.",
        "cases": {}});
    for detailed in [false, true] {
        let mut file_samples = Vec::new();
        let mut memory_samples = Vec::new();
        for index in 0..=runs {
            for in_memory in if index % 2 == 0 { [false, true] } else { [true, false] } {
                let value = if detailed { sample::<true>(&path, in_memory.then(|| bytes.clone())).await } else { sample::<false>(&path, in_memory.then(|| bytes.clone())).await };
                if in_memory {
                    memory_samples.push(value);
                } else {
                    file_samples.push(value);
                }
            }
        }
        for (in_memory, samples) in [(false, file_samples), (true, memory_samples)] {
            let case = match (in_memory, detailed) {
                (false, false) => "file_overall",
                (false, true) => "file_breakdown",
                (true, false) => "memory_overall",
                (true, true) => "memory_breakdown",
            };
            let stats = summary(&samples[1..]);
            println!("{case}: {stats}");
            result["cases"][case] = json!({"first": samples[0], "samples": &samples[1..], "summary": stats});
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&output, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        }
    }
}
