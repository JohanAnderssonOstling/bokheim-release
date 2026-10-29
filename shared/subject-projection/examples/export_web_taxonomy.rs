//! Package the client database so browser startup does no route construction.
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use subject_projection::installed;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(std::env::args_os().nth(1).expect("supply the web bundle directory"));
    export(&directory)
}

fn export(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let path = directory.join("taxonomy.sqlite3");
    std::fs::copy(concat!(env!("OUT_DIR"), "/unified-taxonomy-client-v2.sqlite3"), &path)?;
    installed::prepare(&path)?;
    let db = Connection::open(&path)?;
    db.execute("INSERT INTO taxonomy_meta(key,value) VALUES('source',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [installed::bundled_source()])?;
    db.execute_batch("VACUUM;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use subject_projection::BUNDLED_UNIFIED_TAXONOMY_REVISION_ID;

    #[test]
    fn exported_database_is_ready_for_browser_startup() {
        let directory = tempfile::tempdir().unwrap();
        export(directory.path()).unwrap();
        let path = directory.path().join("taxonomy.sqlite3");
        let stored = installed::load_metadata(&path).unwrap();
        let bundled = installed::StoredMetadata::bundled();
        assert_eq!(stored.source, bundled.source);
        assert_eq!(stored.release, bundled.release);
        assert_eq!(stored.format, bundled.format);
        let db = Connection::open(path).unwrap();
        let revision: String = db.query_row("SELECT value FROM taxonomy_meta WHERE key='client_revision'", [], |row| row.get(0)).unwrap();
        assert_eq!(revision, BUNDLED_UNIFIED_TAXONOMY_REVISION_ID);
        assert!(db.query_row("SELECT count(*) FROM unified_concept_route", [], |row| row.get::<_, i64>(0)).unwrap() > 0);
        assert_eq!(db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)).unwrap(), "ok");
    }
}
