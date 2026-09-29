//! Durable subject routes and membership, incrementally refreshed under the main writer.
use super::*;
use crate::transactions::WriteOutcome;

#[derive(Default)]
struct RawSubjectNode {
    members: BTreeSet<i64>,
    direct: BTreeSet<i64>,
    children: BTreeSet<i64>,
    parent: i64,
}

struct ProjectedSubjectNode {
    route: i64,
    direct: BTreeSet<i64>,
    children: Vec<ProjectedSubjectNode>,
}

fn cache_state(conn: &rusqlite::Connection) -> Result<(bool, bool), DatabaseError> {
    let mut state = (false, false);
    conn.subject_cache_status(&crate::taxonomy_update::revision(conn)?, |row| {
        state = (row.get(0)?, row.get(1)?);
        Ok(())
    })?;
    Ok(state)
}

impl Database {
    /// Keep card reads and membership reads on the same clean cache snapshot.
    /// If another writer changes membership before the snapshot starts, refresh
    /// and retry; once it starts, WAL readers do not hold the main writer.
    pub(in crate::browse) fn with_subject_cache<T>(&self, operation: impl FnOnce() -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
        self.with_cached_browse_snapshot(|| self.ensure_subject_cache(), |connection| Ok(cache_state(connection)? == (false, false)), operation)
    }

    pub(crate) fn ensure_subject_cache(&self) -> Result<(), DatabaseError> {
        if cache_state(&self.connection)? == (false, false) {
            return Ok(());
        }
        self.with_write_transaction(|_| Ok(WriteOutcome::Commit(())))
    }

    /// Called by the writer before commit. Only top-level branches reached by
    /// changed books are projected again, from the unchanged source assignments.
    pub(crate) fn refresh_subject_cache_in_transaction(&self, tx: &rusqlite::Transaction<'_>) -> Result<(), DatabaseError> {
        let (rebuild, dirty) = cache_state(tx)?;
        if !rebuild && !dirty {
            return Ok(());
        }
        let mut books = Vec::new();
        if rebuild {
            tx.subject_cache_rebuild()?;
        }
        tx.subject_cache_dirty_books(|row| {
            books.push(row.get::<_, i64>(0)?);
            Ok(())
        })?;
        let mut branches = BTreeSet::new();
        for book in books {
            if !rebuild {
                let mut old_routes = tx.prepare("SELECT route_id FROM subject_browse_member WHERE book_row_id=?1 AND direct=1")?;
                for route in old_routes.query_map([book], |row| row.get::<_, i64>(0))? {
                    if let Some(branch) = top_branch(tx, route?)? {
                        branches.insert(branch);
                    }
                }
            }
            tx.subject_cache_remove_book(book)?;
            let mut routes = Vec::new();
            tx.book_routes(book, |row| {
                routes.push((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?));
                Ok(())
            })?;
            if routes.is_empty() {
                continue;
            }
            for (route, direct) in &routes {
                if *direct != 0 {
                    if let Some(branch) = top_branch(tx, *route)? {
                        branches.insert(branch);
                    }
                }
                tx.subject_cache_insert_route(*route)?;
                tx.subject_cache_insert_member(*route, book, *direct)?;
            }
            // Route zero represents the synthetic Subject root.
            tx.subject_cache_insert_route(0)?;
            tx.subject_cache_insert_member(0, book, 0)?;
        }
        tx.subject_cache_prune()?;
        for branch in branches {
            project_branch(tx, branch)?;
        }
        tx.subject_cache_clean()?;
        tx.subject_cache_revision(&crate::taxonomy_update::revision(tx)?)?;
        Ok(())
    }
}

fn top_branch(tx: &rusqlite::Transaction<'_>, route: i64) -> Result<Option<i64>, DatabaseError> {
    if route == 0 {
        return Ok(None);
    }
    use rusqlite::OptionalExtension;
    Ok(tx
        .query_row(
            "WITH RECURSIVE ancestors(route_id,parent_route_id) AS (
             SELECT route_id,parent_route_id FROM curated.unified_concept_route WHERE route_id=?1
             UNION ALL
             SELECT parent.route_id,parent.parent_route_id FROM ancestors child
             JOIN curated.unified_concept_route parent ON parent.route_id=child.parent_route_id
         ) SELECT route_id FROM ancestors WHERE parent_route_id=0 LIMIT 1",
            [route],
            |row| row.get(0),
        )
        .optional()?)
}

fn project_branch(tx: &rusqlite::Transaction<'_>, branch: i64) -> Result<(), DatabaseError> {
    tx.execute("DELETE FROM subject_browse_placement WHERE source_branch_id=?1", [branch])?;
    tx.execute(
        "WITH RECURSIVE routes(route_id) AS (
             SELECT ?1 UNION ALL SELECT child.route_id FROM curated.unified_concept_route child
             JOIN routes parent ON child.parent_route_id=parent.route_id
         ) DELETE FROM subject_browse_visible_route WHERE route_id IN (SELECT route_id FROM routes)",
        [branch],
    )?;
    let mut nodes = BTreeMap::<i64, RawSubjectNode>::new();
    {
        let mut statement = tx.prepare(
            "WITH RECURSIVE routes(route_id) AS (
                 SELECT ?1 UNION ALL SELECT child.route_id FROM curated.unified_concept_route child
                 JOIN routes parent ON child.parent_route_id=parent.route_id
                 JOIN library_subject used ON used.route_id=child.route_id
             ) SELECT route.route_id,taxonomy.parent_route_id,member.book_row_id,member.direct
               FROM routes route JOIN curated.unified_concept_route taxonomy USING(route_id)
               JOIN subject_browse_member member USING(route_id)",
        )?;
        let rows = statement.query_map([branch], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, bool>(3)?)))?;
        for row in rows {
            let (route, parent, book, direct) = row?;
            let node = nodes.entry(route).or_default();
            node.parent = parent;
            node.members.insert(book);
            if direct {
                node.direct.insert(book);
            }
        }
    }
    let edges = nodes.iter().map(|(&route, node)| (route, node.parent)).collect::<Vec<_>>();
    for (route, parent) in edges {
        if let Some(node) = nodes.get_mut(&parent) {
            node.children.insert(route);
        }
    }
    if nodes.contains_key(&branch) {
        if let Some(projected) = project_node(branch, &nodes) {
            store_projected_node(tx, &projected, 0, branch)?;
        } else {
            for book in &nodes[&branch].members {
                tx.execute("INSERT INTO subject_browse_placement(route_id,book_row_id,source_branch_id) VALUES(0,?1,?2)", rusqlite::params![book, branch])?;
            }
        }
    }
    Ok(())
}

fn project_node(route: i64, nodes: &BTreeMap<i64, RawSubjectNode>) -> Option<ProjectedSubjectNode> {
    let node = &nodes[&route];
    let mut direct = node.direct.clone();
    let mut children = Vec::new();
    for child in &node.children {
        if let Some(projected) = project_node(*child, nodes) {
            children.push(projected);
        } else {
            direct.extend(&nodes[child].members);
        }
    }
    // First collapse small leaves. Then absorb the sole child of an empty
    // wrapper, keeping this route's label and promoting the child's contents.
    if children.is_empty() && node.members.len() <= 2 {
        return None;
    }
    if direct.is_empty() && children.len() == 1 {
        let child = children.pop().expect("one projected child");
        direct = child.direct;
        children = child.children;
    }
    Some(ProjectedSubjectNode { route, direct, children })
}

fn store_projected_node(tx: &rusqlite::Transaction<'_>, node: &ProjectedSubjectNode, parent: i64, branch: i64) -> Result<(), DatabaseError> {
    tx.execute("INSERT INTO subject_browse_visible_route(route_id,parent_route_id) VALUES(?1,?2)", rusqlite::params![node.route, parent])?;
    for book in &node.direct {
        tx.execute("INSERT INTO subject_browse_placement(route_id,book_row_id,source_branch_id) VALUES(?1,?2,?3)", rusqlite::params![node.route, book, branch])?;
    }
    for child in &node.children {
        store_projected_node(tx, child, node.route, branch)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_time_compaction_restores_and_recascades_after_assignment_changes() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        // This fixture owns derived rows directly; do not enqueue canonical edits.
        db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
        db.connection
            .execute_batch(
                "DETACH curated; ATTACH ':memory:' AS curated;
            CREATE TABLE curated.taxonomy_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE curated.concept(concept_id INTEGER PRIMARY KEY,preferred_label TEXT NOT NULL);
            INSERT INTO curated.concept VALUES(1,'Transportation'),(2,'Land Transport'),(3,'Automotive'),(4,'Railroads'),(5,'Marine Transport');
            CREATE TABLE curated.unified_concept_route(route_id INTEGER PRIMARY KEY,concept_id INTEGER NOT NULL,parent_route_id INTEGER NOT NULL);
            INSERT INTO curated.unified_concept_route VALUES(1,1,0),(2,2,1),(3,3,2),(4,4,2),(5,5,1);
        ",
            )
            .unwrap();
        let insert = |first: i64, last: i64| {
            db.with_write_transaction(|tx| {
                for id in first..=last {
                    tx.execute("INSERT INTO book(content_hash,title,format) VALUES(printf('%064x',?1),printf('Book %d',?1),'epub')", [id])?;
                    tx.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,?2,?3,'',1)", rusqlite::params![sync_common::ROOT_DIR_ID.to_string(), id, format!("book{id}.epub")])?;
                    let concept = match id {
                        1 | 4 | 5 => 3,
                        2 => 4,
                        3 => 5,
                        _ => unreachable!(),
                    };
                    tx.execute("INSERT INTO book_unified_concept(book_row_id,concept_id,mapper_version) VALUES(?1,?2,1)", [id, concept])?;
                }
                Ok(WriteOutcome::Commit(()))
            })
            .unwrap();
        };
        let owners = || {
            let mut statement = db.connection.prepare("SELECT route_id,book_row_id FROM subject_browse_placement ORDER BY route_id,book_row_id").unwrap();
            statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        let visible = || {
            let mut statement = db.connection.prepare("SELECT route_id FROM subject_browse_visible_route ORDER BY route_id").unwrap();
            statement.query_map([], |row| row.get::<_, i64>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        insert(1, 2);
        assert_eq!(visible(), Vec::<i64>::new());
        assert_eq!(owners(), vec![(0, 1), (0, 2)]);
        insert(3, 3);
        assert_eq!(visible(), vec![1]);
        assert_eq!(owners(), vec![(1, 1), (1, 2), (1, 3)]);
        let query = |location: &str| library_model::LibraryBrowseQuery {
            location: location.to_owned(),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: library_model::BrowseChipSort::Alphabetical,
            book_sort: library_model::BrowseBookSort::Alphabetical,
            hide_finished: false,
            include_direct_child_books: false,
        };
        let writes = db.connection.total_changes();
        let root = db.browse_subject_contents(&query("Subject"), false).unwrap();
        assert_eq!(root.children.iter().map(|child| child.name.as_str()).collect::<Vec<_>>(), ["Transportation"]);
        let transportation = db.browse_subject_contents(&query("Transportation"), false).unwrap();
        assert!(transportation.children.is_empty());
        assert_eq!(transportation.books.len(), 3);
        assert_eq!(db.connection.total_changes(), writes, "browsing must not perform compaction writes");
        insert(4, 5);
        assert_eq!(visible(), vec![1, 2, 3]);
        assert_eq!(owners(), vec![(1, 3), (2, 2), (3, 1), (3, 4), (3, 5)]);
        db.with_write_transaction(|tx| {
            tx.execute("DELETE FROM book_unified_concept WHERE book_row_id IN (4,5)", [])?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        assert_eq!(visible(), vec![1]);
        assert_eq!(owners(), vec![(1, 1), (1, 2), (1, 3)]);
        db.with_write_transaction(|tx| {
            tx.execute("DELETE FROM book_unified_concept WHERE book_row_id=3", [])?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        assert_eq!(visible(), Vec::<i64>::new());
        assert_eq!(owners(), vec![(0, 1), (0, 2)]);
        assert_eq!(db.connection.query_row("SELECT concept_id FROM book_unified_concept WHERE book_row_id=1", [], |row| row.get::<_, i64>(0)).unwrap(), 3);
    }

    #[test]
    fn empty_single_child_wrapper_absorbs_and_restores_grandchildren() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        // This fixture owns derived rows directly; do not enqueue canonical edits.
        db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
        db.connection
            .execute_batch(
                "DETACH curated; ATTACH ':memory:' AS curated;
            CREATE TABLE curated.taxonomy_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE curated.concept(concept_id INTEGER PRIMARY KEY,preferred_label TEXT NOT NULL);
            INSERT INTO curated.concept VALUES(1,'Science'),(2,'Physics'),(3,'Quantum'),(4,'Optics');
            CREATE TABLE curated.unified_concept_route(route_id INTEGER PRIMARY KEY,concept_id INTEGER NOT NULL,parent_route_id INTEGER NOT NULL);
            INSERT INTO curated.unified_concept_route VALUES(1,1,0),(2,2,1),(3,3,2),(4,4,2);
        ",
            )
            .unwrap();
        let insert = |range: std::ops::RangeInclusive<i64>| {
            db.with_write_transaction(|tx| {
                for id in range {
                    tx.execute("INSERT INTO book(content_hash,title,format) VALUES(printf('%064x',?1),printf('Book %d',?1),'epub')", [id])?;
                    tx.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,?2,?3,'',1)", rusqlite::params![sync_common::ROOT_DIR_ID.to_string(), id, format!("book{id}.epub")])?;
                    let concept = if id <= 3 {
                        3
                    } else if id <= 6 {
                        4
                    } else {
                        1
                    };
                    tx.execute("INSERT INTO book_unified_concept(book_row_id,concept_id,mapper_version) VALUES(?1,?2,1)", [id, concept])?;
                }
                Ok(WriteOutcome::Commit(()))
            })
            .unwrap();
        };
        let visible = || {
            let mut statement = db.connection.prepare("SELECT route_id,parent_route_id FROM subject_browse_visible_route ORDER BY route_id").unwrap();
            statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        insert(1..=6);
        assert_eq!(visible(), vec![(1, 0), (3, 1), (4, 1)]);
        let query = library_model::LibraryBrowseQuery {
            location: "Science".into(),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: library_model::BrowseChipSort::Alphabetical,
            book_sort: library_model::BrowseBookSort::Alphabetical,
            hide_finished: false,
            include_direct_child_books: false,
        };
        let contents = db.browse_subject_contents(&query, false).unwrap();
        assert_eq!(contents.children.iter().map(|child| child.name.as_str()).collect::<Vec<_>>(), ["Optics", "Quantum"]);
        assert!(contents.books.is_empty());
        insert(7..=7);
        assert_eq!(visible(), vec![(1, 0), (2, 1), (3, 2), (4, 2)]);
        db.with_write_transaction(|tx| {
            tx.execute("DELETE FROM book_unified_concept WHERE book_row_id=7", [])?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        assert_eq!(visible(), vec![(1, 0), (3, 1), (4, 1)]);
        assert_eq!(db.connection.query_row("SELECT concept_id FROM book_unified_concept WHERE book_row_id=1", [], |row| row.get::<_, i64>(0)).unwrap(), 3);
    }

}
