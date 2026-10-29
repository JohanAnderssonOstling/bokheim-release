//! Build and compact the displayed subject tree from current assignments.
pub(crate) use super::*;
use crate::browse::revision::Revision;
use rusqlite::{Connection, OptionalExtension};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct SubjectTrees {
    library: Option<CachedTree>,
    catalog: Option<CachedTree>,
}

struct CachedTree {
    revision: TreeRevision,
    nodes: Arc<BTreeMap<i64, SubjectNode>>,
}

#[derive(PartialEq, Eq)]
struct TreeRevision {
    visibility: i64,
    subjects: i64,
    legacy: Option<(u64, i64)>,
    taxonomy_revision: Option<String>,
    taxonomy_local_changes: Option<u64>,
    taxonomy_data_version: i64,
    taxonomy_schema_version: i64,
    catalog: Option<(Option<(i64, i64, i64, String)>, u64, i64)>,
}

impl Database {
    /// Both sources reuse the same unfiltered, compacted tree. Book filters
    /// and reading state belong to query results. Only visibility, assignment
    /// or taxonomy changes invalidate the library tree; catalog changes
    /// invalidate the catalog tree independently of library writes.
    pub(in crate::browse) fn subject_tree_for(&self, books: Books) -> Result<Arc<BTreeMap<i64, SubjectNode>>, DatabaseError> {
        match books {
            Books::Library => self.cached_subject_tree(books, None, || self.load_subject_tree()),
            Books::Catalog => crate::catalog::read(|catalog| self.catalog_subject_tree(catalog)),
        }
    }

    fn cached_subject_tree(
        &self, books: Books, catalog: Option<(Option<(i64, i64, i64, String)>, u64, i64)>, load: impl FnOnce() -> Result<BTreeMap<i64, SubjectNode>, DatabaseError>,
    ) -> Result<Arc<BTreeMap<i64, SubjectNode>>, DatabaseError> {
        let library = if matches!(books, Books::Library) { Revision::read(&self.connection)? } else { Revision::default() };
        let revision = TreeRevision {
            visibility: library.visibility,
            subjects: library.subjects,
            legacy: library.legacy,
            taxonomy_revision: crate::browse::revision::taxonomy(&self.connection)?,
            taxonomy_local_changes: (!self.connection.is_readonly("curated")?).then(|| self.connection.total_changes()),
            taxonomy_data_version: self.connection.pragma_query_value(Some("curated"), "data_version", |row| row.get(0))?,
            taxonomy_schema_version: self.connection.pragma_query_value(Some("curated"), "schema_version", |row| row.get(0))?,
            catalog,
        };
        {
            let trees = self.subject_trees.borrow();
            let cached = match books {
                Books::Library => &trees.library,
                Books::Catalog => &trees.catalog,
            };
            if let Some(cached) = cached.as_ref().filter(|cached| cached.revision == revision) {
                return Ok(Arc::clone(&cached.nodes));
            }
        }
        let nodes = Arc::new(load()?);
        let mut trees = self.subject_trees.borrow_mut();
        let cached = match books {
            Books::Library => &mut trees.library,
            Books::Catalog => &mut trees.catalog,
        };
        *cached = Some(CachedTree { revision, nodes: Arc::clone(&nodes) });
        Ok(nodes)
    }

    pub(super) fn load_subject_tree(&self) -> Result<BTreeMap<i64, SubjectNode>, DatabaseError> {
        let mut assignments = Vec::new();
        self.connection.subject_assignments(|row| {
            assignments.push((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?));
            Ok(())
        })?;
        self.subject_tree(assignments)
    }

    /// The catalog's persisted concept assignments, using the same taxonomy
    /// route lookup and tree compaction as personal-library assignments.
    pub(in crate::browse) fn catalog_subject_tree(&self, catalog: &Connection) -> Result<Arc<BTreeMap<i64, SubjectNode>>, DatabaseError> {
        let revision = catalog.query_row("SELECT version,release_id,format_version,taxonomy_revision FROM catalog_browse_revision WHERE singleton=1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
        self.cached_subject_tree(Books::Catalog, Some((revision, catalog.total_changes(), catalog.pragma_query_value(None, "data_version", |row| row.get(0))?)), || self.load_catalog_subject_tree(catalog))
    }

    fn load_catalog_subject_tree(&self, catalog: &Connection) -> Result<BTreeMap<i64, SubjectNode>, DatabaseError> {
        let mut statement = catalog.prepare("SELECT book_row_id,concept_id FROM catalog_browse_concept")?;
        let concepts = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        let selected = concepts.iter().map(|(_, concept)| *concept).collect::<BTreeSet<_>>();
        let mut routes = BTreeMap::<i64, Vec<i64>>::new();
        self.connection.concept_routes(&serde_json::to_string(&selected).map_err(DatabaseError::operation)?, |row| {
            routes.entry(row.get(0)?).or_default().push(row.get(1)?);
            Ok(())
        })?;
        let assignments = concepts.into_iter().flat_map(|(book, concept)| routes.get(&concept).into_iter().flatten().map(move |route| (book, *route))).collect();
        self.subject_tree(assignments)
    }

    /// The displayed tree for `(book, assigned route)` pairs.
    fn subject_tree(&self, assignments: Vec<(i64, i64)>) -> Result<BTreeMap<i64, SubjectNode>, DatabaseError> {
        let mut nodes = BTreeMap::from([(0, SubjectNode { label: "Subject".into(), ..Default::default() })]);
        for (book, assigned_route) in assignments {
            let mut current = assigned_route;
            while current != 0 {
                if !nodes.contains_key(&current) {
                    let (parent, label) = self.connection.taxonomy_route_parent(&current, |row| Ok((row.get(0)?, row.get(1)?)))?;
                    nodes.insert(current, SubjectNode { parent, label, ..Default::default() });
                }
                let node = subject_node_mut(&mut nodes, current)?;
                node.members.insert(book);
                if current == assigned_route {
                    node.direct.insert(book);
                }
                current = node.parent;
            }
            subject_node_mut(&mut nodes, 0)?.members.insert(book);
        }
        let edges = nodes.iter().filter(|(route, _)| **route != 0).map(|(&route, node)| (route, node.parent)).collect::<Vec<_>>();
        for (route, parent) in edges {
            subject_node_mut(&mut nodes, parent)?.children.insert(route);
        }
        let branches = subject_node(&nodes, 0)?.children.clone();
        for branch in branches {
            if !compact_node(branch, &mut nodes)? {
                let members = subject_node(&nodes, branch)?.members.clone();
                let root = subject_node_mut(&mut nodes, 0)?;
                root.direct.extend(members);
                root.children.remove(&branch);
            }
        }
        // Compacted-away nodes are no longer valid browse locations.
        let visible = subtree_routes(&nodes, 0);
        nodes.retain(|route, _| visible.contains(route));
        Ok(nodes)
    }
}

fn subject_node(nodes: &BTreeMap<i64, SubjectNode>, route: i64) -> Result<&SubjectNode, DatabaseError> {
    nodes.get(&route).ok_or_else(|| DatabaseError::message(format!("subject tree is missing route {route}")))
}

fn subject_node_mut(nodes: &mut BTreeMap<i64, SubjectNode>, route: i64) -> Result<&mut SubjectNode, DatabaseError> {
    nodes.get_mut(&route).ok_or_else(|| DatabaseError::message(format!("subject tree is missing route {route}")))
}

/// Collapse small leaves, then absorb a sole child of an empty wrapper while
/// keeping the wrapper's label. Membership is independent of page filters.
pub(in crate::browse) fn compact_node(route: i64, nodes: &mut BTreeMap<i64, SubjectNode>) -> Result<bool, DatabaseError> {
    let children = subject_node(nodes, route)?.children.clone();
    for child in children {
        if !compact_node(child, nodes)? {
            let members = subject_node(nodes, child)?.members.clone();
            let node = subject_node_mut(nodes, route)?;
            node.direct.extend(members);
            node.children.remove(&child);
        }
    }
    let node = subject_node(nodes, route)?;
    if node.children.is_empty() && node.members.len() <= 2 {
        return Ok(false);
    }
    if node.direct.is_empty() && node.children.len() == 1 {
        let child = *node.children.first().ok_or_else(|| DatabaseError::message("subject wrapper has no child"))?;
        let direct = subject_node(nodes, child)?.direct.clone();
        let grandchildren = subject_node(nodes, child)?.children.clone();
        for grandchild in &grandchildren {
            subject_node_mut(nodes, *grandchild)?.parent = route;
        }
        let node = subject_node_mut(nodes, route)?;
        node.direct = direct;
        node.children = grandchildren;
    }
    Ok(true)
}

pub(in crate::browse) fn subtree_routes(nodes: &BTreeMap<i64, SubjectNode>, root: i64) -> BTreeSet<i64> {
    let mut routes = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(route) = pending.pop() {
        if let Some(node) = nodes.get(&route)
            && routes.insert(route)
        {
            pending.extend(&node.children);
        }
    }
    routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactions::WriteOutcome;

    #[test]
    fn compaction_reports_missing_children_instead_of_panicking() {
        let mut nodes = BTreeMap::from([(1, SubjectNode { children: BTreeSet::from([2]), ..Default::default() })]);
        assert!(compact_node(1, &mut nodes).is_err());
        assert!(compact_node(99, &mut nodes).is_err());
    }

    #[test]
    fn leaf_assignments_count_upwards_once_without_combining_separate_branches() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.fixtures_detach_curated_concept_2().unwrap();
        let assignments = (1..=3).flat_map(|book| [(book, 4), (book, 5)]).chain((4..=6).map(|book| (book, 2))).collect();
        let tree = db.subject_tree(assignments).unwrap();
        assert_eq!(tree[&1].members, BTreeSet::from([1, 2, 3, 4, 5, 6]));
        assert_eq!(tree[&3].members, BTreeSet::from([1, 2, 3]));
        assert_eq!(tree[&0].members.len(), 6, "separate routes must not double-count books");
        assert_eq!(tree[&1].direct, BTreeSet::from([4, 5, 6]), "descendants count upwards without direct parent assignments");
        assert_eq!(tree[&4].direct, BTreeSet::from([1, 2, 3]));
    }

    #[test]
    fn library_and_catalog_reuse_the_same_tree_structure_and_invalidate_on_edits() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.fixtures_detach_curated_concept_2().unwrap();
        let catalog = Connection::open_in_memory().unwrap();
        catalog.execute_batch(include_str!("../../catalog/schema.sql")).unwrap();
        catalog.execute("INSERT INTO catalog_browse_revision VALUES(1,13,1,8,'test')", []).unwrap();
        for id in 1..=3 {
            db.connection.fixtures_insert_book_6(&id).unwrap();
            db.connection.fixtures_insert_book_dir(&(sync_common::ROOT_DIR_ID.to_string()), &id, &(format!("book{id}.epub"))).unwrap();
            db.connection.fixtures_insert_book_unified_concept_2(&id, &4).unwrap();
            catalog.execute("INSERT INTO catalog_browse_concept VALUES(?1,4)", [id]).unwrap();
        }
        let library = db.subject_tree_for(Books::Library).unwrap();
        let external = db.catalog_subject_tree(&catalog).unwrap();
        let structure = |nodes: &BTreeMap<i64, SubjectNode>| nodes.iter().map(|(&route, node)| (route, node.label.clone(), node.parent, node.children.clone(), node.members.clone(), node.direct.clone())).collect::<Vec<_>>();
        assert_eq!(structure(&library), structure(&external));
        assert!(Arc::ptr_eq(&library, &db.subject_tree_for(Books::Library).unwrap()));
        assert!(Arc::ptr_eq(&external, &db.catalog_subject_tree(&catalog).unwrap()));

        // Filters affect counts and cards, never the cached tree's compaction.
        let query = library_model::LibraryBrowseQuery {
            location: BrowseLocation::Subject(0),
            search: "science".into(),
            languages: vec!["sv".into()],
            file_types: vec![],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        db.browse(&query, false).unwrap();
        assert!(Arc::ptr_eq(&library, &db.subject_tree_for(Books::Library).unwrap()));

        db.connection.fixtures_update_curated_concept_preferred_label_science().unwrap();
        let renamed = db.subject_tree_for(Books::Library).unwrap();
        assert!(!Arc::ptr_eq(&library, &renamed));
        assert_eq!(renamed[&1].label, "Renamed science");
        let renamed_catalog = db.catalog_subject_tree(&catalog).unwrap();
        assert!(!Arc::ptr_eq(&external, &renamed_catalog));
        assert_eq!(structure(&renamed), structure(&renamed_catalog));

        catalog.execute_batch("DELETE FROM catalog_browse_concept WHERE book_row_id=3; UPDATE catalog_browse_revision SET release_id=2").unwrap();
        let updated = db.catalog_subject_tree(&catalog).unwrap();
        assert!(!Arc::ptr_eq(&renamed_catalog, &updated));
        assert!(updated[&0].children.is_empty(), "new releases must recompute compaction");
        assert_eq!(updated[&0].direct, BTreeSet::from([1, 2]));
        assert!(Arc::ptr_eq(&renamed, &db.subject_tree_for(Books::Library).unwrap()), "catalog updates do not rebuild the library tree");

        db.connection.fixtures_delete_book_dir().unwrap();
        let removed = db.subject_tree_for(Books::Library).unwrap();
        assert!(!Arc::ptr_eq(&renamed, &removed));
        assert_eq!(removed[&0].direct.len(), 2);
    }

    #[test]
    fn subject_tree_cache_observes_commits_from_another_library_connection() {
        let path = std::env::temp_dir().join(format!("subject-tree-cache-{}.sqlite3", uuid::Uuid::new_v4()));
        {
            let reader = Database::open(&path).unwrap();
            reader.initialize_library().unwrap();
            let writer = Database::open(&path).unwrap();
            writer.initialize_library().unwrap();
            let before = reader.with_browse_snapshot(|| reader.subject_tree_for(Books::Library)).unwrap();
            let changes = reader.connection.total_changes();
            writer.connection.fixtures_insert_book_6(&1).unwrap();
            writer.connection.fixtures_insert_book_dir(&(sync_common::ROOT_DIR_ID.to_string()), &1, &"book.epub").unwrap();
            let concept: i64 = writer.connection.query_row("SELECT concept_id FROM curated.unified_concept_route WHERE parent_route_id!=0 LIMIT 1", [], |row| row.get(0)).unwrap();
            writer.connection.fixtures_insert_book_unified_concept_2(&1, &concept).unwrap();
            let after = reader.with_browse_snapshot(|| reader.subject_tree_for(Books::Library)).unwrap();
            assert_eq!(reader.connection.total_changes(), changes);
            assert!(!Arc::ptr_eq(&before, &after));
            assert!(after[&0].members.contains(&1));
            assert!(Arc::ptr_eq(&after, &reader.with_browse_snapshot(|| reader.subject_tree_for(Books::Library)).unwrap()));
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_time_compaction_restores_and_recascades_after_assignment_changes() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        // This fixture owns derived rows directly; do not enqueue canonical edits.

        db.connection.fixtures_detach_curated_taxonomy_meta().unwrap();
        let insert = |first: i64, last: i64| {
            db.with_write_transaction(|tx| {
                for id in first..=last {
                    tx.fixtures_insert_book_8(&id)?;
                    tx.fixtures_insert_book_dir(&(sync_common::ROOT_DIR_ID.to_string()), &id, &(format!("book{id}.epub")))?;
                    let concept = match id {
                        1 | 4 | 5 => 3,
                        2 => 4,
                        3 => 5,
                        _ => unreachable!(),
                    };
                    tx.fixtures_insert_book_unified_concept_3(&id, &concept)?;
                }
                Ok(WriteOutcome::Commit(()))
            })
            .unwrap();
        };
        let owners = || db.load_subject_tree().unwrap().into_iter().flat_map(|(route, node)| node.direct.into_iter().map(move |book| (route, book))).collect::<Vec<_>>();
        let visible = || db.load_subject_tree().unwrap().into_keys().filter(|route| *route != 0).collect::<Vec<_>>();
        insert(1, 2);
        assert_eq!(visible(), Vec::<i64>::new());
        assert_eq!(owners(), vec![(0, 1), (0, 2)]);
        insert(3, 3);
        assert_eq!(visible(), vec![1]);
        assert_eq!(owners(), vec![(1, 1), (1, 2), (1, 3)]);
        let query = |route: i64| library_model::LibraryBrowseQuery {
            location: library_model::BrowseLocation::Subject(route),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: library_model::BrowseChipSort::Alphabetical,
            book_sort: library_model::BrowseBookSort::Alphabetical,
            hide_finished: false,
        };
        let writes = db.connection.total_changes();
        let root = db.browse_contents(&query(0), false).unwrap();
        assert_eq!(root.children.iter().map(|child| child.name.as_str()).collect::<Vec<_>>(), ["Transportation"]);
        let transportation = db.browse_contents(&query(1), false).unwrap();
        assert!(transportation.children.is_empty());
        assert_eq!(transportation.books.len(), 3);
        assert_eq!(db.connection.total_changes(), writes, "browsing must not perform compaction writes");
        insert(4, 5);
        assert_eq!(visible(), vec![1, 2, 3]);
        assert_eq!(owners(), vec![(1, 3), (2, 2), (3, 1), (3, 4), (3, 5)]);
        db.with_write_transaction(|tx| {
            tx.fixtures_delete_book_unified_concept()?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        assert_eq!(visible(), vec![1]);
        assert_eq!(owners(), vec![(1, 1), (1, 2), (1, 3)]);
        db.with_write_transaction(|tx| {
            tx.fixtures_delete_book_unified_concept_2()?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        assert_eq!(visible(), Vec::<i64>::new());
        assert_eq!(owners(), vec![(0, 1), (0, 2)]);
        assert_eq!(db.connection.fixtures_read_book_unified_concept_concept_id(|row| row.get::<_, i64>(0)).unwrap(), 3);
    }

    #[test]
    fn empty_single_child_wrapper_absorbs_and_restores_grandchildren() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        // This fixture owns derived rows directly; do not enqueue canonical edits.

        db.connection.fixtures_detach_curated_taxonomy_meta_2().unwrap();
        let insert = |range: std::ops::RangeInclusive<i64>| {
            db.with_write_transaction(|tx| {
                for id in range {
                    tx.fixtures_insert_book_8(&id)?;
                    tx.fixtures_insert_book_dir(&(sync_common::ROOT_DIR_ID.to_string()), &id, &(format!("book{id}.epub")))?;
                    let concept = if id <= 3 {
                        3
                    } else if id <= 6 {
                        4
                    } else {
                        1
                    };
                    tx.fixtures_insert_book_unified_concept_3(&id, &concept)?;
                }
                Ok(WriteOutcome::Commit(()))
            })
            .unwrap();
        };
        let visible = || db.load_subject_tree().unwrap().into_iter().filter(|(route, _)| *route != 0).map(|(route, node)| (route, node.parent)).collect::<Vec<_>>();
        insert(1..=6);
        assert_eq!(visible(), vec![(1, 0), (3, 1), (4, 1)]);
        let query = library_model::LibraryBrowseQuery {
            location: library_model::BrowseLocation::Subject(1),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: library_model::BrowseChipSort::Alphabetical,
            book_sort: library_model::BrowseBookSort::Alphabetical,
            hide_finished: false,
        };
        let contents = db.browse_contents(&query, false).unwrap();
        assert_eq!(contents.children.iter().map(|child| child.name.as_str()).collect::<Vec<_>>(), ["Optics", "Quantum"]);
        assert!(contents.books.is_empty());
        insert(7..=7);
        assert_eq!(visible(), vec![(1, 0), (2, 1), (3, 2), (4, 2)]);
        db.with_write_transaction(|tx| {
            tx.fixtures_delete_book_unified_concept_3()?;
            Ok(WriteOutcome::Commit(()))
        })
        .unwrap();
        assert_eq!(visible(), vec![(1, 0), (3, 1), (4, 1)]);
        assert_eq!(db.connection.fixtures_read_book_unified_concept_concept_id(|row| row.get::<_, i64>(0)).unwrap(), 3);
    }

    #[test]
    fn multiple_routes_filters_search_scope_and_taxonomy_edits_are_read_live() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.fixtures_detach_curated_concept_2().unwrap();
        for id in 1..=3 {
            db.connection.fixtures_insert_book_6(&id).unwrap();
            db.connection.fixtures_insert_book_dir_2(&(sync_common::ROOT_DIR_ID.to_string()), &id, &(format!("book{id}.epub")), &(i64::from(id == 1))).unwrap();
            db.connection.fixtures_insert_book_unified_concept_4(&id).unwrap();
            db.connection.fixtures_insert_book_language_2(&id, &(if id == 1 { "en" } else { "sv" })).unwrap();
        }
        db.connection.fixtures_insert_book_unified_concept_5().unwrap();
        let mut query = library_model::LibraryBrowseQuery {
            location: library_model::BrowseLocation::Subject(0),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        db.connection.pragma_update(None, "query_only", true).unwrap();
        let root = db.browse(&query, false).unwrap();
        assert_eq!(root.contents.children.iter().map(|row| (row.name.as_str(), row.book_count)).collect::<Vec<_>>(), [("Other", 3), ("Science", 3)]);
        assert_eq!(root.sections.len(), 2);
        query.search = "book".into();
        assert_eq!(db.browse_contents(&query, false).unwrap().books.len(), 3, "multiple routes must not duplicate cards");
        query.location = library_model::BrowseLocation::Subject(1);
        query.search = "Other".into();
        let searched = db.browse_contents(&query, false).unwrap();
        assert!(searched.books.is_empty() && searched.children.is_empty(), "search stays inside the selected subject");
        query.location = library_model::BrowseLocation::Subject(0);
        query.search.clear();
        query.languages = vec!["en".into()];
        let filtered = db.browse_contents(&query, false).unwrap();
        assert_eq!(filtered.children.iter().map(|row| (row.name.as_str(), row.book_count)).collect::<Vec<_>>(), [("Other", 1), ("Science", 1)], "filters must not change compaction");
        query.languages.clear();
        let downloaded = db.browse_contents(&query, true).unwrap();
        assert!(downloaded.children.iter().all(|row| row.book_count == 1 && row.downloaded_book_count == 1));
        db.connection.pragma_update(None, "query_only", false).unwrap();
        db.connection.fixtures_update_curated_concept_preferred_label_science().unwrap();
        assert!(db.browse_contents(&query, false).unwrap().children.iter().any(|row| row.name == "Renamed science"));
        db.connection.fixtures_delete_book_dir().unwrap();
        let collapsed = db.browse_contents(&query, false).unwrap();
        assert!(collapsed.children.is_empty());
        assert_eq!(collapsed.books.len(), 2);
        assert_eq!(db.connection.fixtures_read_book_unified_concept_count(|row| row.get::<_, i64>(0)).unwrap(), 4, "compaction never rewrites assignments");
    }
}

#[cfg(test)]
use crate::sql_fixtures::FixturesSql;
#[test]
fn reading_metadata_and_background_writes_preserve_structural_tree() {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    let concept: i64 = db.connection.query_row("SELECT concept_id FROM curated.unified_concept_route WHERE route_id!=0 LIMIT 1", [], |row| row.get(0)).unwrap();
    for id in 1..=3 {
        db.connection.fixtures_insert_book_6(&id).unwrap();
        db.connection.fixtures_insert_book_dir(&sync_common::ROOT_DIR_ID.to_string(), &id, &format!("book{id}.epub")).unwrap();
        db.connection.fixtures_insert_book_unified_concept_2(&id, &concept).unwrap();
    }
    let original = db.subject_tree_for(Books::Library).unwrap();
    db.connection.execute("UPDATE book SET title='Updated',read_progress=0.5,read_at=100,popularity_rating_count=12", []).unwrap();
    db.connection.execute("UPDATE library_local_state SET scan_state='pending'", []).unwrap();
    db.connection.execute("UPDATE book_dir SET local_hash='new fingerprint',is_downloaded=1", []).unwrap();
    assert!(Arc::ptr_eq(&original, &db.subject_tree_for(Books::Library).unwrap()));
    db.connection.execute("DELETE FROM book_dir WHERE book_row_id=1", []).unwrap();
    assert!(!Arc::ptr_eq(&original, &db.subject_tree_for(Books::Library).unwrap()));
}
