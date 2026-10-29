//! One file-backed, bounded-cache reference connection per process/worker.
#[cfg(not(target_arch = "wasm32"))]
use flate2::read::GzDecoder;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    fs::OpenOptions,
    io::{ErrorKind, Read, Write},
    sync::atomic::{AtomicU64, Ordering},
};

pub const BISAC_REFERENCE_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/bisac-reference-digest.txt"));
pub const BISAC_REFERENCE_GZIP_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/bisac-reference-gzip-digest.txt"));
#[cfg(not(target_arch = "wasm32"))]
const IMAGE_SIZE: usize = include!(concat!(env!("OUT_DIR"), "/bisac-reference-size.rs"));
#[cfg(not(target_arch = "wasm32"))]
const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bisac-reference.sqlite3.gz"));
static REFERENCE: OnceLock<Mutex<Option<Connection>>> = OnceLock::new();

fn reference() -> MutexGuard<'static, Option<Connection>> {
    let mutex = REFERENCE.get_or_init(|| Mutex::new(None));
    mutex.lock().unwrap_or_else(|error| {
        let mut reference = error.into_inner();
        *reference = None;
        mutex.clear_poison();
        eprintln!("Recovering the BISAC reference after an interrupted query");
        reference
    })
}

pub fn bisac_reference_filename() -> String {
    format!("bisac-reference-{BISAC_REFERENCE_DIGEST}.sqlite3")
}

fn database_path() -> PathBuf {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::temp_dir().join(format!("bokheim-{}", bisac_reference_filename()))
    }
    #[cfg(target_arch = "wasm32")]
    {
        bisac_reference_filename().into()
    }
}

fn open_file(path: &Path) -> Result<Connection, String> {
    let db = crate::installed::open_read_only(path)?;
    db.set_prepared_statement_cache_capacity(8);
    db.execute_batch("PRAGMA cache_size=-128; PRAGMA mmap_size=0;").map_err(|e| e.to_string())?;
    Ok(db)
}

fn matches_revision(db: &Connection) -> rusqlite::Result<bool> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='reference_meta')", [], |row| row.get(0))?;
    if !exists {
        return Ok(false);
    }
    Ok(db.query_row("SELECT revision FROM reference_meta", [], |row| row.get::<_, String>(0)).optional()?.as_deref() == Some(BISAC_REFERENCE_DIGEST))
}

/// Install a prepared application asset through the platform's bulk writer.
/// Cache identity, connection policy and reuse are common to every host.
pub fn install_bisac_reference(image: &[u8], write: impl FnOnce(&Path, &[u8]) -> Result<(), String>) -> Result<(), String> {
    let mut reference = reference();
    if reference.is_none() {
        *reference = Some(ensure_file(|path| write(path, image))?);
    }
    Ok(())
}

fn cached_file(path: &Path) -> Option<Connection> {
    let db = open_file(path).ok()?;
    matches_revision(&db).ok()?.then_some(db)
}

fn ensure_file(install: impl FnOnce(&Path) -> Result<(), String>) -> Result<Connection, String> {
    let path = database_path();
    if let Some(db) = cached_file(&path) {
        return Ok(db);
    }
    install(&path)?;
    cached_file(&path).ok_or_else(|| "installed BISAC reference does not match this application".into())
}

pub fn open_bisac_reference() -> Result<bool, String> {
    let mut reference = reference();
    if reference.is_none() {
        *reference = cached_file(&database_path());
    }
    Ok(reference.is_some())
}

#[cfg(not(target_arch = "wasm32"))]
fn install_bundled_file(path: &Path) -> Result<(), String> {
    let mut image = Vec::with_capacity(IMAGE_SIZE);
    GzDecoder::new(IMAGE).read_to_end(&mut image).map_err(|e| e.to_string())?;
    write_native_file(path, &image)
}

#[cfg(target_arch = "wasm32")]
fn install_bundled_file(_: &Path) -> Result<(), String> {
    Err("the browser host must download the BISAC application asset".into())
}

#[cfg(not(target_arch = "wasm32"))]
fn write_native_file(path: &Path, image: &[u8]) -> Result<(), String> {
    // A completed file becomes visible in one rename. Separate processes can
    // prepare the same cache without exposing a partial image to readers.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (staging, mut file) = loop {
        let staging = path.with_extension(format!("{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match OpenOptions::new().write(true).create_new(true).open(&staging) {
            Ok(file) => break (staging, file),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    };
    let result = (|| {
        file.write_all(image)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&staging, path)
    })();
    let _ = std::fs::remove_file(&staging);
    if result.is_err() && cached_file(path).is_some() {
        // Another process may have published the same immutable cache first.
        return Ok(());
    }
    result.map_err(|e| e.to_string())
}

pub(super) fn with_reference<T>(read: impl FnOnce(Option<&Connection>) -> T) -> T {
    let mut reference = reference();
    if reference.is_none() {
        match ensure_file(install_bundled_file) {
            Ok(db) => *reference = Some(db),
            Err(error) => eprintln!("BISAC storage is unavailable; using compiled reference nodes: {error}"),
        }
    }
    read(reference.as_ref())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn bundled_gzip_and_web_image_are_identical_and_ready_to_open() {
        let raw = include_bytes!(concat!(env!("OUT_DIR"), "/bisac-reference.sqlite3"));
        let mut decoded = Vec::new();
        GzDecoder::new(IMAGE).read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, raw);
        assert_eq!(decoded.len(), IMAGE_SIZE);
        let file = tempfile::NamedTempFile::new().unwrap();
        write_native_file(file.path(), &decoded).unwrap();
        let db = open_file(file.path()).unwrap();
        assert!(matches_revision(&db).unwrap());
        assert_eq!(db.pragma_query_value(None, "cache_size", |row| row.get::<_, i64>(0)).unwrap(), -128);
        assert!(db.execute("UPDATE reference_meta SET revision='changed'", []).is_err());
    }

    #[test]
    fn publishing_failure_keeps_the_existing_destination_intact() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("reference.sqlite3");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("preserved"), b"keep").unwrap();
        assert!(write_native_file(&destination, b"replacement").is_err());
        assert_eq!(std::fs::read(destination.join("preserved")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn persisted_reference_tracks_the_complete_bundle_artifact() {
        let file = tempfile::NamedTempFile::new().unwrap();
        install_bundled_file(file.path()).unwrap();
        let db = Connection::open(file.path()).unwrap();
        assert!(matches_revision(&db).unwrap());
        db.execute("UPDATE reference_meta SET revision='obsolete artifact'", []).unwrap();
        assert!(!matches_revision(&db).unwrap());
    }
}
