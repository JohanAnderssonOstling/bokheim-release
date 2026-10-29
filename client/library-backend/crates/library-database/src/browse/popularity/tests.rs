use crate::sql_fixtures::FixturesSql;
use crate::{Database, ImportCommit};
use book_model::{AUTHOR_MARC_RELATOR_CODE, BookDetails, BookFormat, BookMetadata, BookRecord, Contributor, LanguageTag};
use library_model::{BrowseBookSort, BrowseChipSort, BrowseLocation, HomeShelf, LibraryBrowseQuery, PublishedStagedBook};
use library_replica::{EnrichmentResult, LibraryStateEntry};
use metadata_contract::WorkPopularity;
use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
use sync_common::{BookId, Delivery, DirId, LibraryRevision, PullStateResponse, ROOT_DIR_ID, ReplicaId, ReplicaSeq, SyncCursor};

fn database() -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    db
}

fn query(location: BrowseLocation) -> LibraryBrowseQuery {
    LibraryBrowseQuery { location, search: String::new(), file_types: vec![], languages: vec![], chip_sort: BrowseChipSort::Popularity, book_sort: BrowseBookSort::Popularity, hide_finished: false }
}

fn add(db: &Database, parent: DirId, number: u8, title: &str, count: Option<u64>, authors: &[&str], language: &str) -> BookId {
    let hash = BookId::from_bytes([number; 32]);
    let name = format!("book{number}.epub");
    let contributors = authors.iter().map(|name| Contributor::with_id(book_model::agent_id_from_migration_seed(name.as_bytes()), name, AUTHOR_MARC_RELATOR_CODE).unwrap()).collect();
    let record =
        BookRecord { metadata: BookDetails { title: title.into(), contributors, book: BookMetadata { languages: vec![LanguageTag::parse(language).unwrap()], ..Default::default() }, ..Default::default() }, description: String::new() };
    db.commit_import(ImportCommit {
        parent_id: parent,
        file_name: name.clone(),
        format: BookFormat::Epub,
        content_hash: hash,
        size_bytes: 1,
        inspection_version: 1,
        inspection: Some(book_inspection::InspectedBook { metadata: record, pdf: None, audiobook: None, toc: vec![] }),
        published: PublishedStagedBook { name: name.clone(), relative_path: format!("/{name}"), published: false, fingerprint: None },
        request_thumbnail: false,
        restore_paths: vec![],
    })
    .unwrap();
    let candidate = db.enrichment_candidates(&[hash], 0).unwrap().pop().unwrap();
    db.record_enrichment(
        &hash,
        &candidate.request_key,
        &BookEnrichment {
            key: String::new(),
            status: BookEnrichmentStatus::Found,
            record: Some(candidate.record),
            toc: None,
            cover: None,
            popularity: count.map(|rating_count| WorkPopularity { open_library_work_id: format!("OL{number}W"), rating_count, snapshot_date: "2026-09-01".into() }),
            detail: String::new(),
        },
        0,
    )
    .unwrap();
    hash
}

#[test]
fn leaf_popularity_orders_known_scores_before_unknowns_and_keeps_answers_immutable() {
    let db = database();
    let unknown = add(&db, ROOT_DIR_ID, 1, "A unknown", None, &["Writer"], "en");
    let zero = add(&db, ROOT_DIR_ID, 2, "B zero", Some(0), &["Writer"], "en");
    let beta = add(&db, ROOT_DIR_ID, 3, "Z beta", Some(30), &["Writer"], "sv");
    let alpha = add(&db, ROOT_DIR_ID, 4, "C alpha", Some(30), &["Writer"], "en");
    let before = db.sync_publishable_mutations().unwrap();
    let mut query = query(BrowseLocation::Books);
    assert_eq!(db.browse_contents(&query, false).unwrap().books.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [alpha, beta, zero, unknown]);
    query.languages = vec!["en".into()];
    assert_eq!(db.browse_contents(&query, false).unwrap().books.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [alpha, zero, unknown]);
    assert_eq!(db.sync_publishable_mutations().unwrap(), before);
    assert_eq!(db.enrichment_popularity(&beta).unwrap().unwrap().rating_count, 30);
}

fn stored_count(db: &Database, hash: &BookId) -> Option<f64> {
    db.connection.query_row("SELECT popularity_rating_count FROM book WHERE content_hash=?1", [hash], |row| row.get(0)).unwrap()
}

#[test]
fn popularity_projection_handles_unknown_zero_and_full_u64_counts() {
    let db = database();
    let unknown = add(&db, ROOT_DIR_ID, 1, "A unknown", None, &["Writer"], "en");
    let zero = add(&db, ROOT_DIR_ID, 2, "B zero", Some(0), &["Writer"], "en");
    let maximum = add(&db, ROOT_DIR_ID, 3, "Z maximum", Some(u64::MAX), &["Writer"], "en");
    assert_eq!(stored_count(&db, &unknown), None);
    assert_eq!(stored_count(&db, &zero), Some(0.0));
    assert_eq!(stored_count(&db, &maximum), Some(u64::MAX as f64));
    let mut query = query(BrowseLocation::Books);
    query.chip_sort = BrowseChipSort::Alphabetical;
    assert_eq!(db.browse_contents(&query, false).unwrap().books.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [maximum, zero, unknown]);
    query.book_sort = BrowseBookSort::Alphabetical;
    assert_eq!(db.browse_contents(&query, false).unwrap().books.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [unknown, zero, maximum]);
}

#[test]
fn popularity_browsing_reads_projected_counts_without_decoding_book_records() {
    let db = database();
    let less = add(&db, ROOT_DIR_ID, 1, "A less", Some(2), &["A writer"], "en");
    let more = add(&db, ROOT_DIR_ID, 2, "Z more", Some(20), &["Z writer"], "en");
    // Deliberately unreadable canonical payloads expose any browse-time decode.
    db.connection.execute("UPDATE sync_book_record SET body=?1 WHERE state_kind='book_add'", [b"invalid".as_slice()]).unwrap();
    assert_eq!(db.browse_contents(&query(BrowseLocation::Books), false).unwrap().books.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [more, less]);
    assert_eq!(db.browse(&query(BrowseLocation::Author(None)), false).unwrap().contents.children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Z writer", "A writer"]);
}

#[test]
fn version_38_backfills_popularity_once_and_preserves_authored_state() {
    let path = std::env::temp_dir().join(format!("popularity-migration-{}.sqlite3", uuid::Uuid::new_v4()));
    let db = Database::open(&path).unwrap();
    let replica = db.initialize_library().unwrap();
    let unknown = add(&db, ROOT_DIR_ID, 1, "Unknown", None, &["Writer"], "en");
    let zero = add(&db, ROOT_DIR_ID, 2, "Zero", Some(0), &["Writer"], "en");
    let known = add(&db, ROOT_DIR_ID, 3, "Known", Some(42), &["Writer"], "en");
    let unobserved = BookId::from_bytes([4; 32]);
    db.connection.fixtures_insert_book(&unobserved).unwrap();
    let publications = db.sync_publishable_mutations().unwrap();
    crate::browse::revision::remove(&db.connection).unwrap();
    db.connection.execute_batch("ALTER TABLE book DROP COLUMN popularity_rating_count; PRAGMA user_version=38;").unwrap();
    drop(db);

    let db = Database::open(&path).unwrap();
    assert_eq!(db.initialize_library().unwrap(), replica);
    assert_eq!(stored_count(&db, &unknown), None);
    assert_eq!(stored_count(&db, &zero), Some(0.0));
    assert_eq!(stored_count(&db, &known), Some(42.0));
    assert_eq!(stored_count(&db, &unobserved), None);
    assert_eq!(db.sync_publishable_mutations().unwrap(), publications);
    assert_eq!(db.connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).unwrap(), crate::LIBRARY_SCHEMA_VERSION);
    assert_eq!(db.initialize_library().unwrap(), replica);
    assert_eq!(stored_count(&db, &known), Some(42.0));
    db.check_migrated().unwrap();
    drop(db);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn server_accepted_addition_updates_and_clears_projected_popularity() {
    let db = database();
    let hash = add(&db, ROOT_DIR_ID, 1, "Book", Some(10), &["Writer"], "en");
    let publication = db.sync_publishable_mutations().unwrap().into_iter().find(|publication| matches!(publication.state.value, LibraryStateEntry::BookAdd { .. })).unwrap();
    let LibraryStateEntry::BookAdd { value: mut addition, .. } = publication.state.value else { unreachable!() };
    for (sequence, count) in [(1, Some(50)), (2, None)] {
        let Some(EnrichmentResult::Found { popularity, .. }) = &mut addition.enrichment else { unreachable!() };
        *popularity = count.map(|rating_count| WorkPopularity { open_library_work_id: "OL1W".into(), rating_count, snapshot_date: "2026-09-01".into() });
        let accepted = LibraryStateEntry::BookAdd { content_hash: hash, value: addition.clone() }.publication(ReplicaId::from_u128(0), 0, ReplicaSeq::new(sequence).unwrap());
        let delivery = Delivery { state: sync_common::transport::representation(&accepted.state).unwrap(), revision: LibraryRevision::new(sequence).unwrap() };
        db.sync_commit_pull_response(&[], &PullStateResponse { mutations: vec![delivery], next_cursor: SyncCursor::default(), has_more: false }).unwrap();
        assert_eq!(stored_count(&db, &hash), count.map(|count| count as f64));
    }
}

#[test]
fn folder_popularity_counts_shared_descendants_once_and_sorts_sections_and_ancestors() {
    let db = database();
    let repeated = db.create_directory(&ROOT_DIR_ID, &"A repeated".into()).unwrap().id;
    let first = db.create_directory(&repeated, &"First".into()).unwrap().id;
    let second = db.create_directory(&repeated, &"Second".into()).unwrap().id;
    let winner = db.create_directory(&ROOT_DIR_ID, &"Z winner".into()).unwrap().id;
    let hash = add(&db, first, 1, "Shared", Some(10), &["Writer"], "en");
    db.copy_book_to_directory(&hash, Some(&first), &second).unwrap();
    add(&db, winner, 2, "Winner", Some(15), &["Other writer"], "sv");
    let mut query = query(BrowseLocation::Folder(ROOT_DIR_ID));
    let root = db.browse(&query, false).unwrap();
    assert_eq!(root.contents.children.iter().map(|row| row.id).collect::<Vec<_>>(), [BrowseLocation::Folder(winner), BrowseLocation::Folder(repeated)]);
    assert_eq!(root.sections.iter().map(|section| section.parent.id).collect::<Vec<_>>(), [BrowseLocation::Folder(winner), BrowseLocation::Folder(repeated)]);
    query.location = BrowseLocation::Folder(first);
    let nested = db.browse(&query, false).unwrap();
    assert_eq!(nested.ancestors[0].1[0].id, BrowseLocation::Folder(winner));
    query.location = BrowseLocation::Folder(ROOT_DIR_ID);
    query.languages = vec!["en".into()];
    assert_eq!(db.browse(&query, false).unwrap().contents.children.iter().map(|row| row.id).collect::<Vec<_>>(), [BrowseLocation::Folder(repeated)]);
}

#[test]
fn author_popularity_sums_coauthored_books_once_and_recomputes_after_filters() {
    let db = database();
    add(&db, ROOT_DIR_ID, 1, "Shared", Some(10), &["A author", "Z author", "Z author"], "en");
    add(&db, ROOT_DIR_ID, 2, "Additional", Some(5), &["Z author"], "sv");
    let mut query = query(BrowseLocation::Author(None));
    let page = db.browse(&query, false).unwrap();
    assert_eq!(page.contents.children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Z author", "A author"]);
    assert_eq!(page.sections[0].books.iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Shared", "Additional"]);
    query.languages = vec!["en".into()];
    assert_eq!(db.browse(&query, false).unwrap().contents.children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A author", "Z author"]);
    let shared = BookId::from_bytes([1; 32]);
    db.update_reading_position(&shared, "epubcfi(/6/2)", Some(100.0), None).unwrap();
    query.hide_finished = true;
    query.languages.clear();
    assert_eq!(db.browse(&query, false).unwrap().contents.children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Z author"]);
}

#[test]
fn subject_popularity_uses_subtree_union_and_filtered_members() {
    let db = database();
    let shared = add(&db, ROOT_DIR_ID, 1, "Shared", Some(10), &["Writer"], "en");
    let extra = add(&db, ROOT_DIR_ID, 2, "Extra", Some(5), &["Writer"], "sv");
    let unknown = add(&db, ROOT_DIR_ID, 3, "Unknown", None, &["Writer"], "en");
    let another = add(&db, ROOT_DIR_ID, 4, "Another", None, &["Writer"], "en");
    let competitor = add(&db, ROOT_DIR_ID, 5, "Competitor", Some(20), &["Writer"], "en");
    db.connection.fixtures_detach_curated_concept().unwrap();
    db.connection.fixtures_popularity_other_subject().unwrap();
    let shared_id: i64 = db.connection.fixtures_read_book_row_id(&shared, |row| row.get(0)).unwrap();
    let extra_id: i64 = db.connection.fixtures_read_book_row_id(&extra, |row| row.get(0)).unwrap();
    db.connection.fixtures_insert_book_unified_concept_2(&shared_id, &3).unwrap();
    db.connection.fixtures_insert_book_unified_concept_2(&shared_id, &4).unwrap();
    db.connection.fixtures_insert_book_unified_concept_2(&extra_id, &3).unwrap();
    for (hash, concepts) in [(unknown, vec![3, 4, 5]), (another, vec![4, 5]), (competitor, vec![5])] {
        let id: i64 = db.connection.fixtures_read_book_row_id(&hash, |row| row.get(0)).unwrap();
        for concept in concepts {
            db.connection.fixtures_insert_book_unified_concept_2(&id, &concept).unwrap();
        }
    }
    let mut query = query(BrowseLocation::Subject(0));
    let root = db.browse(&query, false).unwrap();
    assert_eq!(root.contents.children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A other", "Science"]);
    let parent = root.contents.children[1].id;
    query.location = parent;
    let children = db.browse(&query, false).unwrap().contents.children;
    assert_eq!(children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Quantum", "Optics"]);
    query.languages = vec!["en".into()];
    assert_eq!(db.browse(&query, false).unwrap().contents.children.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Optics", "Quantum"]);
}

#[test]
fn home_popularity_reorders_activity_members_without_changing_shelf_membership() {
    let db = database();
    let popular = add(&db, ROOT_DIR_ID, 1, "Popular", Some(20), &["Writer"], "en");
    let recent = add(&db, ROOT_DIR_ID, 2, "Recent", Some(5), &["Writer"], "en");
    add(&db, ROOT_DIR_ID, 3, "New addition", Some(50), &["Writer"], "en");
    db.update_reading_position(&popular, "epubcfi(/6/2)", Some(0.25), None).unwrap();
    db.update_reading_position(&recent, "epubcfi(/6/2)", Some(0.25), None).unwrap();
    // Consecutive writes can share a millisecond; give this ordering fixture explicit recency.
    db.connection.execute("UPDATE book SET read_at=1 WHERE content_hash=?1", [&popular]).unwrap();
    db.connection.execute("UPDATE book SET read_at=2 WHERE content_hash=?1", [&recent]).unwrap();
    let mut query = query(BrowseLocation::Home(Some(HomeShelf::RecentlyRead)));
    query.book_sort = BrowseBookSort::DefaultOrder;
    query.chip_sort = BrowseChipSort::DefaultOrder;
    let before = db.browse_contents(&query, false).unwrap().books;
    assert_eq!(before.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [recent, popular]);
    query.book_sort = BrowseBookSort::Popularity;
    let after = db.browse_contents(&query, false).unwrap().books;
    assert_eq!(after.iter().map(|book| book.content_hash().unwrap()).collect::<Vec<_>>(), [popular, recent]);
    query.location = BrowseLocation::Home(None);
    query.chip_sort = BrowseChipSort::Popularity;
    let root = db.browse(&query, false).unwrap();
    assert_eq!(root.contents.children.iter().map(|row| row.id).collect::<Vec<_>>(), [BrowseLocation::Home(Some(HomeShelf::RecentlyAdded)), BrowseLocation::Home(Some(HomeShelf::RecentlyRead))]);
    assert_eq!(root.sections.iter().map(|section| section.parent.id).collect::<Vec<_>>(), root.contents.children.iter().map(|row| row.id).collect::<Vec<_>>());
}
