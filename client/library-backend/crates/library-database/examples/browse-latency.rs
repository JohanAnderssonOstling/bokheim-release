//! Run with two SQLite paths and optional --profile. Inputs are copied from
//! SQLite snapshots; the benchmark migrates and writes only its own copies.
use client_runtime::wire::{decode_worker_message, encode_worker_message};
use library_database::{Database, LibraryBrowseData, catalog};
use library_model::{BrowseBookPage, BrowseBookSort, BrowseChipSort, BrowseLocation, BrowsePageRequest, LibraryBrowseQuery};
use rusqlite::{Connection, OpenFlags};
use serde::{Serialize, de::DeserializeOwned};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use sync_common::ROOT_DIR_ID;
use uuid::Uuid;

static PROFILING: AtomicBool = AtomicBool::new(false);
static PLANS: AtomicBool = AtomicBool::new(false);
struct Profile;
impl log::Log for Profile {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        (metadata.target() == "browse_profile" || (metadata.target() == "browse_query_plan" && PLANS.load(Ordering::Relaxed))) && PROFILING.load(Ordering::Relaxed)
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}", record.args());
        }
    }
    fn flush(&self) {}
}
static PROFILE: Profile = Profile;

struct Snapshot(PathBuf);
impl Snapshot {
    fn new(catalog: &Path, library: &Path) -> Self {
        let directory = std::env::temp_dir().join(format!("browse-latency-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        for (source, name) in [(catalog, "catalog.sqlite3"), (library, "library.db")] {
            let connection = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            std::fs::write(directory.join(name), &*connection.serialize("main").unwrap()).unwrap();
        }
        Self(directory)
    }
}
impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

trait Rows: Serialize + DeserializeOwned {
    fn rows(&self) -> (usize, usize);
}
impl Rows for LibraryBrowseData {
    fn rows(&self) -> (usize, usize) {
        (self.contents.children.len(), self.contents.books.len() + self.sections.iter().map(|section| section.books.len()).sum::<usize>())
    }
}
impl Rows for BrowseBookPage {
    fn rows(&self) -> (usize, usize) {
        (0, self.books.len())
    }
}

fn measure<T: Rows>(case: String, profile: bool, mut run: impl FnMut() -> T) -> T {
    PROFILING.store(profile, Ordering::Relaxed);
    if profile {
        eprintln!("{case}");
    }
    let start = Instant::now();
    let first = run();
    let first_ms = start.elapsed().as_secs_f64() * 1000.0;
    PROFILING.store(false, Ordering::Relaxed);
    let mut times = Vec::new();
    let mut encoding = Vec::new();
    let mut wire_encoding = Vec::new();
    let mut wire_decoding = Vec::new();
    let wire = encode_worker_message(&first).unwrap();
    let decoded: T = decode_worker_message(&wire).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), serde_json::to_value(&first).unwrap());
    for _ in 0..30 {
        let start = Instant::now();
        black_box(run());
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        black_box(serde_json::to_vec(&first).unwrap());
        encoding.push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        black_box(encode_worker_message(&first).unwrap());
        wire_encoding.push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        black_box(decode_worker_message::<T>(&wire).unwrap());
        wire_decoding.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    encoding.sort_by(f64::total_cmp);
    wire_encoding.sort_by(f64::total_cmp);
    wire_decoding.sort_by(f64::total_cmp);
    let (headings, cards) = first.rows();
    println!(
        "{}",
        serde_json::json!({"case":case,"first_ms":first_ms,"median_ms":(times[14]+times[15])/2.0,"p95_ms":times[28],"json_serialization_median_ms":(encoding[14]+encoding[15])/2.0,"json_serialization_p95_ms":encoding[28],"returned_headings":headings,"card_occurrences":cards,"json_bytes":serde_json::to_vec(&first).unwrap().len(),"wire_bytes":wire.len(),"wire_encode_median_ms":(wire_encoding[14]+wire_encoding[15])/2.0,"wire_decode_median_ms":(wire_decoding[14]+wire_decoding[15])/2.0})
    );
    first
}

fn query(location: BrowseLocation) -> LibraryBrowseQuery {
    LibraryBrowseQuery { location, search: String::new(), file_types: vec![], languages: vec![], chip_sort: BrowseChipSort::DefaultOrder, book_sort: BrowseBookSort::DefaultOrder, hide_finished: false }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert!(args.len() >= 2, "usage: browse-latency CATALOG_SQLITE LIBRARY_SQLITE [--profile]");
    let plans = args.iter().any(|arg| arg == "--plans");
    PLANS.store(plans, Ordering::Relaxed);
    let profile = plans || args.iter().any(|arg| arg == "--profile");
    log::set_logger(&PROFILE).unwrap();
    log::set_max_level(log::LevelFilter::Debug);
    let snapshot = Snapshot::new(Path::new(&args[0]), Path::new(&args[1]));
    catalog::activate_path(&snapshot.0.join("catalog.sqlite3")).unwrap();
    let library = snapshot.0.join("library.db");
    let discovery = Database::open(&library).unwrap();
    discovery.initialize_library().unwrap();
    let catalog_counts = Connection::open_with_flags(snapshot.0.join("catalog.sqlite3"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    println!(
        "{}",
        serde_json::json!({"case":"dataset","catalog_books":catalog_counts.query_row("SELECT count(*) FROM catalog_browse_search", [], |row| row.get::<_, i64>(0)).unwrap(),"catalog_authors":catalog_counts.query_row("SELECT count(*) FROM author", [], |row| row.get::<_, i64>(0)).unwrap(),"library_visible_books":discovery.book_count().unwrap()})
    );
    drop(catalog_counts);
    let root = discovery.browse_page(&query(BrowseLocation::Catalog(0)), false, BrowsePageRequest::default()).unwrap();
    let biggest = root.contents.children.iter().max_by_key(|row| row.book_count).map(|row| row.id);
    let mut locations = vec![BrowseLocation::Catalog(0), BrowseLocation::CatalogAuthor(None), BrowseLocation::Books, BrowseLocation::Subject(0), BrowseLocation::Author(None), BrowseLocation::Folder(ROOT_DIR_ID), BrowseLocation::Home(None)];
    if let Some(biggest) = biggest {
        locations.insert(1, biggest);
    }
    for location in locations {
        // A new session has no result, ordering or facet cache. SQLite and OS
        // caches are uncontrolled; first_ms is preparation, not cold-disk I/O.
        let db = Database::open(&library).unwrap();
        let query = query(location);
        let first = measure(format!("{location}:initial"), profile, || db.browse_page(&query, false, BrowsePageRequest::default()).unwrap());
        let state = first.page.as_ref().unwrap();
        let revision = state.result_revision.as_ref().unwrap();
        if state.heading_count() > 24 || state.book_total > 48 {
            measure(format!("{location}:next_range"), profile, || db.browse_result_page(&query, false, revision, BrowsePageRequest { section_offset: 24, section_limit: 24, book_offset: 48, book_limit: 48 }).unwrap().unwrap());
        }
        if let Some(section) = first.sections.iter().filter(|section| section.book_count.unwrap_or_default() > 8).max_by_key(|section| section.book_count) {
            measure(format!("{location}:section_arrow"), profile, || db.browse_result_books(&query, false, revision, section.parent.id, 8, 8).unwrap().unwrap());
        }
        if let Some(author) = first.contents.children.iter().find(|row| matches!(row.id, BrowseLocation::Author(Some(_)) | BrowseLocation::CatalogAuthor(Some(_)))) {
            let author_query = self::query(author.id);
            measure(format!("{}:individual_author", author.id), profile, || db.browse_page(&author_query, false, BrowsePageRequest::default()).unwrap());
        }
    }
    let db = Database::open(&library).unwrap();
    let query = query(BrowseLocation::Books);
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    let writer = Connection::open(&library).unwrap();
    writer.execute("UPDATE library_local_state SET scan_state='pending'", []).unwrap();
    let bookkeeping_keeps_revision = db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_some();
    writer.execute("UPDATE book SET title=coalesce(title,'') || ' changed' WHERE row_id=(SELECT min(row_id) FROM book)", []).unwrap();
    let metadata_rejects_revision = db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_none();
    assert!(bookkeeping_keeps_revision && metadata_rejects_revision);
    println!("{}", serde_json::json!({"case":"revision_checks","bookkeeping_keeps_revision":bookkeeping_keeps_revision,"metadata_rejects_revision":metadata_rejects_revision}));
}
