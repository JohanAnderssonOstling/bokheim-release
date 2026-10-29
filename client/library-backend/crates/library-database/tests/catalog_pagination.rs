//! A separate process owns this fixture's single catalog activation.
use library_database::{
    Database, catalog,
    rusqlite::{Connection, OptionalExtension, params},
};
use library_model::{BrowseBookSort, BrowseChipSort, BrowseLocation, BrowsePageRequest, LibraryBrowseQuery, LibraryFileTypeFilter};
use std::path::{Path, PathBuf};

#[test]
fn catalog_first_pages_and_arrow_pages_match_complete_sections() {
    let taxonomy = Connection::open_with_flags(subject_projection::bundled::path().unwrap(), library_database::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let concepts =
        taxonomy.prepare("SELECT concept_id FROM unified_concept_route WHERE parent_route_id=0 AND route_id!=0 ORDER BY route_id LIMIT 2").unwrap().query_map([], |row| row.get::<_, i64>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(concepts.len(), 2);
    let content: Option<String> = taxonomy.query_row("SELECT value FROM taxonomy_meta WHERE key='client_revision'", [], |row| row.get(0)).optional().unwrap();
    let revision = format!("{}:{}", content.as_deref().unwrap_or(subject_projection::BUNDLED_UNIFIED_TAXONOMY_REVISION_ID), subject_projection::UNIFIED_MATCHER_VERSION);
    let source = Connection::open_in_memory().unwrap();
    source.execute_batch(include_str!("sql/catalog_pagination.sql")).unwrap();
    source.execute_batch("INSERT INTO snapshot VALUES(1,8,1); INSERT INTO description VALUES(1,'Description unused by cards'); INSERT INTO source VALUES(1,'publicdomainlibrary.org'),(2,'gutenberg.org');").unwrap();
    source.execute("INSERT INTO subject_projection VALUES(1,?1)", [&revision]).unwrap();
    for id in 1_i64..=2 {
        source.execute("INSERT INTO author VALUES(?1,?2,?3)", params![id, uuid::Uuid::from_u128(id as u128).as_bytes().as_slice(), format!("Writer {id}")]).unwrap();
    }
    for id in 1..=14 {
        let external = id % 3 == 0;
        let format = if external { 16 } else { 1 };
        let source_id = if id % 4 == 0 { 1 } else { 2 };
        source
            .execute(
                "INSERT INTO book(row_id,key,external,title,subtitle,description_id,source_id,entry_url,download_url,format) VALUES(?1,?2,?3,?4,?5,1,?6,'https://example.org/book','https://example.org/audio.zip',?7)",
                params![id, [id as u8; 32].as_slice(), external, format!("Title {:02}", 15 - id), format!("Subtitle {id}"), source_id, format],
            )
            .unwrap();
        source.execute("INSERT INTO book_author VALUES(?1,?2)", params![id, if id <= 7 { 1 } else { 2 }]).unwrap();
        if id == 7 {
            source.execute("INSERT INTO book_author VALUES(7,2)", []).unwrap();
        }
        source.execute("INSERT INTO book_language VALUES(?1,?2)", params![id, if id % 2 == 0 { "sv" } else { "en" }]).unwrap();
        source.execute("INSERT INTO book_source VALUES(?1,?2)", params![id, source_id]).unwrap();
        source.execute("INSERT INTO book_concept VALUES(?1,?2)", params![id, concepts[usize::from(id > 7)]]).unwrap();
    }
    source
        .execute_batch(
            "CREATE TABLE book_audiobook_duration(book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id),duration_ms INTEGER NOT NULL CHECK(duration_ms>0)) STRICT;
        INSERT INTO book_audiobook_duration VALUES(3,27789000);",
        )
        .unwrap();
    // A coauthored book gives the index more headings than its first window.
    for id in 3_i64..=38 {
        source.execute("INSERT INTO author VALUES(?1,?2,?3)", params![id, uuid::Uuid::from_u128(id as u128).as_bytes().as_slice(), format!("Writer {id}")]).unwrap();
        source.execute("INSERT INTO book_author VALUES(14,?1)", [id]).unwrap();
    }
    let catalog_path = std::env::temp_dir().join(format!("catalog-result-{}.sqlite3", uuid::Uuid::new_v4()));
    std::fs::write(&catalog_path, source.serialize("main").unwrap().to_vec()).unwrap();
    assert_eq!(catalog::activate_path(&catalog_path).unwrap(), Some(1));
    let path: PathBuf = std::env::temp_dir().join(format!("catalog-pagination-{}.sqlite3", uuid::Uuid::new_v4()));
    {
        let db = Database::open(&path).unwrap();
        db.initialize_library().unwrap();
        let audio_key = library_model::CardKey([3; 32]);
        assert_eq!(catalog::entry(audio_key).unwrap().unwrap().audiobook_duration_ms, Some(27_789_000));
        assert_eq!(db.external_catalog_detail(audio_key).unwrap().book.audiobook_duration_ms, Some(27_789_000));
        assert_eq!(catalog::entry(library_model::CardKey([6; 32])).unwrap().unwrap().audiobook_duration_ms, None);
        let writer = library_writer(&path);
        writer.execute("INSERT INTO book(row_id,content_hash,title,format) VALUES(1,?1,'Held',1)", [[2_u8; 32].as_slice()]).unwrap();
        writer.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,1,'held.epub','',1)", [sync_common::ROOT_DIR_ID.to_string()]).unwrap();
        drop(writer);
        let mut heading_query = LibraryBrowseQuery {
            location: BrowseLocation::CatalogAuthor(None),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::Alphabetical,
            book_sort: BrowseBookSort::Alphabetical,
            hide_finished: false,
        };
        let initial_headings = db.browse_page(&heading_query, false, BrowsePageRequest::default()).unwrap();
        assert_eq!(initial_headings.contents.children.len(), 24);
        assert_eq!(initial_headings.sections.len(), 24);
        assert_eq!(initial_headings.page.as_ref().unwrap().section_ids.len(), 24);
        assert_eq!(initial_headings.page.unwrap().heading_count(), 38);
        for search in ["", "writer", "writer 3", "missing"] {
            heading_query.search = search.into();
            let complete = db.browse(&heading_query, false).unwrap();
            let first = db.browse_page(&heading_query, false, BrowsePageRequest::default()).unwrap();
            assert_eq!(first.page.as_ref().unwrap().heading_count(), complete.contents.children.len());
            assert_eq!(first.contents.children, complete.contents.children[..complete.contents.children.len().min(24)]);
            let mut names = Vec::new();
            for offset in (0..complete.contents.children.len()).step_by(24) {
                let page = db.browse_page(&heading_query, false, BrowsePageRequest { section_offset: offset, section_limit: 24, book_offset: 0, book_limit: 0 }).unwrap();
                assert_eq!(page.page.as_ref().unwrap().headings, first.page.as_ref().unwrap().headings);
                names.extend(page.contents.children);
            }
            assert_eq!(names, complete.contents.children);
        }
        for location in [BrowseLocation::Catalog(0), BrowseLocation::CatalogAuthor(None)] {
            for sort in [BrowseBookSort::DefaultOrder, BrowseBookSort::Alphabetical, BrowseBookSort::Popularity] {
                for languages in [vec![], vec!["en".to_owned()], vec!["sv".to_owned()]] {
                    for file_types in [vec![], vec![LibraryFileTypeFilter::Book], vec![LibraryFileTypeFilter::Audiobook]] {
                        let mut query =
                            LibraryBrowseQuery { location, search: String::new(), file_types, languages: languages.clone(), chip_sort: BrowseChipSort::Popularity, book_sort: sort, hide_finished: false };
                        let full = db.browse(&query, false).unwrap();
                        for card in full.contents.books.iter().chain(full.sections.iter().flat_map(|section| &section.books)) {
                            let expected_duration = (card.key() == audio_key).then_some(27_789_000);
                            assert_eq!(card.audiobook_duration_ms, expected_duration);
                        }
                        let window_full = db.browse_sections(&query, false, library_model::BROWSE_SECTION_BOOK_LIMIT).unwrap();
                        let mut panels = Vec::new();
                        for offset in 0..window_full.sections.len() {
                            let page = db.browse_page(&query, false, BrowsePageRequest { section_offset: offset, section_limit: 1, book_offset: 0, book_limit: 0 }).unwrap();
                            let revision = page.page.as_ref().unwrap().result_revision.clone().unwrap();
                            let continuation = db.browse_result_page(&query, false, &revision, BrowsePageRequest { section_offset: offset, section_limit: 1, book_offset: 0, book_limit: 0 }).unwrap().unwrap();
                            assert_eq!(continuation.contents.books, page.contents.books);
                            if matches!(location, BrowseLocation::CatalogAuthor(None)) {
                                assert_eq!(continuation.contents.children, page.contents.children);
                            } else {
                                assert!(continuation.contents.children.is_empty(), "continuations reuse installed subject navigation");
                            }
                            assert!(continuation.language_counts.is_empty());
                            assert!(continuation.format_counts.is_empty());
                            assert_eq!(continuation.sections, page.sections);
                            let state = page.page.unwrap();
                            if matches!(location, BrowseLocation::CatalogAuthor(None)) {
                                assert_eq!(state.heading_count(), full.contents.children.len());
                                assert_eq!(state.section_ids, vec![window_full.sections[offset].parent.id]);
                                assert_eq!(page.contents.children, full.contents.children[offset..offset + 1]);
                            } else {
                                assert_eq!(state.section_ids, window_full.sections.iter().map(|section| section.parent.id).collect::<Vec<_>>());
                                assert_eq!(page.contents.children, full.contents.children);
                            }
                            assert_eq!(page.language_counts, full.language_counts);
                            assert_eq!(page.sections.len(), 1);
                            panels.extend(page.sections);
                        }
                        assert_eq!(panels, window_full.sections);
                        let result = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
                        let result_revision = result.page.as_ref().unwrap().result_revision.as_ref().unwrap();
                        for section in &result.sections {
                            let all = window_full.sections.iter().find(|found| found.parent.id == section.parent.id).unwrap();
                            let cards = db.browse_result_books(&query, false, result_revision, section.parent.id, 0, 2).unwrap().unwrap();
                            assert_eq!(cards.books, all.books[..all.books.len().min(2)]);
                        }
                        let first = db.browse_sections(&query, false, 2).unwrap();
                        assert_eq!(first.contents, full.contents);
                        assert_eq!(first.sections.len(), full.sections.len());
                        for (initial, complete) in first.sections.iter().zip(&full.sections) {
                            assert_eq!(initial.book_count, Some(complete.books.len()));
                            assert_eq!(initial.books, complete.books[..complete.books.len().min(2)]);
                            query.location = initial.parent.id;
                            for offset in (0..complete.books.len()).step_by(2) {
                                let page = db.browse_page(&query, false, BrowsePageRequest { section_offset: 0, section_limit: 0, book_offset: offset, book_limit: 2 }).unwrap();
                                assert_eq!(page.page.unwrap().book_total, complete.books.len());
                                assert_eq!(page.contents.books, complete.books[offset..(offset + 2).min(complete.books.len())]);
                            }
                            let mut loaded = Vec::new();
                            for offset in (0..complete.books.len()).step_by(2) {
                                let page = db.browse_section_books(&query, false, offset, 2).unwrap();
                                assert_eq!(page.total, complete.books.len());
                                assert_eq!(page.offset, offset);
                                loaded.extend(page.books);
                            }
                            assert_eq!(loaded, complete.books);
                            assert!(loaded.iter().all(|card| card.key().0 != [2; 32]), "held hosted books must stay excluded");
                            assert_eq!(db.browse_contents(&query, false).unwrap().books, complete.books);
                            let past = db.browse_section_books(&query, false, usize::MAX, 2).unwrap();
                            assert!(past.books.is_empty());
                            assert_eq!(past.total, complete.books.len());
                        }
                    }
                }
            }
        }
        heading_query.search.clear();
        let before = db.browse_page(&heading_query, false, BrowsePageRequest::default()).unwrap();
        let revision = before.page.as_ref().unwrap().result_revision.as_ref().unwrap();
        let section = before.sections[0].parent.id;
        let writer = library_writer(&path);
        writer.execute("UPDATE book SET title='Library metadata',read_progress=1 WHERE row_id=1", []).unwrap();
        writer.execute("UPDATE book_dir SET is_downloaded=0 WHERE book_row_id=1", []).unwrap();
        writer.execute("UPDATE library_local_state SET scan_state='pending'", []).unwrap();
        assert!(db.browse_result_page(&heading_query, false, revision, BrowsePageRequest::default()).unwrap().is_some(), "library metadata and availability do not change catalog eligibility");
        writer.execute("UPDATE book_dir SET deleted_at=1 WHERE book_row_id=1", []).unwrap();
        assert!(db.browse_result_page(&heading_query, false, revision, BrowsePageRequest::default()).unwrap().is_none(), "removing a held book changes catalog eligibility");
        writer.execute("UPDATE book_dir SET deleted_at=NULL WHERE book_row_id=1", []).unwrap();
        drop(writer);
        let before = db.browse_page(&heading_query, false, BrowsePageRequest::default()).unwrap();
        let revision = before.page.as_ref().unwrap().result_revision.as_ref().unwrap();
        // Even a replacement with the same release ID must invalidate pages.
        let attached = Connection::open_in_memory().unwrap();
        attached.execute("ATTACH DATABASE ?1 AS catalog", [catalog_path.to_str().unwrap()]).unwrap();
        let stable_id: i64 = attached.query_row("SELECT row_id FROM catalog.catalog_browse_book WHERE key=?1", [[1_u8; 32].as_slice()], |row| row.get(0)).unwrap();
        // Older format-8 snapshots omit the optional duration table entirely.
        source.execute_batch("DROP TABLE book_audiobook_duration").unwrap();
        source.execute("UPDATE book SET title='Retitled' WHERE row_id=1", []).unwrap();
        catalog::store(&source.serialize("main").unwrap(), &catalog_path, (1, 8)).unwrap();
        assert_eq!(catalog::entry(audio_key).unwrap().unwrap().audiobook_duration_ms, None);
        let published: (i64, String) = attached
            .query_row("SELECT browse.row_id,source.title FROM catalog.catalog_hosted source JOIN catalog.catalog_browse_book browse ON source.identity=browse.key WHERE browse.key=?1", [[1_u8; 32].as_slice()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(published, (stable_id, "Retitled".into()), "existing attached readers must see the complete replacement with stable IDs");
        assert!(db.browse_result_page(&heading_query, false, revision, BrowsePageRequest::default()).unwrap().is_none());
        assert!(db.browse_result_books(&heading_query, false, revision, section, 0, 2).unwrap().is_none());
        let fresh = db.browse_page(&heading_query, false, BrowsePageRequest::default()).unwrap();
        assert_ne!(fresh.page.as_ref().unwrap().result_revision.as_ref().unwrap(), revision);
        assert_eq!(fresh.page.as_ref().unwrap().heading_count(), before.page.as_ref().unwrap().heading_count());
        assert!(fresh.sections.iter().flat_map(|section| &section.books).any(|book| book.key().0 == [1; 32] && book.title == "Retitled"));
        let fresh_revision = fresh.page.as_ref().unwrap().result_revision.as_ref().unwrap();
        source.pragma_update(None, "foreign_keys", false).unwrap();
        source.execute("DELETE FROM description WHERE row_id=1", []).unwrap();
        assert!(catalog::store(&source.serialize("main").unwrap(), &catalog_path, (1, 8)).is_err(), "invalid foreign keys must roll the whole snapshot back");
        let retained = db.browse_result_page(&heading_query, false, fresh_revision, BrowsePageRequest::default()).unwrap().unwrap();
        assert_eq!(retained.contents, fresh.contents);
        assert_eq!(retained.sections, fresh.sections);
    }
    std::fs::remove_file(path).unwrap();
}

fn library_writer(path: &Path) -> Connection {
    let writer = Connection::open(path).unwrap();
    writer
        .create_scalar_function("portable_name_key", 1, library_database::rusqlite::functions::FunctionFlags::SQLITE_UTF8 | library_database::rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC, |context| {
            Ok(library_replica::portable_name_key(&context.get::<String>(0)?))
        })
        .unwrap();
    writer
}
