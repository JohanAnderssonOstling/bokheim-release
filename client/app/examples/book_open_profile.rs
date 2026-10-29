//! Desktop local-book opening, stopping before the HTML engine.
//!
//! BOKHEIM_CATALOG_FILE=/nonexistent cargo run --offline --release -p app \
//!   --example book_open_profile -- BOOK.epub OUTPUT_DIRECTORY [RUNS]
//!
//! Imports into a new private library outside the timer. Records total-only
//! samples first, then the same workload with stage timers. Filesystem caches
//! are warm after import; the first sample of each phase is reported separately.
//! Excludes application startup, import, HTML parsing/layout, fonts and paint.
//! The catalog override prevents an unrelated background catalog download.
//! Uses Tokio's blocking pool for preparation in place of GPUI's executor.
//! Append --lookup-only to profile just LibraryClient::resolve_book, first
//! without instrumentation and then with buffered, nested startup-profile events.
//! Lookup mode compares a cached handle with a fresh client per opening, as
//! the desktop shell creates for ReaderView (the library actor remains open).

use std::{
    collections::BTreeMap,
    fs,
    hint::black_box,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use app::{AppClient, AppDataLocation, host::BackendLaunch};
use book_content::{Book, BookFormat};
use futures_lite::future::block_on;
use library_backend::{BookId, DocumentSource, LibraryClient, ROOT_DIR_ID};
use serde_json::{Value, json};

type Stages = BTreeMap<&'static str, f64>;

struct LookupLogger {
    active: AtomicBool,
    events: Mutex<Vec<String>>,
}

static LOOKUP_LOGGER: LookupLogger = LookupLogger { active: AtomicBool::new(false), events: Mutex::new(Vec::new()) };

impl log::Log for LookupLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "startup_profile" && self.active.load(Ordering::Relaxed)
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let event = record.args().to_string();
            if event.starts_with("scope=book_lookup_") {
                self.events.lock().unwrap().push(event);
            }
        }
    }

    fn flush(&self) {}
}

async fn lookup_sample<const DETAILED: bool>(library: &LibraryClient, hash: BookId) -> Value {
    if DETAILED {
        LOOKUP_LOGGER.events.lock().unwrap().reserve(40);
    }
    LOOKUP_LOGGER.active.store(DETAILED, Ordering::Relaxed);
    let started = Instant::now();
    let resolved = library.resolve_book(hash).await.expect("resolve local book");
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    LOOKUP_LOGGER.active.store(false, Ordering::Relaxed);
    // Parsing/aggregation happens after the sample. Log formatting is included
    // in profiled timings, but events never write to a terminal or file here.
    let lines = std::mem::take(&mut *LOOKUP_LOGGER.events.lock().unwrap());
    let mut stages = BTreeMap::new();
    let mut events = Vec::new();
    for line in lines {
        let fields: BTreeMap<_, _> = line.split_whitespace().map(|field| field.split_once('=').unwrap()).collect();
        let scope = fields["scope"];
        let stage = fields["stage"];
        let duration_ms: f64 = fields["duration_ms"].parse().unwrap();
        let elapsed_ms: f64 = fields["elapsed_ms"].parse().unwrap();
        let since_start_ms: f64 = fields["since_start_ms"].parse().unwrap();
        assert!(stages.insert(format!("{scope}.{stage}"), duration_ms).is_none(), "unexpected overlapping lookup events");
        events.push(json!({"scope": scope, "stage": stage, "duration_ms": duration_ms, "elapsed_ms": elapsed_ms, "since_start_ms": since_start_ms}));
    }
    if DETAILED {
        let timestamp = |scope: &str, stage: &str| events.iter().find(|event| event["scope"] == scope && event["stage"] == stage).expect("profile boundary")["since_start_ms"].as_f64().unwrap();
        stages.insert("derived.actor_inbox_wait".to_owned(), timestamp("book_lookup_actor", "received") - timestamp("book_lookup_handle", "request_ready"));
        stages.insert("derived.queue_and_admission".to_owned(), timestamp("book_lookup_actor", "operation_start") - timestamp("book_lookup_handle", "request_ready"));
        stages.insert("derived.reply_delivery".to_owned(), timestamp("book_lookup_client", "actor_roundtrip") - timestamp("book_lookup_actor", "operation_complete"));
    }
    black_box(&resolved);
    json!({"total_ms": total_ms, "stages": stages, "events": events})
}

fn mark<const DETAILED: bool>(stages: &mut Stages, previous: &mut Option<Instant>, name: &'static str) {
    if DETAILED {
        let now = Instant::now();
        stages.insert(name, now.duration_since(previous.replace(now).unwrap()).as_secs_f64() * 1000.0);
    }
}

async fn sample<const DETAILED: bool>(library: &LibraryClient, hash: BookId) -> Value {
    let started = Instant::now();
    let mut previous = DETAILED.then_some(started);
    let mut stages = Stages::new();
    // The same LibraryClient/actor and async local-file source used by ReaderView.
    let resolved = library.resolve_book(hash).await.expect("resolve local book");
    mark::<DETAILED>(&mut stages, &mut previous, "resolve_local_source_ms");
    let format = match resolved.format() {
        library_backend::BookFormat::Epub => BookFormat::Epub,
        library_backend::BookFormat::Mobi => BookFormat::Mobi,
        library_backend::BookFormat::Fb2 => BookFormat::Fb2,
        _ => panic!("use an EPUB, MOBI or FB2 book"),
    };
    let source = resolved.into_document().expect("document").source;
    let (reading, mut stages, mut previous) = tokio::task::spawn_blocking(move || {
        mark::<DETAILED>(&mut stages, &mut previous, "dispatch_to_worker_ms");
        let book = match source {
            DocumentSource::Async(source) => block_on(Book::open(format, move || source.reader())),
            DocumentSource::Reader(reader) => Book::open_reader(format, reader),
        }
        .expect("open book container");
        mark::<DETAILED>(&mut stages, &mut previous, "container_open_ms");
        let reading = block_on(book.prepare_reading()).expect("prepare reading resources");
        mark::<DETAILED>(&mut stages, &mut previous, "prepare_reading_ms");
        // ReaderView consults language metadata when choosing CJK fallback.
        black_box(reading.languages());
        mark::<DETAILED>(&mut stages, &mut previous, "language_metadata_ms");
        (reading, stages, previous)
    })
    .await
    .expect("book preparation worker");
    mark::<DETAILED>(&mut stages, &mut previous, "return_from_worker_ms");
    black_box(library.reading_position(hash).await.expect("reading position"));
    mark::<DETAILED>(&mut stages, &mut previous, "reading_position_ms");
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    // Inspect results after stopping the timer; keep the prepared book alive
    // through completion so destruction is not included in opening time.
    assert!(!reading.document_uris().is_empty());
    json!({"total_ms": total_ms, "stages": stages, "documents": reading.document_uris().len(),
           "spine_bytes": reading.document_weights.iter().sum::<u64>()})
}

fn summarize(samples: &[Value], field: &str) -> Value {
    let mut values: Vec<f64> = samples.iter().map(|sample| if field == "total_ms" { sample[field].as_f64().unwrap() } else { sample["stages"][field].as_f64().unwrap() }).collect();
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = if n % 2 == 0 { (values[n / 2 - 1] + values[n / 2]) / 2.0 } else { values[n / 2] };
    json!({"median_ms": median, "min_ms": values[0], "max_ms": values[n - 1]})
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().expect("BOOK.epub OUTPUT_DIRECTORY [RUNS]"));
    let output = PathBuf::from(args.next().expect("output directory"));
    let runs: usize = args.next().map(|n| n.to_string_lossy().parse().expect("positive run count")).unwrap_or(10);
    let lookup_only = args
        .next()
        .map(|arg| {
            assert_eq!(arg, "--lookup-only");
            true
        })
        .unwrap_or(false);
    assert!(runs > 0 && args.next().is_none());
    if lookup_only {
        log::set_logger(&LOOKUP_LOGGER).expect("install buffered profiler");
        log::set_max_level(log::LevelFilter::Debug);
    }
    fs::create_dir_all(&output).unwrap();
    let output = output.canonicalize().unwrap();
    let directory = output.join(format!("isolated-library-{}", std::process::id()));
    assert!(!directory.exists(), "use a fresh isolated directory");
    let _maintenance = app::host::defer_library_maintenance();
    let launch = BackendLaunch::initialize(AppDataLocation::native_path(&directory)).expect("isolated backend").with_private_library_storage();
    let (backend, _) = AppClient::start_native(launch).expect("start backend");
    let library = backend.library_for_import(None).await.expect("private library");
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let bytes = fs::read(&path).expect("read fixture");
    let file_bytes = bytes.len();
    let hash = library.import_downloaded_book(ROOT_DIR_ID, name, bytes).await.expect("import local fixture");
    let mut results = json!({"book": path, "file_bytes": file_bytes, "runs": runs,
        "platform": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "debug_assertions": cfg!(debug_assertions), "preparation_executor": "Tokio blocking pool",
        "lookup_only": lookup_only,
        "lookup_profile_notes": "Nested/overlapping scopes; durations have 0.001 ms precision. Detailed timings include in-memory log formatting; baseline disables event recording.",
        "conditions": "Desktop release; isolated private local library; warm filesystem cache after import; startup, import and HTML engine excluded; first sample excluded from summary; background maintenance deferred",
        "phases": {}});
    let phases =
        if lookup_only { vec![("cached_overall", false, false), ("cached_breakdown", true, false), ("fresh_overall", false, true), ("fresh_breakdown", true, true)] } else { vec![("overall", false, false), ("breakdown", true, false)] };
    for (phase, detailed, fresh) in phases {
        let mut samples = Vec::new();
        for index in 0..=runs {
            let fresh_library = fresh.then(|| backend.library(*library.id()));
            let sample_library = fresh_library.as_ref().unwrap_or(&library);
            let record = if lookup_only {
                if detailed { lookup_sample::<true>(sample_library, hash).await } else { lookup_sample::<false>(sample_library, hash).await }
            } else if detailed {
                sample::<true>(&library, hash).await
            } else {
                sample::<false>(&library, hash).await
            };
            println!("{phase} {index}: {:.3} ms", record["total_ms"].as_f64().unwrap());
            samples.push(record);
        }
        let mut summary = json!({"total": summarize(&samples[1..], "total_ms")});
        if detailed {
            for stage in samples[0]["stages"].as_object().unwrap().keys() {
                summary[stage] = summarize(&samples[1..], stage);
            }
        }
        results["phases"][phase] = json!({"first": samples[0], "samples": &samples[1..], "summary": summary});
        fs::write(output.join("results.json"), serde_json::to_vec_pretty(&results).unwrap()).unwrap();
        println!("{phase} summary: {summary}");
    }
}
