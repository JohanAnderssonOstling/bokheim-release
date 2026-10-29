//! Indexed logical asset names.

pub(super) fn initialize(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "synchronous", "FULL")?;
    let transaction = conn.unchecked_transaction()?;
    transaction.execute_batch(include_str!("sql/asset_index/initialize_pragma.sql"))?;
    let compatible: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('asset_revision') WHERE name='content_hash' AND upper(type)='BLOB')", [], |row| row.get(0))?;
    if !compatible {
        return Err(rusqlite::Error::InvalidQuery);
    }
    transaction.commit()
}

pub(super) fn publish(conn: &rusqlite::Connection, name: &str, physical: &str, length: u64) -> rusqlite::Result<()> {
    conn.execute(include_str!("sql/asset_index/publish_insert.sql"), rusqlite::params![name, physical, length as i64])?;
    Ok(())
}

/// Immutable generations record their length when published; totals never open files.
pub(super) fn total_bytes(conn: &rusqlite::Connection, prefix: &str) -> rusqlite::Result<u64> {
    let upper = format!("{}0", prefix.strip_suffix('/').ok_or(rusqlite::Error::InvalidQuery)?);
    conn.query_row(include_str!("sql/asset_index/total_bytes_select.sql"), rusqlite::params![prefix, upper], |row| row.get::<_, i64>(0)).map(|bytes| bytes as u64)
}

/// The physical directory is removed first. Clear its logical names and
/// revision metadata in one transaction so a retry after interruption is safe.
pub(super) fn purge_library(conn: &rusqlite::Connection, library: &str) -> rusqlite::Result<()> {
    let logical = format!("assets/{library}/");
    let logical_upper = format!("assets/{library}0");
    let physical = format!("__libraries/{library}/");
    let physical_upper = format!("__libraries/{library}0");
    conn.execute("DELETE FROM asset_file WHERE name>=?1 AND name<?2", rusqlite::params![logical, logical_upper])?;
    conn.execute("DELETE FROM retained_provider_upload WHERE name>=?1 AND name<?2", rusqlite::params![logical, logical_upper])?;
    conn.execute("DELETE FROM asset_revision WHERE physical_name>=?1 AND physical_name<?2", rusqlite::params![physical, physical_upper])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_hash_columns_are_rejected_without_rewriting_data() {
        for column in ["checksum TEXT", "content_hash TEXT"] {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!("CREATE TABLE asset_revision(physical_name TEXT PRIMARY KEY, {column} NOT NULL);")).unwrap();
            let hash = "ab".repeat(32);
            conn.execute("INSERT INTO asset_revision VALUES('book',?1)", [&hash]).unwrap();
            assert!(initialize(&conn).is_err());
            assert!(!conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='reference_selection')", [], |row| row.get::<_, bool>(0)).unwrap());
            let stored: String = conn.query_row("SELECT * FROM asset_revision", [], |row| row.get(1)).unwrap();
            assert_eq!(stored, hash);
        }
    }

    #[test]
    fn publishing_records_replacement_and_preserves_existing_files() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        publish(&conn, "assets/a/book/existing", "keep", 2).unwrap();
        publish(&conn, "assets/a/book/new", "first", 3).unwrap();
        publish(&conn, "assets/a/book/new", "replacement", 5).unwrap();
        initialize(&conn).unwrap();
        assert_eq!(conn.query_row(include_str!("sql/asset_index/publishing_records_replacement_and_preserves_existing_files_select.sql"), [], |r| r.get::<_, String>(0)).unwrap(), "replacement");
        assert_eq!(conn.query_row(include_str!("sql/asset_index/publishing_records_replacement_and_preserves_existing_files_select_2.sql"), [], |r| r.get::<_, String>(0)).unwrap(), "keep");
        assert_eq!(total_bytes(&conn, "assets/a/book/").unwrap(), 7);
    }

    #[test]
    fn discarding_an_obsolete_download_keeps_the_current_published_generation() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let name = "assets/library/book/provider";
        publish(&conn, name, "old-download", 5).unwrap();
        publish(&conn, name, "new-download", 9).unwrap();
        let discard = include_str!("sql/web_storage/discard_publication.sql");
        assert_eq!(conn.execute(discard, rusqlite::params![name, "old-download"]).unwrap(), 0);
        assert_eq!(conn.query_row("SELECT physical_name FROM asset_file WHERE name=?1", [name], |row| row.get::<_, String>(0)).unwrap(), "new-download");
        assert_eq!(total_bytes(&conn, "assets/library/book/").unwrap(), 9);
        assert_eq!(conn.execute(discard, rusqlite::params![name, "new-download"]).unwrap(), 1);
        assert!(!conn.query_row("SELECT EXISTS(SELECT 1 FROM asset_file WHERE name=?1)", [name], |row| row.get::<_, bool>(0)).unwrap());
    }

    #[test]
    fn purging_one_library_preserves_other_library_assets() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        for (name, physical) in [("assets/a/book/first", "__libraries/a/book/one"), ("assets/a/thumbnail/first", "__libraries/a/thumbnail/two"), ("assets/ab/book/second", "__libraries/ab/book/three")] {
            publish(&conn, name, physical, 10).unwrap();
            conn.execute("INSERT INTO asset_revision(physical_name,content_hash) VALUES(?1,'content_hash')", [physical]).unwrap();
        }

        for name in ["assets/a/book/pending-upload", "assets/ab/book/pending-upload"] {
            conn.execute("INSERT INTO retained_provider_upload(name,metadata) VALUES(?1,?2)", rusqlite::params![name,b"manifest"]).unwrap();
        }
        purge_library(&conn, "a").unwrap();
        assert_eq!(conn.query_row("SELECT name FROM retained_provider_upload", [], |row| row.get::<_,String>(0)).unwrap(), "assets/ab/book/pending-upload");
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM asset_file", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT name FROM asset_file", [], |row| row.get::<_, String>(0)).unwrap(), "assets/ab/book/second");
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM asset_revision", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    }
}
