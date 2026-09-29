//! Immutable, approved snapshot selection. Hosts call activate once, before workers.
use rusqlite::{Connection, OpenFlags};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

static ACTIVE: OnceLock<PathBuf> = OnceLock::new();
pub fn active_path() -> Option<&'static Path> {
    ACTIVE.get().map(PathBuf::as_path)
}

pub fn metadata(path: &Path) -> Result<(u64, u32), String> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let read = |key| -> Result<String, String> { db.query_row("SELECT value FROM taxonomy_meta WHERE key=?1", [key], |r| r.get(0)).map_err(|e| e.to_string()) };
    Ok((read("release_id")?.parse::<u64>().map_err(|e| e.to_string())?, read("format_version")?.parse::<u32>().map_err(|e| e.to_string())?))
}

/// Transform a private copy of a verified server snapshot into the client image.
/// Never called on the signed source or the currently active image.
pub fn prepare(path: &Path) -> Result<(), String> {
    let concepts = crate::read_unified_taxonomy_sqlite(path)?;
    let matcher = crate::UnifiedTaxonomy::from_concepts(concepts)?;
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.execute_batch("DROP VIEW IF EXISTS unified_concept_paths").map_err(|e| e.to_string())?;
    db.execute_batch(include_str!("route_storage.sql")).map_err(|e| e.to_string())?;
    db.execute("INSERT INTO taxonomy_meta(key,value) VALUES('client_revision',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [matcher.revision_id()]).map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;").map_err(|e| e.to_string())?;
    Ok(())
}

pub fn activate(path: &Path) -> Result<(), String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if let Some(previous) = ACTIVE.get() {
        return if previous == &path { Ok(()) } else { Err("taxonomy can only change at process startup".into()) };
    }
    let concepts = crate::read_unified_taxonomy_sqlite(&path)?;
    crate::install_unified_taxonomy_concepts(concepts)?;
    ACTIVE.set(path).map_err(|_| "taxonomy already selected".to_string())
}
