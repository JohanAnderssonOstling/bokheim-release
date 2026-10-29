//! Immutable, approved snapshot selection. Hosts call activate once, before workers.
//! Every host selects an immutable file. SQLite pages and classification
//! matchers are loaded on demand using the same policy on every platform.
use rusqlite::{Connection, OpenFlags};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

static ACTIVE: OnceLock<PathBuf> = OnceLock::new();
pub const BUNDLED_RELEASE_ID: u64 = include!(concat!(env!("OUT_DIR"), "/taxonomy-release.rs"));
pub const PAGE_CACHE_KIB: i64 = 512;

/// Identifies the prepared reference packaged with this application build.
pub fn bundled_source() -> String {
    format!("bundled:file-v1:{}", include_str!(concat!(env!("OUT_DIR"), "/taxonomy.digest")))
}

pub fn active_path() -> Option<&'static Path> {
    ACTIVE.get().map(PathBuf::as_path)
}

pub fn path() -> Result<PathBuf, String> {
    if let Some(path) = active_path() {
        return Ok(path.to_owned());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::bundled::path()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err("no taxonomy has been installed".into())
    }
}

/// Use the same read-only, immutable SQLite attachment on every host.
pub fn read_only_uri(path: &Path) -> Result<String, String> {
    let path = path.to_str().ok_or("taxonomy path is not UTF-8")?;
    let mut encoded = String::with_capacity(path.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
    Ok(format!("file:{encoded}?mode=ro&immutable=1"))
}

pub fn open_read_only(path: &Path) -> Result<Connection, String> {
    let db = Connection::open_with_flags(read_only_uri(path)?, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI).map_err(|e| e.to_string())?;
    db.pragma_update(None, "cache_size", -PAGE_CACHE_KIB).map_err(|e| e.to_string())?;
    Ok(db)
}

pub fn metadata(path: &Path) -> Result<(u64, u32), String> {
    let db = open_read_only(path)?;
    metadata_in(&db)
}

fn metadata_in(db: &Connection) -> Result<(u64, u32), String> {
    let read = |key| -> Result<String, String> { db.query_row("SELECT value FROM taxonomy_meta WHERE key=?1", [key], |r| r.get(0)).map_err(|e| e.to_string()) };
    Ok((read("release_id")?.parse::<u64>().map_err(|e| e.to_string())?, read("format_version")?.parse::<u32>().map_err(|e| e.to_string())?))
}

/// Transform a private copy of a verified server snapshot into the client image.
/// Never called on the signed source or the currently active image.
#[cfg(not(target_arch = "wasm32"))]
pub fn prepare(path: &Path) -> Result<(), String> {
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    prepare_in(&db)?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;").map_err(|e| e.to_string())?;
    Ok(())
}

fn prepare_in(db: &Connection) -> Result<(), String> {
    let concepts = crate::read_unified_taxonomy_connection(db, "taxonomy snapshot")?;
    let matcher = crate::UnifiedTaxonomy::from_concepts(concepts)?;
    db.execute_batch("DROP VIEW IF EXISTS unified_concept_paths").map_err(|e| e.to_string())?;
    db.execute_batch(include_str!("route_storage.sql")).map_err(|e| e.to_string())?;
    db.execute("INSERT INTO taxonomy_meta(key,value) VALUES('client_revision',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [matcher.revision_id()]).map_err(|e| e.to_string())?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS client_matcher(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL, image BLOB NOT NULL);").map_err(|e| e.to_string())?;
    db.execute("INSERT OR REPLACE INTO client_matcher(singleton,version,image) VALUES(1,?1,?2)", rusqlite::params![crate::UNIFIED_MATCHER_VERSION, matcher.encode()?]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn activate(path: &Path) -> Result<(), String> {
    #[cfg(not(target_arch = "wasm32"))]
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    let path = path.to_owned();
    if let Some(previous) = ACTIVE.get() {
        return if previous == &path { Ok(()) } else { Err("taxonomy can only change at process startup".into()) };
    }
    // Hosts have already selected the reference. Open SQLite only when a
    // consumer queries it, rather than reading metadata again at activation.
    ACTIVE.set(path).map_err(|_| "taxonomy already selected".to_string())
}

/// The client image of a verified server snapshot, checked against the
/// release and format the signed manifest names. `source` records where the
/// snapshot came from, for workers that load it themselves.
pub fn prepare_image(snapshot: &[u8], expected: (u64, u32), source: &str) -> Result<Vec<u8>, String> {
    let db = open_snapshot(snapshot)?;
    if metadata_in(&db)? != expected {
        return Err("taxonomy version differs from signed manifest".into());
    }
    prepare_in(&db)?;
    db.execute("INSERT INTO taxonomy_meta(key,value) VALUES('source',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [source]).map_err(|e| e.to_string())?;
    Ok(db.serialize("main").map_err(|e| e.to_string())?.to_vec())
}

/// Installs a verified server snapshot's concepts for matching only, in a
/// worker that never attaches the taxonomy to a library.
pub fn install_snapshot(snapshot: &[u8]) -> Result<(), String> {
    crate::runtime::install_from_connection(&open_snapshot(snapshot)?)
}

fn open_snapshot(snapshot: &[u8]) -> Result<Connection, String> {
    let mut snapshot = snapshot.to_vec();
    // A snapshot saved in WAL mode cannot be deserialized; mark it as a
    // rollback-journal database, which is what it is once in memory.
    if snapshot.len() > 19 && snapshot[18] == 2 {
        snapshot[18] = 1;
        snapshot[19] = 1;
    }
    let mut db = Connection::open_in_memory().map_err(|e| e.to_string())?;
    db.deserialize_read_exact("main", snapshot.as_slice(), snapshot.len(), false).map_err(|e| e.to_string())?;
    Ok(db)
}

/// Replaces the database at `path` with `image` in one transaction.
pub fn store_image(image: &[u8], path: &Path) -> Result<(), String> {
    let mut source = Connection::open_in_memory().map_err(|e| e.to_string())?;
    source.deserialize_read_exact("main", image, image.len(), true).map_err(|e| e.to_string())?;
    let mut destination = Connection::open(path).map_err(|e| e.to_string())?;
    let backup = rusqlite::backup::Backup::new(&source, &mut destination).map_err(|e| e.to_string())?;
    match backup.step(-1).map_err(|e| e.to_string())? {
        rusqlite::backup::StepResult::Done => Ok(()),
        _ => Err("taxonomy cache is busy".into()),
    }
}

/// Small reference metadata; reading it never copies the database image.
pub struct StoredMetadata {
    pub release: u64,
    pub format: u32,
    pub source: String,
}

impl StoredMetadata {
    /// The packaged database's metadata is fixed by the application build.
    pub fn bundled() -> Self {
        Self { release: BUNDLED_RELEASE_ID, format: include!(concat!(env!("OUT_DIR"), "/taxonomy-format.rs")), source: bundled_source() }
    }
}

pub fn load_metadata(path: &Path) -> Result<StoredMetadata, String> {
    let db = open_read_only(path)?;
    let (release, format) = metadata_in(&db)?;
    let source = db.query_row("SELECT value FROM taxonomy_meta WHERE key='source'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    Ok(StoredMetadata { release, format, source })
}
