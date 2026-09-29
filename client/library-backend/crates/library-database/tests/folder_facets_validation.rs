//! Equivalence and opt-in release benchmarks for combined folder facets.
use library_database::{
    Database,
    rusqlite::{named_params, params},
};
use std::time::{Duration, Instant};
const SOURCE: &str = concat!(
    include_str!("../src/browse/sql/folders.sql"),
    "
",
    include_str!("../src/browse/sql/counts.sql")
);
const PREVIOUS: &str = include_str!("fixtures/folder_facets_split.sql");
const ROOT: &str = "00000000-0000-0000-0000-000000000000";
const TARGET: &str = "00000000-0000-0000-0000-000000000001";
const CHILD: &str = "00000000-0000-0000-0000-000000000002";
const HIDDEN: &str = "00000000-0000-0000-0000-000000000003";
const OTHER: &str = "00000000-0000-0000-0000-000000000004";
fn query<'a>(source: &'a str, name: &str) -> &'a str {
    source.split(&format!("-- name: {name}?\n")).nth(1).unwrap().split("-- name:").next().unwrap()
}
fn fixture(books: usize) -> Database {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    let tx = db.connection.unchecked_transaction().unwrap();
    for (id, parent, name) in [(TARGET, ROOT, "Target"), (CHILD, TARGET, "Nested"), (HIDDEN, TARGET, ".Hidden"), (OTHER, ROOT, "Targetish")] {
        tx.execute("INSERT INTO dir(id,parent_id,name) VALUES(?1,?2,?3)", [id, parent, name]).unwrap();
    }
    for i in 1..=books as i64 {
        tx.execute("INSERT INTO book(content_hash,title,format) VALUES(?1,?2,?3)", params![format!("{i:064x}"), format!("Book {i}"), ["epub", "pdf", "m4b"][i as usize % 3]]).unwrap();
        tx.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,?2,?3,'',?4)", params![[TARGET, CHILD, HIDDEN, OTHER][i as usize % 4], i, format!("{i}.epub"), i % 2]).unwrap();
        if i % 11 == 0 {
            tx.execute("INSERT OR IGNORE INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,?2,?3,'',1)", params![OTHER, i, format!("extra{i}.epub")]).unwrap();
        }
        tx.execute("INSERT INTO book_language VALUES(?1,0,?2)", params![i, if i % 3 == 0 { "sv" } else { "en-US" }]).unwrap();
        if i % 7 == 0 {
            tx.execute("INSERT INTO book_language VALUES(?1,1,'en-GB')", [i]).unwrap();
        }
    }
    tx.commit().unwrap();
    refresh(&db);
    db
}
fn refresh(db: &Database) {
    db.browse_folder_facet_counts(ROOT, "", &[], &[], false).unwrap();
}
type Rows = Vec<(i64, String, i64)>;
fn combined(db: &Database, folder: &str, search: &str, formats: i32, languages: &str, downloaded: bool) -> Rows {
    db.connection.prepare_cached(query(SOURCE, "get_folder_facets"))
        .unwrap()
        .query_map(named_params! {":dir_id":folder,":normalized_query":search,":file_types":formats,":languages":languages,":downloaded_only":downloaded}, |r| {
            let kind = r.get::<_, i64>(0)?;
            let value = if kind == 0 { r.get::<_, i64>(1)?.to_string() } else { r.get::<_, String>(1)? };
            Ok((kind, value, r.get(2)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
fn split(db: &Database, folder: &str, search: &str, formats: i32, languages: &str, downloaded: bool) -> Rows {
    let mut result = Vec::new();
    for (kind, name) in [(0, "get_folder_format_facets"), (1, "get_folder_language_facets")] {
        let mut stmt = db.connection.prepare_cached(query(PREVIOUS, name)).unwrap();
        for (key, value) in [(":dir_id", folder), (":normalized_query", search), (":languages", languages)] {
            if let Some(i) = stmt.parameter_index(key).unwrap() {
                stmt.raw_bind_parameter(i, value).unwrap();
            }
        }
        for (key, value) in [(":file_types", i64::from(formats)), (":downloaded_only", i64::from(downloaded))] {
            if let Some(i) = stmt.parameter_index(key).unwrap() {
                stmt.raw_bind_parameter(i, value).unwrap();
            }
        }
        let mut rows = stmt.raw_query();
        while let Some(r) = rows.next().unwrap() {
            result.push((kind, if kind == 0 { r.get::<_, i64>(0).unwrap().to_string() } else { r.get::<_, String>(0).unwrap() }, r.get(1).unwrap()));
        }
    }
    result
}
#[test]
fn folder_facets_preserve_counts_across_filters_and_hierarchy_changes() {
    let db = fixture(160);
    for mutation in [
        "SELECT 1",
        "UPDATE book SET hidden_at=1 WHERE row_id%5=0; UPDATE book SET deleted_at=1 WHERE row_id%7=0",
        "UPDATE book_dir SET deleted_at=1 WHERE book_row_id%3=0",
        "UPDATE dir SET deleted_at=1 WHERE name='Target'",
        "UPDATE dir SET deleted_at=NULL; UPDATE book SET hidden_at=NULL,deleted_at=NULL; UPDATE book_dir SET deleted_at=NULL",
        "UPDATE dir SET name='Visible' WHERE name='.Hidden'; UPDATE dir SET parent_id='00000000-0000-0000-0000-000000000004' WHERE name='Nested'",
    ] {
        db.connection.execute_batch(mutation).unwrap();
        refresh(&db);
        for folder in [ROOT, TARGET, CHILD, "00000000-0000-0000-0000-000000000099"] {
            for search in ["", "book 1", "nested", "hidden", "targetish", "qzx-no-match"] {
                for formats in [0, 1, 2, 4, 3] {
                    for languages in ["", ",en,", ",sv,", ",en,sv,", ",xx,"] {
                        for downloaded in [false, true] {
                            assert_eq!(
                                combined(&db, folder, search, formats, languages, downloaded),
                                split(&db, folder, search, formats, languages, downloaded),
                                "mutation={mutation}, folder={folder}, search={search}, formats={formats}, languages={languages}, downloaded={downloaded}"
                            );
                        }
                    }
                }
            }
        }
    }
    // Public decoding preserves selected, unavailable choices at zero.
    let (languages, formats) = db.browse_folder_facet_counts(TARGET, "qzx-no-match", &[library_model::LibraryFileTypeFilter::Pdf], &["xx"], false).unwrap();
    assert_eq!(languages.len(), 1);
    assert_eq!(languages[0].language, "xx");
    assert_eq!(languages[0].book_count, 0);
    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].format, library_model::LibraryFileTypeFilter::Pdf);
    assert_eq!(formats[0].book_count, 0);
    let mut stmt = db.connection.prepare(&format!("EXPLAIN QUERY PLAN {}", query(SOURCE, "get_folder_facets"))).unwrap();
    let plan = stmt.query_map(named_params! {":dir_id":ROOT,":normalized_query":"nested",":file_types":0,":languages":"",":downloaded_only":false}, |r| r.get::<_, String>(3)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(plan.iter().filter(|s| s.as_str() == "MATERIALIZE search_matches").count(), 1, "{plan:?}");
}
fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}
#[test]
#[ignore = "release benchmark; run explicitly with --ignored --nocapture --test-threads=1"]
fn folder_facet_combination_performance() {
    for size in [1000, 10000] {
        let db = fixture(size);
        for (case, folder, search, formats, languages, downloaded) in [
            ("root", ROOT, "", 0, "", false),
            ("folder", TARGET, "", 0, "", false),
            ("title", TARGET, "book 1", 0, "", false),
            ("folder_name", TARGET, "nested", 0, "", false),
            ("miss", TARGET, "qzx-no-match", 0, "", false),
            ("filters", TARGET, "", 2, ",en,", true),
            ("filtered_search", TARGET, "book 1", 2, ",en,", true),
        ] {
            assert_eq!(combined(&db, folder, search, formats, languages, downloaded), split(&db, folder, search, formats, languages, downloaded));
            let mut before = Vec::new();
            let mut after = Vec::new();
            for i in 0..11 {
                for old in [i % 2 == 0, i % 2 != 0] {
                    let t = Instant::now();
                    let rows = if old { split(&db, folder, search, formats, languages, downloaded) } else { combined(&db, folder, search, formats, languages, downloaded) };
                    std::hint::black_box(rows);
                    let elapsed = t.elapsed();
                    if i > 0 {
                        if old { before.push(elapsed) } else { after.push(elapsed) }
                    }
                }
            }
            println!("FOLDER books={size} case={case} split_us={} combined_us={}", median(before).as_micros(), median(after).as_micros());
        }
    }
}

#[test]
fn folder_name_search_uses_parent_identity_even_in_detached_subtrees() {
    let db = fixture(16);
    // A slash in a sibling name must not make it a descendant of Target.
    db.connection.execute("UPDATE dir SET name='Target/Nested' WHERE id=?1", [OTHER]).unwrap();
    let foreign = "00000000-0000-0000-0000-000000000005";
    db.connection.execute("INSERT INTO dir(id,parent_id,name) VALUES(?1,?2,'Foreign')", [foreign, OTHER]).unwrap();
    db.connection.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash) VALUES(?1,1,'copy.epub','')", [foreign]).unwrap();
    assert!(db.browse_folder_facet_counts(TARGET, "foreign", &[], &[], false).unwrap().1.is_empty());
    // A live subtree remains searchable when an ancestor is deleted.
    let ancestor = "00000000-0000-0000-0000-000000000006";
    db.connection.execute("INSERT INTO dir(id,parent_id,name) VALUES(?1,?2,'Ancestor')", [ancestor, ROOT]).unwrap();
    db.connection.execute("UPDATE dir SET parent_id=?1 WHERE id=?2", [ancestor, TARGET]).unwrap();
    db.connection.execute("UPDATE dir SET deleted_at=1 WHERE id=?1", [ancestor]).unwrap();
    let counts = db.browse_folder_facet_counts(TARGET, "nested", &[], &[], false).unwrap().1;
    assert_eq!(counts.iter().map(|c| c.book_count).sum::<u32>(), 4);
}

#[test]
fn cached_descendants_and_format_projection_follow_mutations() {
    let db = fixture(32);
    let folder = sync_common::DirId::parse_str(TARGET).unwrap();
    for mutation in [
        "SELECT 1",
        "UPDATE book_dir SET deleted_at=1 WHERE book_row_id%3=0",
        "UPDATE book SET hidden_at=1 WHERE row_id%5=0; UPDATE book SET deleted_at=1 WHERE row_id%7=0",
        "UPDATE dir SET parent_id='00000000-0000-0000-0000-000000000004' WHERE name='Nested'",
        "UPDATE dir SET deleted_at=1 WHERE name='Target'",
        "UPDATE dir SET deleted_at=NULL; UPDATE book SET format='M4B' WHERE row_id%2=0",
    ] {
        db.connection.execute_batch(mutation).unwrap();
        // No manual refresh: the public search must refresh dirty membership.
        let actual = db.browse_library_search_folder(&folder, "book", library_model::LibraryFileTypeFilter::All, false).unwrap();
        let mut hashes = actual.title_matches.iter().map(|b| b.content_hash.to_string()).collect::<Vec<_>>();
        hashes.sort();
        let mut stmt = db.connection.prepare("WITH RECURSIVE subtree(id) AS (SELECT id FROM dir WHERE id=?1 AND deleted_at IS NULL UNION SELECT d.id FROM dir d JOIN subtree s ON d.parent_id=s.id WHERE d.id!=d.parent_id AND d.deleted_at IS NULL) SELECT DISTINCT b.content_hash FROM book b JOIN book_dir p ON p.book_row_id=b.row_id JOIN subtree s ON s.id=p.dir_id WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL AND p.deleted_at IS NULL ORDER BY b.content_hash").unwrap();
        let expected = stmt.query_map([TARGET], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(hashes, expected, "{mutation}");
        let read_counts = |sql: &str| db.connection.prepare(sql).unwrap().query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            read_counts(query(SOURCE, "library_format_counts_select")),
            read_counts(
                "SELECT CASE lower(b.format) WHEN 'pdf' THEN 2 WHEN 'm4b' THEN 4 ELSE 1 END AS category,COUNT(*) FROM book b WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL AND EXISTS(SELECT 1 FROM book_dir p WHERE p.book_row_id=b.row_id AND p.deleted_at IS NULL) GROUP BY category ORDER BY category"
            ),
            "{mutation}"
        );
    }
}

#[test]
#[ignore = "release comparison of recursive membership and format classification"]
fn membership_and_format_performance() {
    let db = fixture(10000);
    for (case, old, new) in [
        (
            "membership",
            "WITH RECURSIVE subtree(id) AS (SELECT id FROM dir WHERE id=:dir_id AND deleted_at IS NULL UNION SELECT d.id FROM dir d JOIN subtree s ON d.parent_id=s.id WHERE d.id!=d.parent_id AND d.deleted_at IS NULL) SELECT DISTINCT b.content_hash FROM book b JOIN book_dir p ON p.book_row_id=b.row_id JOIN subtree s ON s.id=p.dir_id WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL AND p.deleted_at IS NULL ORDER BY b.content_hash",
            query(SOURCE, "get_descendant_content_hashes"),
        ),
        (
            "formats",
            "SELECT CASE lower(b.format) WHEN 'pdf' THEN 2 WHEN 'm4b' THEN 4 ELSE 1 END AS category,COUNT(*) FROM book b WHERE b.deleted_at IS NULL AND b.hidden_at IS NULL AND EXISTS(SELECT 1 FROM book_dir p WHERE p.book_row_id=b.row_id AND p.deleted_at IS NULL) GROUP BY category ORDER BY category",
            query(SOURCE, "library_format_counts_select"),
        ),
    ] {
        let read = |sql: &str| {
            let mut stmt = db.connection.prepare_cached(sql).unwrap();
            if let Some(index) = stmt.parameter_index(":dir_id").unwrap() {
                stmt.raw_bind_parameter(index, TARGET).unwrap();
            }
            let columns = stmt.column_count();
            let mut cursor = stmt.raw_query();
            let mut rows = Vec::new();
            while let Some(row) = cursor.next().unwrap() {
                rows.push((0..columns).map(|i| row.get::<_, library_database::rusqlite::types::Value>(i).unwrap()).collect::<Vec<_>>());
            }
            rows
        };
        assert_eq!(read(old), read(new));
        let mut before = Vec::new();
        let mut after = Vec::new();
        for i in 0..21 {
            for baseline in [i % 2 == 0, i % 2 != 0] {
                let start = Instant::now();
                std::hint::black_box(read(if baseline { old } else { new }));
                let elapsed = start.elapsed();
                if i > 0 {
                    if baseline { before.push(elapsed) } else { after.push(elapsed) }
                }
            }
        }
        println!("SIMPLIFICATION books=10000 case={case} old_us={} new_us={}", median(before).as_micros(), median(after).as_micros());
    }
}
