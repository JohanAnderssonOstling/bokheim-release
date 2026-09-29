use crate::Database;
use crate::browse::*;
use library_model::{BrowseBookSort, BrowseChipSort, LibraryFileTypeFilter};

struct TestDatabase {
    database: Database,
    path: std::path::PathBuf,
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn open_test_database() -> TestDatabase {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("library-database-browse-{}-{id}", std::process::id()));
    let database = Database::open(&path).expect("open test database");
    database.initialize_library().expect("initialize test library");
    TestDatabase { database, path }
}

#[test]
fn directory_hierarchy_browses() {
    let holder = open_test_database();
    let database = &holder.database;
    let parent = database.create_directory_with_id(&uuid::Uuid::new_v4(), &sync_common::ROOT_DIR_ID, &"Parent".to_owned()).expect("seed parent");
    let child = database.create_directory_with_id(&uuid::Uuid::new_v4(), &parent.id, &"Child".to_owned()).expect("seed child");
    assert_eq!(database.browse_book_count().expect("book count"), 0);

    let contents = database.browse_library_folder(&sync_common::ROOT_DIR_ID, false, library_model::LibraryFileTypeFilter::All).expect("folder contents");
    assert!(contents.books.is_empty());
    assert!(contents.children.iter().any(|row| row.name == "Parent" && row.book_count == 0));

    let path = database.browse_directory_path(&child.id).expect("directory path");
    assert_eq!(path.last().map(|segment| segment.name.as_str()), Some("Child"));

    let (languages, formats) = database.browse_folder_facet_counts(&sync_common::ROOT_DIR_ID.to_string(), "", &[], &[], false).expect("facet counts");
    assert!(languages.is_empty() && formats.is_empty());

    let home = database.browse_home(10, false).expect("home view");
    assert!(home.most_progress.is_empty() && home.recently_read.is_empty() && home.recently_added.is_empty());
    assert!(database.browse_authors(library_model::AuthorSort::Name, false).expect("authors").authors.is_empty());
}

fn inspected(title: &str, author: &str) -> book_metadata::InspectedBook {
    book_metadata::InspectedBook {
        pdf: None,
        metadata: book_model::BookRecord {
            title: title.to_owned(),
            subtitle: None,
            contributors: vec![book_model::Contributor::new(author, book_model::MarcRelatorCode(*b"aut")).unwrap()],
            description: "A synopsis.".to_owned(),
            book: book_model::BookMetadata::default(),
        },
        audiobook: None,
        toc: Vec::new(),
    }
}

fn commit_book(database: &Database, parent: &sync_common::DirId, digit: &str, title: &str, author: &str, file_name: &str) -> sync_common::ContentHash {
    use crate::import::{ImportCommit, PublishedImport};
    let hash = sync_common::ContentHash::new(&digit.repeat(64));
    database
        .commit_import(ImportCommit {
            parent_id: parent.clone(),
            file_name: file_name.to_owned(),
            format: book_model::BookFormat::Epub,
            content_hash: hash,
            checksum: sync_common::ContentHash::new(&"a".repeat(64)),
            size_bytes: 1024,
            inspection_version: 1,
            inspection: Some(inspected(title, author)),
            published: PublishedImport { name: file_name.to_owned(), relative_path: file_name.to_owned(), published: true, fingerprint: None },
            request_thumbnail: false,
            restore_paths: Vec::new(),
        })
        .expect("commit import");
    hash
}

#[test]
fn library_search_finds_title_and_author_matches() {
    let holder = open_test_database();
    let database = &holder.database;
    let parent = database.create_directory(&sync_common::ROOT_DIR_ID, &"Fiction".to_owned()).expect("create folder");
    let rust = commit_book(database, &parent.id, "9", "The Rust Programming Language", "Steve Klabnik", "rust.epub");
    let cooking = commit_book(database, &parent.id, "8", "Goran's Cookbook", "Ada Lovelace", "cookbook.epub");

    let titles = database.browse_library_search("rust", false).expect("title search");
    assert_eq!(titles.title_matches.iter().map(|card| card.content_hash).collect::<Vec<_>>(), vec![rust]);
    assert!(titles.author_matches.is_empty());

    let authors = database.browse_library_search("lovelace", false).expect("author search");
    assert!(authors.title_matches.is_empty());
    assert_eq!(authors.author_matches.iter().map(|card| card.content_hash).collect::<Vec<_>>(), vec![cooking]);

    let empty = database.browse_library_search("", false).expect("empty search");
    assert!(empty.title_matches.is_empty() && empty.author_matches.is_empty());
    let missing = database.browse_library_search("missing", false).expect("missing search");
    assert!(missing.title_matches.is_empty() && missing.author_matches.is_empty());
}

fn page_query(location: String) -> library_model::LibraryBrowseQuery {
    library_model::LibraryBrowseQuery {
        location,
        search: String::new(),
        file_types: vec![],
        languages: vec![],
        chip_sort: BrowseChipSort::Alphabetical,
        book_sort: BrowseBookSort::Alphabetical,
        hide_finished: false,
        include_direct_child_books: false,
    }
}

#[test]
fn complete_pages_keep_cards_facets_and_paths_on_one_snapshot() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    for subject in [false, true] {
        let holder = open_test_database();
        let db = &holder.database;
        let directory = db.create_directory(&sync_common::ROOT_DIR_ID, &"Original folder".into()).unwrap().id;
        commit_book(db, &directory, "1", "Original book", "Writer", "book.epub");
        db.connection.execute("INSERT INTO book_language SELECT row_id,0,'en-US' FROM book", []).unwrap();
        db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
        let query = page_query(if subject { "Subject".into() } else { directory.to_string() });
        let browse = || if subject { db.browse_subject(&query, false) } else { db.browse_folder(&query, false) };
        // Build membership first so the hook only intercepts a clean read.
        browse().unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let observed = fired.clone();
        let mutation = Arc::new(Mutex::new(None));
        let result = mutation.clone();
        let path = holder.path.clone();
        db.connection.authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(context.action, AuthAction::Read { table_name: "book", column_name: "title" }) && !observed.swap(true, Ordering::SeqCst) {
                let update = (|| -> Result<(), crate::DatabaseError> {
                    let writer = Database::open(&path)?;
                    writer.connection.execute_batch("UPDATE sync_metadata SET change_origin='remote'; UPDATE book SET title='Changed book',format='pdf'; UPDATE book_language SET language_tag='sv';")?;
                    writer.connection.execute("UPDATE dir SET name='Changed folder' WHERE id=?1", [directory.to_string()])?;
                    Ok(())
                })();
                *result.lock().unwrap() = Some(update.map_err(|error| error.to_string()));
            }
            Authorization::Allow
        }))
        .unwrap();
        let page = browse().unwrap();
        db.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
        assert!(fired.load(Ordering::SeqCst), "concurrent mutation must be exercised");
        assert_eq!(mutation.lock().unwrap().as_ref(), Some(&Ok(())), "WAL writer must complete during browsing");
        assert_eq!(page.contents.books[0].title, "Original book");
        assert_eq!(page.format_counts[0].format, LibraryFileTypeFilter::Book);
        assert_eq!(page.language_counts[0].language, "en");
        if !subject {
            assert_eq!(page.path.last().unwrap().name, "Original folder");
        }
        let updated = browse().unwrap();
        assert_eq!(updated.contents.books[0].title, "Changed book");
        assert_eq!(updated.format_counts[0].format, LibraryFileTypeFilter::Pdf);
        assert_eq!(updated.language_counts[0].language, "sv");
        if !subject {
            assert_eq!(updated.path.last().unwrap().name, "Changed folder");
        }
    }
}

#[test]
fn card_queries_return_the_same_named_fields() {
    let holder = open_test_database();
    let db = &holder.database;
    let directory = sync_common::ROOT_DIR_ID;
    let hash = commit_book(db, &directory, "2", "First book", "Shared author", "first.epub");
    let other = commit_book(db, &directory, "3", "Second book", "Shared author", "second.epub");
    db.connection.execute_batch("UPDATE book SET added_at=row_id*1000,read_progress=25;").unwrap();
    db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
    db.connection.execute("UPDATE sync_metadata SET change_origin='local'", []).unwrap();
    db.update_reading_position(&hash, "epubcfi(/6/2!/4/2/1:0)", Some(25.), None).unwrap();
    db.connection.execute("INSERT OR IGNORE INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
    let expected = db.browse_book_detail(hash).unwrap().book;
    assert_eq!(expected.added_at, 1000);
    let home = db.browse_home(10, false).unwrap();
    let authors = db.browse_authors(library_model::AuthorSort::Name, false).unwrap();
    let related = db.browse_book_detail(other).unwrap().related;
    let subject = db.browse_subject_contents(&page_query("Subject".into()), false).unwrap();
    let search = db.browse_library_search("first", false).unwrap();
    for cards in [&home.most_progress, &home.recently_read, &home.recently_added, &authors.authors[0].books, &related, &subject.books, &search.title_matches] {
        assert_eq!(cards.iter().find(|card| card.content_hash == hash).unwrap(), &expected);
    }
    let folder = db.browse_library_folder(&directory, false, LibraryFileTypeFilter::Book).unwrap();
    let card = folder.books.iter().find(|card| card.content_hash == hash).unwrap();
    assert_eq!(card.added_at, expected.added_at);
    assert_eq!(card.source_directory, Some(directory));
}

#[test]
fn nullable_card_fields_do_not_hide_invalid_data() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let sql = "SELECT ?1 AS title, ?2 AS audiobook_duration_ms, ?3 AS source_directory,
        '0000000000000000000000000000000000000000000000000000000000000000' AS content_hash,
        NULL AS author,NULL AS subtitle,NULL AS description,NULL AS progress,
        0 AS downloaded,0 AS download_requested,1 AS format_category,
        NULL AS audiobook_chapter_count,NULL AS added_at";
    let null = rusqlite::types::Value::Null;
    let card = connection.query_row(sql, [&null, &null, &null], read_book_card_row).unwrap();
    assert!(card.title.starts_with("Untitled"));
    assert_eq!(card.added_at, 0);
    for values in [[rusqlite::types::Value::Integer(7), null.clone(), null.clone()], [null.clone(), rusqlite::types::Value::Integer(-1), null.clone()], [null.clone(), null.clone(), rusqlite::types::Value::Text("invalid directory".into())]]
    {
        assert!(connection.query_row(sql, values, read_book_card_row).is_err());
    }
    assert!(connection.query_row(&sql.replace("AS source_directory", "AS missing_column"), [&null, &null, &null], read_book_card_row).is_err());
}

#[test]
fn scoped_search_filters_card_formats_and_membership() {
    let holder = open_test_database();
    let db = &holder.database;
    let shelf = db.create_directory(&sync_common::ROOT_DIR_ID, &"Shelf".into()).unwrap().id;
    let nested = db.create_directory(&shelf, &"Nested".into()).unwrap().id;
    let pdf = commit_book(db, &nested, "4", "Shared title", "Writer", "first.pdf");
    let epub = commit_book(db, &shelf, "5", "Another title", "Shared author", "second.epub");
    commit_book(db, &sync_common::ROOT_DIR_ID, "6", "Shared elsewhere", "Writer", "outside.epub");
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'",[]).unwrap();
    db.connection.execute("UPDATE book SET format='pdf' WHERE content_hash=?1", [pdf.as_str()]).unwrap();
    db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book WHERE content_hash=?1", [pdf.as_str()]).unwrap();
    let pdf_result = db.browse_library_search_folder(&shelf, "shared", LibraryFileTypeFilter::Pdf, false).unwrap();
    assert_eq!(pdf_result.title_matches.iter().map(|card| card.content_hash).collect::<Vec<_>>(), [pdf]);
    assert!(pdf_result.author_matches.is_empty());
    let epub_result = db.browse_library_search_folder(&shelf, "shared", LibraryFileTypeFilter::Book, false).unwrap();
    assert!(epub_result.title_matches.is_empty());
    assert_eq!(epub_result.author_matches.iter().map(|card| card.content_hash).collect::<Vec<_>>(), [epub]);
    let all = db.browse_library_search_subject(None, "shared", LibraryFileTypeFilter::Pdf, false).unwrap();
    assert_eq!(all, pdf_result);
    let assigned = db.browse_library_search_subject(Some("Subject"), "shared", LibraryFileTypeFilter::All, false).unwrap();
    assert_eq!(assigned, pdf_result);
    assert!(db.browse_library_search_subject(Some("Subject"), "shared", LibraryFileTypeFilter::Book, false).unwrap().author_matches.is_empty());
    let mut query = page_query(shelf.to_string());
    query.search = "shared".into();
    query.file_types = vec![LibraryFileTypeFilter::Pdf];
    assert_eq!(db.browse_folder(&query, false).unwrap().contents.books.iter().map(|card| card.content_hash).collect::<Vec<_>>(), [pdf]);
}

#[test]
fn blank_language_selections_and_home_limits() {
    let holder = open_test_database();
    let db = &holder.database;
    let hash = commit_book(db, &sync_common::ROOT_DIR_ID, "7", "Book", "Author", "book.epub");
    db.update_reading_position(&hash, "epubcfi(/6/2!/4/2/1:0)", Some(25.), None).unwrap();
    db.connection.execute("INSERT INTO book_language SELECT row_id,0,'en-US' FROM book", []).unwrap();
    db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
    for location in [sync_common::ROOT_DIR_ID.to_string(), "Subject".into()] {
        let mut query = page_query(location.clone());
        let browse = |query: &library_model::LibraryBrowseQuery| if location == "Subject" { db.browse_subject(query, false) } else { db.browse_folder(query, false) }.unwrap();
        let expected = browse(&query);
        for languages in [vec!["".into(), "  ".into()], vec!["".into(), " EN_us ".into(), "en-GB".into()]] {
            query.languages = languages;
            let actual = browse(&query);
            assert_eq!(actual.contents, expected.contents);
            assert_eq!(actual.language_counts, expected.language_counts);
            assert_eq!(actual.format_counts, expected.format_counts);
        }
    }
    for limit in [-1, i32::MIN] {
        assert!(db.browse_home(limit, false).unwrap_err().to_string().contains("limit"));
    }
    for limit in [0, 1] {
        let view = db.browse_home(limit, false).unwrap();
        assert_eq!(view.most_progress.len(), limit as usize);
        assert_eq!(view.recently_read.len(), limit as usize);
        assert_eq!(view.recently_added.len(), limit as usize);
    }
}

#[test]
fn home_authors_and_detail_hold_a_snapshot_between_component_queries() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{Arc, Mutex};
    for (view, table, column) in [("home", "sync_state_version", "changed_at"), ("authors", "book_unified_concept", "book_row_id"), ("detail", "book_dir", "dir_id")] {
        let holder = open_test_database();
        let db = &holder.database;
        let directory = db.create_directory(&sync_common::ROOT_DIR_ID, &"Original folder".into()).unwrap().id;
        let hash = commit_book(db, &directory, "8", "Original book", "Author", "book.epub");
        db.update_reading_position(&hash, "epubcfi(/6/2!/4/2/1:0)", Some(25.), None).unwrap();
    db.connection.execute("INSERT OR IGNORE INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
        let outcome = Arc::new(Mutex::new(None));
        let observed = outcome.clone();
        let path = holder.path.clone();
        db.connection.authorizer(Some(move |context: AuthContext<'_>| {
            if let AuthAction::Read { table_name, column_name } = context.action {
                let mut result = observed.lock().unwrap();
                if table_name == table && column_name == column && result.is_none() {
                    *result = Some(
                        (|| -> Result<(), crate::DatabaseError> {
                            let writer = Database::open(&path)?;
                            writer.connection.execute_batch("UPDATE sync_metadata SET change_origin='remote'; UPDATE book SET title='Changed'; UPDATE book_dir SET deleted_at=1;")?;
                            Ok(())
                        })()
                        .map_err(|error| error.to_string()),
                    );
                }
            }
            Authorization::Allow
        }))
        .unwrap();
        match view {
            "home" => {
                let home = db.browse_home(10, false).unwrap();
                for cards in [home.most_progress, home.recently_read, home.recently_added] {
                    assert_eq!(cards.len(), 1);
                    assert_eq!(cards[0].title, "Original book");
                }
            }
            "authors" => {
                let authors = db.browse_authors(library_model::AuthorSort::Name, false).unwrap().authors;
                assert_eq!(authors.len(), 1);
                assert_eq!(authors[0].book_count, 1);
                assert_eq!(authors[0].books[0].title, "Original book");
            }
            _ => {
                let detail = db.browse_book_detail(hash).unwrap();
                assert_eq!(detail.book.title, "Original book");
                assert_eq!(detail.source_directory_id, Some(directory.to_string()));
                assert_eq!(detail.path.last().unwrap().name, "Original folder");
            }
        }
        db.connection.authorizer(None::<fn(AuthContext<'_>) -> Authorization>).unwrap();
        assert_eq!(outcome.lock().unwrap().as_ref(), Some(&Ok(())), "{view}: writer ran between component queries");
        assert_eq!(db.browse_book_count().unwrap(), 0, "{view}: snapshot released");
        assert!(db.connection.is_autocommit());
    }
}

#[test]
fn search_keeps_classification_and_membership_on_one_snapshot() {
    use rusqlite::functions::FunctionFlags;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for scope in ["library", "folder", "subject"] {
        let holder = open_test_database();
        let db = &holder.database;
        let directory = db.create_directory(&sync_common::ROOT_DIR_ID, &"Folder".into()).unwrap().id;
        let hash = commit_book(db, &directory, "9", "Shared title", "Shared author", "book.epub");
        db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
        let browse = || match scope {
            "folder" => db.browse_library_search_folder(&directory, "shared", LibraryFileTypeFilter::All, false),
            "subject" => db.browse_library_search_subject(Some("Subject"), "shared", LibraryFileTypeFilter::All, false),
            _ => db.browse_library_search("shared", false),
        };
        browse().unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let observed = fired.clone();
        let path = holder.path.clone();
        // Mutate while the search statement is executing, after its snapshot starts.
        db.connection
            .create_scalar_function("instr", 2, FunctionFlags::SQLITE_UTF8, move |context| {
                let text = context.get::<String>(0)?;
                let needle = context.get::<String>(1)?;
                if needle == "shared" && !observed.swap(true, Ordering::SeqCst) {
                    let update = (|| -> Result<(), crate::DatabaseError> {
                        let writer = Database::open(&path)?;
                        writer.connection.execute_batch("UPDATE sync_metadata SET change_origin='remote'; UPDATE book SET title='Changed'; UPDATE book_dir SET deleted_at=1;")?;
                        Ok(())
                    })();
                    update.map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
                }
                Ok(text.find(&needle).map_or(0, |offset| text[..offset].chars().count() + 1) as i64)
            })
            .unwrap();
        let result = browse().unwrap();
        assert!(fired.load(Ordering::SeqCst), "{scope}");
        assert_eq!(result.title_matches.len(), 1);
        assert_eq!(result.title_matches[0].content_hash, hash);
        assert_eq!(result.title_matches[0].title, "Shared title");
        assert!(result.author_matches.is_empty());
        let next = browse().unwrap();
        assert!(next.title_matches.is_empty() && next.author_matches.is_empty());
    }
}

#[test]
fn scoped_search_cost_does_not_grow_with_unrelated_books() {
    use rusqlite::{StatementStatus, named_params};
    let holder = open_test_database();
    let db = &holder.database;
    let directory = db.create_directory(&sync_common::ROOT_DIR_ID, &"Small shelf".into()).unwrap().id;
    commit_book(db, &directory, "a", "Shared title", "Writer", "book.epub");
    db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
    let measure = || {
        db.browse_library_search_folder(&directory, "shared", LibraryFileTypeFilter::All, false).unwrap();
        db.browse_library_search_subject(Some("Subject"), "shared", LibraryFileTypeFilter::All, false).unwrap();
        ["folder", "subject"].map(|scope| {
            let source = include_str!("sql/search.sql");
            let name = format!("-- name: search_{scope}_cards?\n").replace(
                "\n", "
",
            );
            let sql = source.split(&name).nth(1).unwrap().split("-- name:").next().unwrap();
            let mut statement = db.connection.prepare(sql).unwrap();
            let directory = directory.to_string();
            if scope == "folder" {
                let cards = statement.query_map(named_params! {":directory_id":directory,":normalized_query":"shared",":downloaded_only":false,":file_type":0}, read_book_card_row).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
                assert_eq!(cards.len(), 1);
            } else {
                let cards = statement.query_map(named_params! {":route_id":0,":normalized_query":"shared",":downloaded_only":false,":file_type":0}, read_book_card_row).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
                assert_eq!(cards.len(), 1);
            }
            (statement.get_status(StatementStatus::VmStep), statement.get_status(StatementStatus::FullscanStep))
        })
    };
    let before = measure();
    db.connection.execute_batch(
        "UPDATE sync_metadata SET change_origin='remote';
        WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<2000)
        INSERT INTO book(content_hash,title,format) SELECT printf('%064x',i),'Shared unrelated','epub' FROM n;
        INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded)
        SELECT '00000000-0000-0000-0000-000000000000',row_id,content_hash,'',1 FROM book WHERE title='Shared unrelated';",
    )
    .unwrap();
    let after = measure();
    for (before, after) in before.into_iter().zip(after) {
        assert_eq!(after.1, 0, "scoped search must use indexed membership");
        assert!(after.0 <= before.0 * 3, "unrelated books increased query work: {before:?} -> {after:?}");
    }
}

#[test]
fn failed_view_read_releases_its_snapshot() {
    let holder = open_test_database();
    let db = &holder.database;
    let missing = sync_common::ContentHash::new(&"f".repeat(64));
    assert!(db.browse_book_detail(missing).is_err());
    assert!(db.connection.is_autocommit());
    commit_book(db, &sync_common::ROOT_DIR_ID, "b", "Recovered", "Writer", "book.epub");
    assert_eq!(db.browse_home(1, false).unwrap().recently_added.len(), 1);
}

fn assert_untitled_search(db: &Database, hash: sync_common::ContentHash, search: &str, expected_count: usize) {
    let global = db.browse_library_search(search, false).unwrap();
    let folder = db.browse_library_search_folder(&sync_common::ROOT_DIR_ID, search, LibraryFileTypeFilter::All, false).unwrap();
    let subject = db.browse_library_search_subject(Some("Subject"), search, LibraryFileTypeFilter::All, false).unwrap();
    assert_eq!(folder, global);
    assert_eq!(subject, global);
    assert_eq!(global.title_matches.len(), expected_count, "{search}");
    assert!(global.author_matches.is_empty());
    if expected_count != 0 {
        assert_eq!(global.title_matches[0].content_hash, hash);
    }
    for location in [sync_common::ROOT_DIR_ID.to_string(), "Subject".into()] {
        let mut query = page_query(location.clone());
        query.search = search.into();
        let page = if location == "Subject" { db.browse_subject(&query, false) } else { db.browse_folder(&query, false) }.unwrap();
        assert_eq!(page.contents.books.len(), expected_count, "{location}: {search}");
        assert_eq!(page.format_counts.iter().map(|count| count.book_count as usize).sum::<usize>(), expected_count);
        assert_eq!(page.language_counts.iter().map(|count| count.book_count as usize).sum::<usize>(), expected_count);
        if expected_count != 0 {
            assert_eq!(page.contents.books[0].content_hash, hash);
            assert_eq!(page.format_counts[0].format, LibraryFileTypeFilter::Book);
            assert_eq!(page.language_counts[0].language, "en");
        }
    }
}

#[test]
fn untitled_books_share_search_matches_and_facets_across_views() {
    let holder = open_test_database();
    let db = &holder.database;
    // This browse fixture owns derived rows, including the distinction between NULL and empty title.
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'",[]).unwrap();
    let hash = sync_common::ContentHash::new(&"c".repeat(64));
    db.connection.execute("INSERT INTO book(content_hash,format,subtitle) VALUES(?1,'epub','Écology')", [hash.as_str()]).unwrap();
    db.connection.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) SELECT ?1,row_id,'untitled.epub','',1 FROM book", [sync_common::ROOT_DIR_ID.to_string()]).unwrap();
    db.sync_publishable_mutations().unwrap();
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'",[]).unwrap();
    db.connection.execute("INSERT INTO book_language SELECT row_id,0,'en' FROM book", []).unwrap();
    db.connection.execute("INSERT INTO book_unified_concept SELECT row_id,(SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 LIMIT 1),1 FROM book", []).unwrap();
    for search in ["untitled", hash.as_str(), "ecology"] {
        assert_untitled_search(db, hash, search, 1);
    }
    let display = format!("Untitled ({hash}): Écology");
    assert_untitled_search(db, hash, &display, 1);
    assert_eq!(db.browse_book_detail(hash).unwrap().book.title, format!("Untitled ({hash})"));
    assert_eq!(db.connection.query_row("SELECT sort_title FROM book_browse_text", [], |row| row.get::<_, String>(0)).unwrap(), "", "search fallback does not change alphabetical ordering");

    for title in ["Named book", ""] {
        db.connection.execute("UPDATE book SET title=?1", [title]).unwrap();
        assert_untitled_search(db, hash, "untitled", 0);
        assert_untitled_search(db, hash, hash.as_str(), 0);
        assert_untitled_search(db, hash, "ecology", 1);
    }
    db.connection.execute_batch("UPDATE book SET title=NULL,subtitle=NULL;").unwrap();
    assert_untitled_search(db, hash, "untitled", 1);
    assert_untitled_search(db, hash, "ecology", 0);
}
