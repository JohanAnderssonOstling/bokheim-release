use super::cache::{MAX_CACHED_BYTES, MAX_RESULTS};
use super::*;
use crate::sql_fixtures::FixturesSql;
use library_model::{BrowseBookSort, BrowseChipSort};
use rusqlite::params;
use uuid::Uuid;

fn fixture() -> Database {
    let db = Database::open(":memory:").unwrap();
    populate(&db);
    db
}

fn populate(db: &Database) {
    db.initialize_library().unwrap();
    db.connection.fixtures_detach_curated_concept_2().unwrap();
    for id in 1..=37 {
        db.connection.execute("INSERT INTO author_identity(id,stable_id,name) VALUES(?1,?2,?3)", params![id, Uuid::from_u128(id as u128).to_string(), format!("Writer {id:02}")]).unwrap();
        db.connection.fixtures_insert_book_6(&id).unwrap();
        db.connection.fixtures_insert_book_dir_2(&sync_common::ROOT_DIR_ID.to_string(), &id, &format!("book{id}.epub"), &(id % 2)).unwrap();
        db.connection.fixtures_insert_book_unified_concept_2(&id, &4).unwrap();
        db.connection.execute("INSERT INTO book_contributor(book_row_id,position,author_identity_id,name,role) VALUES(?1,0,?1,?2,?3)", params![id, format!("Writer {id:02}"), b"aut".as_slice()]).unwrap();
    }
}

fn query(location: BrowseLocation) -> LibraryBrowseQuery {
    LibraryBrowseQuery { location, search: String::new(), languages: vec![], file_types: vec![], chip_sort: BrowseChipSort::Alphabetical, book_sort: BrowseBookSort::Alphabetical, hide_finished: false }
}

#[test]
fn heading_and_card_continuations_reuse_the_same_prepared_selection() {
    let db = fixture();
    for location in [BrowseLocation::Author(None), BrowseLocation::Subject(0), BrowseLocation::Books] {
        let query = query(location);
        let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
        let revision = first.page.as_ref().unwrap().result_revision.as_ref().unwrap();
        let prepared = db.browse_results.borrow().preparations;
        let complete = db.legacy_browse_sections(&query, false, BROWSE_SECTION_BOOK_LIMIT).unwrap();
        let next = db.browse_result_page(&query, false, revision, BrowsePageRequest { section_offset: 24, book_offset: 24, ..Default::default() }).unwrap().unwrap();
        assert_eq!(next.page.as_ref().unwrap().result_revision.as_ref(), Some(revision));
        if location == BrowseLocation::Author(None) {
            assert_eq!(next.contents.children, complete.contents.children[24..]);
        }
        if location == BrowseLocation::Books {
            assert_eq!(next.contents.books, complete.contents.books[24..]);
        }
        for section in &first.sections {
            let full = complete.sections.iter().find(|full| full.parent.id == section.parent.id).unwrap();
            let page = db.browse_result_books(&query, false, revision, section.parent.id, 0, 2).unwrap().unwrap();
            assert_eq!(page.books, full.books[..full.books.len().min(2)]);
            let cached = db.browse_result_books(&query, false, revision, section.parent.id, 0, 2).unwrap().unwrap();
            assert_eq!(cached, page);
        }
        assert_eq!(db.browse_results.borrow().preparations, prepared, "page reads must not redo filtering, totals or ordering");
    }
}

#[test]
fn book_metadata_changes_reject_old_pages_even_when_counts_and_headings_match() {
    let db = fixture();
    let query = query(BrowseLocation::Author(None));
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    let section = first.sections[0].parent.id;
    db.connection.execute("UPDATE book SET title='Changed title' WHERE row_id=1", []).unwrap();
    assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_none());
    assert!(db.browse_result_books(&query, false, &revision, section, 0, 2).unwrap().is_none());
    let fresh = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    assert_ne!(fresh.page.unwrap().result_revision.unwrap(), revision);
    assert_eq!(fresh.sections[0].books[0].title, "Changed title");
}

#[test]
fn normalized_queries_reuse_results_but_changed_filters_and_foreign_sections_do_not() {
    let db = fixture();
    let mut query = query(BrowseLocation::Books);
    query.search = "BOOK".into();
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    let prepared = db.browse_results.borrow().preparations;
    query.search = "book".into();
    assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_some());
    assert_eq!(db.browse_results.borrow().preparations, prepared);
    assert!(db.browse_result_books(&query, false, &revision, BrowseLocation::Author(Some(Uuid::nil())), 0, 2).is_err());
    query.hide_finished = true;
    assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_none());
}

#[test]
fn grouping_edits_only_expire_results_that_depend_on_them() {
    let db = fixture();
    let queries = [query(BrowseLocation::Books), query(BrowseLocation::Author(None)), query(BrowseLocation::Folder(sync_common::ROOT_DIR_ID))];
    let revisions = queries.each_ref().map(|query| db.browse_page(query, false, BrowsePageRequest::default()).unwrap().page.unwrap().result_revision.unwrap());

    db.connection.execute("DELETE FROM book_unified_concept", []).unwrap();
    for (query, revision) in queries.iter().zip(&revisions) {
        assert!(db.browse_result_page(query, false, revision, BrowsePageRequest::default()).unwrap().is_some(), "subject edits should preserve {location}", location = query.location);
    }

    db.connection.execute("UPDATE author_identity SET stable_id=?1 WHERE id=1", [Uuid::from_u128(999).to_string()]).unwrap();
    assert!(db.browse_result_page(&queries[0], false, &revisions[0], BrowsePageRequest::default()).unwrap().is_some());
    assert!(db.browse_result_page(&queries[1], false, &revisions[1], BrowsePageRequest::default()).unwrap().is_none());
    assert!(db.browse_result_page(&queries[2], false, &revisions[2], BrowsePageRequest::default()).unwrap().is_some());

    db.connection.execute("INSERT INTO dir(id,parent_id,name) VALUES(?1,?2,'New folder')", params![Uuid::from_u128(999).to_string(), sync_common::ROOT_DIR_ID.to_string()]).unwrap();
    assert!(db.browse_result_page(&queries[0], false, &revisions[0], BrowsePageRequest::default()).unwrap().is_some());
    assert!(db.browse_result_page(&queries[2], false, &revisions[2], BrowsePageRequest::default()).unwrap().is_none());
}

#[test]
fn eviction_rebuilds_an_unchanged_revision_and_limits_retained_results() {
    let db = fixture();
    let root = query(BrowseLocation::Books);
    let first = db.browse_page(&root, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    for index in 0..MAX_RESULTS + 3 {
        let mut distinct = root.clone();
        distinct.search = format!("missing {index}");
        db.browse_page(&distinct, false, BrowsePageRequest::default()).unwrap();
    }
    assert!(db.browse_results.borrow().entries.len() <= MAX_RESULTS);
    let before = db.browse_results.borrow().preparations;
    let rebuilt = db.browse_result_page(&root, false, &revision, BrowsePageRequest::default()).unwrap().unwrap();
    assert_eq!(rebuilt.contents.books, first.contents.books);
    assert_eq!(rebuilt.page.unwrap().result_revision.unwrap(), revision);
    assert_eq!(db.browse_results.borrow().preparations, before + 1);
}

#[test]
fn recently_used_results_survive_capacity_eviction() {
    let db = fixture();
    let query = query(BrowseLocation::Books);
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    for index in 1..MAX_RESULTS {
        let mut distinct = query.clone();
        distinct.search = format!("missing {index}");
        db.browse_page(&distinct, false, BrowsePageRequest::default()).unwrap();
    }
    // Touch the original result before exceeding capacity: the older unused
    // query should be evicted, and this result should still be reusable.
    db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().unwrap();
    let mut distinct = query.clone();
    distinct.search = "another query".into();
    db.browse_page(&distinct, false, BrowsePageRequest::default()).unwrap();
    let before = db.browse_results.borrow().preparations;
    let resumed = db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().unwrap();
    assert_eq!(resumed.contents, first.contents);
    assert_eq!(resumed.page.unwrap().result_revision.unwrap(), revision);
    assert_eq!(db.browse_results.borrow().preparations, before);
}

#[test]
fn complete_author_contents_preserve_headings_without_preparing_section_books() {
    let db = fixture();
    let query = query(BrowseLocation::Author(None));
    let contents = db.browse_contents(&query, false).unwrap();
    assert_eq!(contents.children.len(), 37);
    assert!(contents.books.is_empty());
    assert!(db.browse_results.borrow().entries.back().unwrap().result.section_orders.lock().unwrap().is_empty());

    let initial = db.browse_page(&query, false, BrowsePageRequest { section_limit: 2, ..Default::default() }).unwrap();
    assert_eq!(initial.contents.children, contents.children[..2]);
    assert_eq!(initial.sections.len(), 2);
    assert!(initial.page.is_some());
    assert!(!initial.format_counts.is_empty());

    let revision = initial.page.as_ref().unwrap().result_revision.as_ref().unwrap();
    let continued = db.browse_result_page(&query, false, revision, BrowsePageRequest { section_offset: 2, section_limit: 2, ..Default::default() }).unwrap().unwrap();
    assert_eq!(continued.contents.children, contents.children[2..4]);
    assert_eq!(continued.sections.len(), 2);
    assert!(continued.page.is_some());
    assert!(continued.format_counts.is_empty());
    assert!(continued.path.is_empty());
    assert!(continued.ancestors.is_empty());
}

#[test]
fn commits_from_another_library_connection_invalidate_continuations() {
    let path = std::env::temp_dir().join(format!("browse-result-{}.db", Uuid::new_v4()));
    {
        let db = Database::open(&path).unwrap();
        populate(&db);
        let query = query(BrowseLocation::Books);
        let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
        let revision = first.page.unwrap().result_revision.unwrap();
        let changes = db.connection.total_changes();
        let writer = Database::open(&path).unwrap();
        writer.connection.execute("UPDATE book SET title='Changed by writer' WHERE row_id=1", []).unwrap();
        assert_eq!(db.connection.total_changes(), changes);
        assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_none());
        let fresh = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
        assert_ne!(fresh.page.unwrap().result_revision.unwrap(), revision);
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn author_results_only_prepare_requested_memberships() {
    let db = fixture();
    let query = query(BrowseLocation::Author(None));
    let first = db.browse_page(&query, false, BrowsePageRequest { section_limit: 2, ..Default::default() }).unwrap();
    let cache = db.browse_results.borrow();
    let result = &cache.entries.back().unwrap().result;
    assert!(result.sections.is_empty(), "author sections must not eagerly retain membership sets");
    assert!(result.positions.is_empty(), "author index must not globally order all books");
    assert_eq!(result.section_orders.lock().unwrap().len(), 2);
    assert_eq!(first.page.as_ref().unwrap().heading_count(), 37);
    let bytes = result.estimated_bytes();
    drop(cache);
    let revision = first.page.unwrap().result_revision.unwrap();
    db.browse_result_page(&query, false, &revision, BrowsePageRequest { section_offset: 2, section_limit: 2, ..Default::default() }).unwrap().unwrap();
    let cache = db.browse_results.borrow();
    let result = &cache.entries.back().unwrap().result;
    assert_eq!(result.section_orders.lock().unwrap().len(), 4);
    assert!(result.estimated_bytes() > bytes, "lazy orders must count toward the cache budget");
}

#[test]
fn bookkeeping_and_no_op_writes_preserve_results_but_download_requests_do_not() {
    let db = fixture();
    let query = query(BrowseLocation::Author(None));
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    let prepared = db.browse_results.borrow().preparations;
    db.connection.execute("UPDATE library_local_state SET scan_state='pending'", []).unwrap();
    db.connection.execute("INSERT INTO book_transfer(content_hash,cover_state) SELECT content_hash,'ready' FROM book WHERE row_id=1", []).unwrap();
    db.connection.execute("UPDATE book_transfer SET server_has_cover=1,cover_checked=1", []).unwrap();
    db.connection.execute("UPDATE book SET title=title,read_progress=read_progress", []).unwrap();
    db.connection.execute("UPDATE book_dir SET local_hash='new fingerprint'", []).unwrap();
    assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_some());
    assert_eq!(db.browse_results.borrow().preparations, prepared);
    db.connection.execute("UPDATE book_transfer SET book_download='background'", []).unwrap();
    assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_none());
}

#[test]
fn rolled_back_changes_preserve_revisions_and_author_orders() {
    let db = fixture();
    let query = query(BrowseLocation::Author(None));
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    let transaction = db.connection.unchecked_transaction().unwrap();
    transaction.execute("UPDATE book SET title='Rolled back',read_progress=1", []).unwrap();
    transaction.execute("DELETE FROM book_dir WHERE book_row_id=1", []).unwrap();
    transaction.rollback().unwrap();
    let resumed = db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().unwrap();
    assert_eq!(resumed.contents, first.contents);
    assert_eq!(resumed.sections, first.sections);
}

#[test]
fn external_bookkeeping_commits_preserve_result_revisions() {
    let path = std::env::temp_dir().join(format!("browse-bookkeeping-{}.db", Uuid::new_v4()));
    {
        let db = Database::open(&path).unwrap();
        populate(&db);
        let query = query(BrowseLocation::Books);
        let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
        let revision = first.page.unwrap().result_revision.unwrap();
        let writer = Database::open(&path).unwrap();
        writer.connection.execute("UPDATE library_local_state SET scan_state='pending'", []).unwrap();
        assert!(db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().is_some());
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn folder_pages_preserve_placements_and_home_pages_preserve_activity_order() {
    let db = fixture();
    let nested = Uuid::from_u128(1000);
    db.connection.execute("INSERT INTO dir(id,parent_id,name) VALUES(?1,?2,'Nested')", params![nested.to_string(), sync_common::ROOT_DIR_ID.to_string()]).unwrap();
    for id in 1..=17 {
        db.connection.fixtures_insert_book_dir_2(&nested.to_string(), &id, &format!("copy{id}.epub"), &(id % 2)).unwrap();
    }
    db.connection.execute("UPDATE book SET read_at=row_id,added_at=100-row_id,read_progress=CASE WHEN row_id%3=0 THEN 1 WHEN row_id%2=0 THEN 0.4 ELSE 0 END", []).unwrap();
    for location in [BrowseLocation::Folder(sync_common::ROOT_DIR_ID), BrowseLocation::Folder(nested), BrowseLocation::Home(None), BrowseLocation::Home(Some(library_model::HomeShelf::RecentlyRead))] {
        for sort in [BrowseBookSort::DefaultOrder, BrowseBookSort::Alphabetical, BrowseBookSort::RecentlyRead, BrowseBookSort::Popularity] {
            for search in ["", "book"] {
                for downloaded_only in [false, true] {
                    let mut query = query(location);
                    query.book_sort = sort;
                    query.search = search.into();
                    let full = db.legacy_browse(&query, downloaded_only).unwrap();
                    let first = db.browse_page(&query, downloaded_only, BrowsePageRequest { section_limit: 1, book_limit: 2, ..Default::default() }).unwrap();
                    let revision = first.page.as_ref().unwrap().result_revision.as_ref().unwrap();
                    assert_eq!(first.contents.children, full.contents.children);
                    assert_eq!(first.path, full.path);
                    assert_eq!(first.ancestors, full.ancestors);
                    let mut loaded = Vec::new();
                    for offset in (0..full.contents.books.len()).step_by(2) {
                        let page = db.browse_result_books(&query, downloaded_only, revision, location, offset, 2).unwrap().unwrap();
                        assert_eq!(page.total, full.contents.books.len());
                        loaded.extend(page.books);
                    }
                    assert_eq!(loaded, full.contents.books, "{location} {sort:?} {search} downloaded={downloaded_only}");
                    for (index, section) in full.sections.iter().enumerate() {
                        let page = db.browse_result_page(&query, downloaded_only, revision, BrowsePageRequest { section_offset: index, section_limit: 1, book_limit: 0, ..Default::default() }).unwrap().unwrap();
                        assert_eq!(page.sections[0].parent, section.parent);
                        assert_eq!(page.sections[0].children, section.children);
                        assert_eq!(page.sections[0].book_count, Some(section.books.len()));
                        assert_eq!(page.sections[0].books, section.books[..section.books.len().min(BROWSE_SECTION_BOOK_LIMIT)]);
                        let mut loaded = Vec::new();
                        for offset in (0..section.books.len()).step_by(2) {
                            loaded.extend(db.browse_result_books(&query, downloaded_only, revision, section.parent.id, offset, 2).unwrap().unwrap().books);
                        }
                        assert_eq!(loaded, section.books);
                    }
                }
            }
        }
    }
}

#[test]
fn continuation_pages_omit_unchanged_navigation_and_facets() {
    let db = fixture();
    let query = query(BrowseLocation::Subject(0));
    let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
    assert!(!first.contents.children.is_empty());
    assert!(!first.format_counts.is_empty());
    let revision = first.page.unwrap().result_revision.unwrap();
    let continuation = db.browse_result_page(&query, false, &revision, BrowsePageRequest::default()).unwrap().unwrap();
    assert_eq!(continuation.sections, first.sections);
    assert!(continuation.contents.children.is_empty());
    assert!(continuation.language_counts.is_empty());
    assert!(continuation.format_counts.is_empty());
    assert!(continuation.path.is_empty());
    assert!(continuation.ancestors.is_empty());
}

#[test]
fn growing_lazy_orders_are_evicted_when_the_result_budget_is_exceeded() {
    let db = fixture();
    let query = query(BrowseLocation::Author(None));
    let first = db.browse_page(&query, false, BrowsePageRequest { section_limit: 1, ..Default::default() }).unwrap();
    let revision = first.page.unwrap().result_revision.unwrap();
    let section = first.sections[0].parent.id;
    let cache = db.browse_results.borrow();
    let result = &cache.entries.back().unwrap().result;
    let book = result.section_orders.lock().unwrap()[&section][0];
    // Model a large lazily prepared ordering without an enormous SQL fixture.
    result.section_orders.lock().unwrap().insert(section, Arc::new(vec![book; MAX_CACHED_BYTES / size_of::<Item>() + 1]));
    assert!(result.estimated_bytes() > MAX_CACHED_BYTES);
    drop(cache);
    db.browse_result_page(&query, false, &revision, BrowsePageRequest { section_limit: 0, book_limit: 0, ..Default::default() }).unwrap().unwrap();
    assert!(db.browse_results.borrow().entries.is_empty());
    let resumed = db.browse_result_page(&query, false, &revision, BrowsePageRequest { section_limit: 1, ..Default::default() }).unwrap().unwrap();
    assert_eq!(resumed.sections, first.sections);
}
