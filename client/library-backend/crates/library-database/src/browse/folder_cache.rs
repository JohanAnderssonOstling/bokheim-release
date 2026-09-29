//! Batch folder membership maintenance; triggers only record invalidation.
use crate::{Database, DatabaseError, transactions::WriteOutcome};
use include_sqlite_sql::include_sql;
use std::collections::{HashMap, HashSet};
include_sql!("src/browse/sql/folder_cache.sql");

fn cache_state(conn: &rusqlite::Connection) -> Result<(bool, bool), DatabaseError> {
    let mut state = (false, false);
    conn.folder_cache_status(|row| {
        state = (row.get(0)?, row.get(1)?);
        Ok(())
    })?;
    Ok(state)
}

#[derive(Default)]
struct Ownership {
    placements: i64,
    downloads: i64,
    visible: i64,
    visible_downloads: i64,
    source: String,
}

impl Ownership {
    fn record_placement(&mut self, placement: &Placement, visible: bool) {
        self.placements += 1;
        self.downloads += i64::from(placement.downloaded);
        self.visible += i64::from(visible);
        self.visible_downloads += i64::from(visible && placement.downloaded);
        if self.source.is_empty() || placement.directory < self.source {
            self.source.clone_from(&placement.directory);
        }
    }
}

struct Placement {
    book: i64,
    directory: String,
    downloaded: bool,
}

struct Directory {
    parent: String,
    hidden: bool,
}

struct Ancestor {
    directory: String,
    visible: bool,
}

/// Resolves each placement directory once per refresh, including missing ancestors.
struct AncestryResolver<'a> {
    connection: &'a rusqlite::Connection,
    preloaded: bool,
    directories: HashMap<String, Option<Directory>>,
    chains: HashMap<String, Vec<Ancestor>>,
}

impl<'a> AncestryResolver<'a> {
    fn new(connection: &'a rusqlite::Connection, preload: bool) -> Result<Self, DatabaseError> {
        let mut resolver = Self { connection, preloaded: preload, directories: HashMap::new(), chains: HashMap::new() };
        if preload {
            connection.folder_cache_directories(|row| {
                resolver.directories.insert(row.get(0)?, Some(Directory { parent: row.get(1)?, hidden: row.get::<_, String>(2)?.starts_with('.') }));
                Ok(())
            })?;
        }
        Ok(resolver)
    }

    fn resolve(&mut self, directory: &str) -> Result<&[Ancestor], DatabaseError> {
        if !self.chains.contains_key(directory) {
            let chain = self.walk(directory)?;
            self.chains.insert(directory.to_owned(), chain);
        }
        Ok(&self.chains[directory])
    }

    fn walk(&mut self, directory: &str) -> Result<Vec<Ancestor>, DatabaseError> {
        let mut chain = Vec::new();
        let mut current = directory.to_owned();
        let mut visible = true;
        let mut seen = HashSet::new();
        loop {
            if !self.preloaded && !self.directories.contains_key(&current) {
                let mut found = None;
                self.connection.folder_cache_directory(&current, |row| {
                    found = Some(Directory { parent: row.get(0)?, hidden: row.get::<_, String>(1)?.starts_with('.') });
                    Ok(())
                })?;
                self.directories.insert(current.clone(), found);
            }
            let Some(Some(directory)) = self.directories.get(&current) else { break };
            if !seen.insert(current.clone()) {
                return Err(DatabaseError::message("cycle in folder ancestry"));
            }
            chain.push(Ancestor { directory: current.clone(), visible });
            if directory.parent == current {
                break;
            }
            // A hidden directory is visible from itself, but not from its parent.
            visible &= !directory.hidden;
            current = directory.parent.clone();
        }
        Ok(chain)
    }
}

impl Database {
    pub(super) fn with_folder_cache<T>(&self, operation: impl FnOnce() -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
        self.with_cached_browse_snapshot(|| self.ensure_folder_cache(), |connection| Ok(cache_state(connection)? == (false, false)), operation)
    }

    fn ensure_folder_cache(&self) -> Result<(), DatabaseError> {
        if cache_state(&self.connection)? == (false, false) {
            return Ok(());
        }
        self.with_write_transaction(|tx| {
            let (rebuild, dirty) = cache_state(tx)?;
            if !rebuild && !dirty {
                return Ok(WriteOutcome::Commit(()));
            }
            if rebuild {
                tx.folder_cache_rebuild()?;
            }
            tx.folder_cache_remove_dirty()?;
            let mut ancestry = AncestryResolver::new(tx, rebuild)?;
            let mut memberships = HashMap::<(String, i64), Ownership>::new();
            let mut placements = Vec::new();
            tx.folder_cache_placements(|row| {
                placements.push(Placement { book: row.get(0)?, directory: row.get(1)?, downloaded: row.get(2)? });
                Ok(())
            })?;
            for placement in placements {
                for ancestor in ancestry.resolve(&placement.directory)? {
                    memberships.entry((ancestor.directory.clone(), placement.book)).or_default().record_placement(&placement, ancestor.visible);
                }
            }
            for ((folder, book), count) in memberships {
                tx.folder_cache_insert(&folder, book, count.placements, count.downloads, count.visible, count.visible_downloads, &count.source)?;
            }
            tx.folder_cache_clean()?;
            Ok(WriteOutcome::Commit(()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sync_common::ROOT_DIR_ID;

    #[test]
    fn full_and_lazy_ancestry_agree_on_visibility_missing_parents_and_cycles() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE dir(id TEXT PRIMARY KEY, parent_id TEXT, name TEXT, deleted_at INTEGER);
             INSERT INTO dir VALUES
             ('root','root','Library',NULL),
             ('hidden','root','.Hidden',NULL),
             ('leaf','hidden','Leaf',NULL),
             ('deleted','root','Deleted',1),
             ('orphan','deleted','Orphan',NULL),
             ('missing','absent','Missing parent',NULL),
             ('cycle-a','cycle-b','A',NULL),
             ('cycle-b','cycle-a','B',NULL);",
            )
            .unwrap();
        for preload in [false, true] {
            let mut resolver = AncestryResolver::new(&connection, preload).unwrap();
            for (start, expected) in [
                ("leaf", vec![("leaf", true), ("hidden", true), ("root", false)]),
                ("hidden", vec![("hidden", true), ("root", false)]),
                ("root", vec![("root", true)]),
                ("orphan", vec![("orphan", true)]),
                ("missing", vec![("missing", true)]),
                ("deleted", vec![]),
                ("absent", vec![]),
            ] {
                // Repeated placements reuse the resolved chain.
                for _ in 0..2 {
                    let actual = resolver.resolve(start).unwrap().iter().map(|ancestor| (ancestor.directory.as_str(), ancestor.visible)).collect::<Vec<_>>();
                    assert_eq!(actual, expected, "preload={preload}, start={start}");
                }
            }
            assert!(resolver.resolve("cycle-a").is_err());
            assert!(resolver.resolve("cycle-b").is_err());
        }
    }

    #[test]
    fn refresh_releases_writer_and_browse_keeps_one_snapshot() {
        struct TestDirectory(std::path::PathBuf);
        impl Drop for TestDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = TestDirectory(std::env::temp_dir().join(format!("folder-snapshot-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&root.0).unwrap();
        let path = root.0.join("library.sqlite3");
        let reader = Database::open(&path).unwrap();
        reader.initialize_library().unwrap();
        let writer = Database::open(&path).unwrap();
        writer.initialize_library().unwrap();
        reader
            .with_folder_cache(|| {
                assert_eq!(cache_state(&reader.connection)?, (false, false));
                // WAL writes succeed during browsing, but cannot change its snapshot.
                let directory = writer.create_directory(&ROOT_DIR_ID, &".Pending".into())?;
                // Creation alone leaves membership clean; a directory update
                // exercises invalidation while the old snapshot remains open.
                writer.connection.execute("UPDATE dir SET name='Concurrent' WHERE id=?1", [directory.id.to_string()])?;
                assert_eq!(cache_state(&reader.connection)?, (false, false));
                let found: bool = reader.connection.query_row("SELECT EXISTS(SELECT 1 FROM dir WHERE name='Concurrent')", [], |r| r.get(0))?;
                assert!(!found);
                Ok(())
            })
            .unwrap();
        assert_eq!(cache_state(&reader.connection).unwrap(), (true, false));
        reader
            .with_folder_cache(|| {
                assert_eq!(cache_state(&reader.connection)?, (false, false));
                let found: bool = reader.connection.query_row("SELECT EXISTS(SELECT 1 FROM dir WHERE name='Concurrent')", [], |r| r.get(0))?;
                assert!(found);
                Ok(())
            })
            .unwrap();
    }
}
