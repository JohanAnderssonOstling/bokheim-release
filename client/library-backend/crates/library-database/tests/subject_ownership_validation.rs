//! Correctness checks and an opt-in release benchmark for subject ownership removal.
use library_database::{
    Database,
    rusqlite::{StatementStatus, named_params, params},
};
use std::time::{Duration, Instant};
const SOURCE: &str = concat!(
    include_str!("../src/browse/sql/subjects.sql"),
    "
",
    include_str!("../src/browse/sql/subject_cache.sql")
);
const OLD_FORMAT: &str = include_str!("fixtures/subject_format_facets_before.sql");
const OLD_LANGUAGE: &str = include_str!("fixtures/subject_language_facets_before.sql");

fn sql(name: &str) -> &str {
    SOURCE.split(&format!("-- name: {name}")).nth(1).unwrap().split("-- name:").next().unwrap()
}
fn facet_sql(kind: i64) -> String {
    let combined = sql("get_subject_facets?\n").trim().trim_end_matches(';');
    format!("SELECT value,book_count FROM ({combined}) WHERE kind={kind}")
}
fn old_sql(source: &str) -> String {
    format!("WITH RECURSIVE descendant_routes{}", source.split("), descendant_routes").nth(1).unwrap())
}
fn fixture(books: usize) -> (Database, i64, String) {
    let db = Database::open(":memory:").unwrap();
    db.initialize_library().unwrap();
    db.connection.execute("UPDATE sync_metadata SET change_origin='remote'", []).unwrap();
    let concepts = db.connection
        .prepare("SELECT concept_id FROM curated.unified_concept_route WHERE concept_id>0 GROUP BY concept_id HAVING count(*)>1 ORDER BY concept_id LIMIT 2")
        .unwrap()
        .query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(concepts.len(), 2);
    let (route, label): (i64, String) = db.connection
        .query_row("SELECT r.route_id,c.preferred_label FROM curated.unified_concept_route r JOIN curated.concept c USING(concept_id) WHERE r.concept_id=?1 ORDER BY r.route_id LIMIT 1", [concepts[0]], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    let tx = db.connection.unchecked_transaction().unwrap();
    for i in 1..=books as i64 {
        tx.execute("INSERT INTO book(content_hash,title,format) VALUES(?1,?2,?3)", params![format!("{i:064x}"), format!("Book {i}"), ["epub", "pdf", "m4b"][i as usize % 3]]).unwrap();
        tx.execute("INSERT INTO book_dir(dir_id,book_row_id,file_name,local_hash,is_downloaded) VALUES(?1,?2,?3,'',?4)", params![sync_common::ROOT_DIR_ID.to_string(), i, format!("{i}.epub"), i % 2]).unwrap();
        tx.execute("INSERT INTO book_unified_concept VALUES(?1,?2,1)", params![i, concepts[usize::from(i > 8)]]).unwrap();
        if i % 11 == 0 {
            tx.execute("INSERT OR IGNORE INTO book_unified_concept VALUES(?1,?2,1)", params![i, concepts[0]]).unwrap();
        }
        tx.execute("INSERT INTO book_language VALUES(?1,0,?2)", params![i, if i % 3 == 0 { "sv" } else { "en-US" }]).unwrap();
        if i % 7 == 0 {
            tx.execute("INSERT INTO book_language VALUES(?1,1,'en-GB')", [i]).unwrap();
        }
    }
    tx.commit().unwrap();
    db.connection.execute_batch("CREATE TABLE subject_book_ownership(route_id INTEGER NOT NULL,book_row_id INTEGER NOT NULL,assignment_count INTEGER NOT NULL,PRIMARY KEY(route_id,book_row_id)); CREATE INDEX idx_subject_book_ownership_book ON subject_book_ownership(book_row_id,route_id);").unwrap();
    refresh(&db);
    (db, route, book_model::normalize_search_text(&label))
}
fn refresh(db: &Database) {
    db.browse_subject_facet_counts("Subject", "", &[], &[], false).unwrap();
    // Independent reconstruction from assignments, not the new membership cache.
    db.connection.execute("DELETE FROM subject_book_ownership", []).unwrap();
    let prefix = OLD_FORMAT.split("WITH RECURSIVE ").nth(1).unwrap().split("), subject_book_ownership").next().unwrap();
    db.connection.execute_batch(&format!("INSERT INTO subject_book_ownership WITH RECURSIVE {prefix}) SELECT route_id,book_row_id,1 FROM assigned_routes UNION SELECT 0,book_row_id,1 FROM assigned_routes;")).unwrap();
}
fn read(db: &Database, query: &str, route: i64, search: &str, downloaded: bool) -> (Vec<(String, i64)>, i32) {
    let mut stmt = db.connection.prepare_cached(query).unwrap();
    stmt.reset_status(StatementStatus::FullscanStep);
    for (key, value) in [(":route_id", route), (":downloaded_only", i64::from(downloaded)), (":file_types", 0)] {
        if let Some(i) = stmt.parameter_index(key).unwrap() {
            stmt.raw_bind_parameter(i, value).unwrap();
        }
    }
    for (key, value) in [(":normalized_query", search), (":languages", "")] {
        if let Some(i) = stmt.parameter_index(key).unwrap() {
            stmt.raw_bind_parameter(i, value).unwrap();
        }
    }
    let mut rows = stmt.raw_query();
    let mut result = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        let key = match r.get_ref(0).unwrap() {
            library_database::rusqlite::types::ValueRef::Integer(i) => i.to_string(),
            library_database::rusqlite::types::ValueRef::Text(s) => String::from_utf8(s.to_vec()).unwrap(),
            _ => panic!("unexpected facet"),
        };
        result.push((key, r.get(1).unwrap()));
    }
    drop(rows);
    (result, stmt.get_status(StatementStatus::FullscanStep))
}
#[test]
fn bulk_membership_changes_preserve_subject_facets() {
    let (db, route, label) = fixture(240);
    let queries = [(facet_sql(0), old_sql(OLD_FORMAT)), (facet_sql(1), old_sql(OLD_LANGUAGE))];
    for mutation in [
        "SELECT 1",
        "UPDATE book SET hidden_at=1 WHERE row_id%5=0",
        "UPDATE book SET deleted_at=1 WHERE row_id%7=0",
        "UPDATE book_dir SET deleted_at=1 WHERE book_row_id%3=0",
        "DELETE FROM book_unified_concept WHERE book_row_id%4=0",
        "UPDATE book SET hidden_at=NULL,deleted_at=NULL; UPDATE book_dir SET deleted_at=NULL",
        "DELETE FROM book WHERE row_id%11=0",
    ] {
        db.connection.execute_batch(mutation).unwrap();
        refresh(&db);
        for (new, old) in &queries {
            for selected in [route, 0, -1] {
                for search in ["", "book 1", label.as_str(), "no such match qzx"] {
                    if selected == 0 && search == label {
                        continue;
                    }
                    for downloaded in [false, true] {
                        assert_eq!(read(&db, new, selected, search, downloaded).0, read(&db, old, selected, search, downloaded).0, "mutation={mutation}, route={selected}, search={search}, downloaded={downloaded}");
                    }
                }
            }
        }
    }
}
fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

// Replay the common cache-refresh SQL, optionally adding the exact retired
// ownership maintenance. Both variants use the same connection and transaction.
fn refresh_batch(db: &Database, count: i64, old: bool) -> Duration {
    let tx = db.connection.unchecked_transaction().unwrap();
    tx.execute("UPDATE main.book SET row_id=row_id WHERE 0", []).unwrap();
    let start = Instant::now();
    for book in 1..=count {
        tx.prepare_cached(sql("subject_cache_remove_book!\n")).unwrap().execute([book]).unwrap();
        if old {
            tx.prepare_cached("DELETE FROM subject_book_ownership WHERE book_row_id=?1").unwrap().execute([book]).unwrap();
        }
        let routes = tx.prepare_cached(sql("book_routes?\n")).unwrap().query_map([book], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        if routes.is_empty() {
            continue;
        }
        for (route, direct) in routes.into_iter().chain([(0, 0)]) {
            tx.prepare_cached(sql("subject_cache_insert_route!\n")).unwrap().execute([route]).unwrap();
            tx.prepare_cached(sql("subject_cache_insert_member!\n")).unwrap().execute(named_params! {":route_id":route,":book_row_id":book,":direct":direct}).unwrap();
        }
        if old {
            tx.prepare_cached("INSERT INTO subject_book_ownership WITH RECURSIVE routes(route_id,parent_route_id) AS (SELECT r.route_id,r.parent_route_id FROM book_unified_concept a JOIN curated.unified_concept_route r USING(concept_id) WHERE a.book_row_id=?1 UNION ALL SELECT r.route_id,r.parent_route_id FROM routes child JOIN curated.unified_concept_route r ON r.route_id=child.parent_route_id) SELECT route_id,?1,count(*) FROM routes GROUP BY route_id").unwrap().execute([book]).unwrap();
            tx.prepare_cached("INSERT INTO subject_book_ownership SELECT 0,book_row_id,count(*) FROM book_unified_concept WHERE book_row_id=?1 GROUP BY book_row_id ON CONFLICT(route_id,book_row_id) DO UPDATE SET assignment_count=excluded.assignment_count").unwrap().execute([book]).unwrap();
        }
    }
    tx.execute_batch(sql("subject_cache_prune &\n")).unwrap();
    tx.execute_batch(sql("subject_cache_clean&\n")).unwrap();
    tx.execute(sql("subject_cache_revision!\n"), [subject_projection::BUNDLED_UNIFIED_TAXONOMY_REVISION_ID]).unwrap();
    tx.commit().unwrap();
    start.elapsed()
}
#[test]
#[ignore = "release performance benchmark; run explicitly with --ignored --nocapture"]
fn subject_ownership_performance() {
    for size in [1000, 10000] {
        let (db, route, label) = fixture(size);
        println!("FIXTURE books={size} membership_rows={}", db.connection.query_row("SELECT count(*) FROM subject_browse_member", [], |r| r.get::<_, i64>(0)).unwrap());
        benchmark_facets(&db, size, route, &label);
        for count in [1, 100, size as i64] {
            let mut before = Vec::new();
            let mut after = Vec::new();
            for i in 0..7 {
                for old in [i % 2 == 0, i % 2 != 0] {
                    let elapsed = refresh_batch(&db, count, old);
                    if i > 0 {
                        if old { before.push(elapsed) } else { after.push(elapsed) }
                    }
                }
            }
            println!("REFRESH books={size} dirty={count} old_us={} new_us={}", median(before).as_micros(), median(after).as_micros());
        }
    }
}

fn combined_rows(db: &Database, route: i64, search: &str, formats: i32, languages: &str, downloaded: bool) -> Vec<(i64, String, i64)> {
    db.connection.prepare_cached(sql("get_subject_facets?\n"))
        .unwrap()
        .query_map(named_params! {":route_id":route,":normalized_query":search,":file_types":formats,":languages":languages,":downloaded_only":downloaded}, |r| {
            let value = if r.get::<_, i64>(0)? == 0 { r.get::<_, i64>(1)?.to_string() } else { r.get::<_, String>(1)? };
            Ok((r.get(0)?, value, r.get(2)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
fn split_rows(db: &Database, route: i64, search: &str, formats: i32, languages: &str, downloaded: bool) -> Vec<(i64, String, i64)> {
    let source = include_str!("fixtures/subject_facets_split.sql");
    let mut result = Vec::new();
    for (kind, name) in [(0, "get_subject_format_facets"), (1, "get_subject_language_facets")] {
        let marker = format!("-- name: {name}?\n");
        let query = source.split(&marker).nth(1).unwrap().split("-- name:").next().unwrap();
        let mut stmt = db.connection.prepare_cached(query).unwrap();
        for (key, value) in [(":route_id", route), (":downloaded_only", i64::from(downloaded)), (":file_types", i64::from(formats))] {
            if let Some(i) = stmt.parameter_index(key).unwrap() {
                stmt.raw_bind_parameter(i, value).unwrap();
            }
        }
        for (key, value) in [(":normalized_query", search), (":languages", languages)] {
            if let Some(i) = stmt.parameter_index(key).unwrap() {
                stmt.raw_bind_parameter(i, value).unwrap();
            }
        }
        let mut rows = stmt.raw_query();
        while let Some(r) = rows.next().unwrap().map(|r| (if kind == 0 { r.get::<_, i64>(0).unwrap().to_string() } else { r.get::<_, String>(0).unwrap() }, r.get::<_, i64>(1).unwrap())) {
            result.push((kind, r.0, r.1));
        }
    }
    result
}
#[test]
fn combined_facets_share_search_and_preserve_cross_filters() {
    let (db, route, label) = fixture(240);
    let parent: i64 = db.connection.query_row("SELECT parent_route_id FROM curated.unified_concept_route WHERE route_id=?1", [route], |r| r.get(0)).unwrap();
    for selected in [route, parent, 0, -1] {
        for search in ["", "book 1", label.as_str(), "qzx-no-match"] {
            if selected == 0 && !search.is_empty() {
                continue;
            }
            for formats in [0, 1, 2, 4, 3] {
                for languages in ["", ",en,", ",sv,", ",en,sv,", ",xx,"] {
                    for downloaded in [false, true] {
                        assert_eq!(
                            combined_rows(&db, selected, search, formats, languages, downloaded),
                            split_rows(&db, selected, search, formats, languages, downloaded),
                            "route={selected} search={search} formats={formats} languages={languages} downloaded={downloaded}"
                        );
                    }
                }
            }
        }
    }
    let mut stmt = db.connection.prepare(&format!("EXPLAIN QUERY PLAN {}", sql("get_subject_facets?\n"))).unwrap();
    let plan = stmt.query_map(named_params! {":route_id":parent,":normalized_query":label,":file_types":0,":languages":"",":downloaded_only":false}, |r| r.get::<_, String>(3)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(plan.iter().filter(|line| line.as_str() == "MATERIALIZE search_matches").count(), 1, "{plan:?}");
}
fn benchmark_facets(db: &Database, size: usize, route: i64, label: &str) {
    let parent: i64 = db.connection.query_row("SELECT parent_route_id FROM curated.unified_concept_route WHERE route_id=?1", [route], |r| r.get(0)).unwrap();
    for (case, selected, search, formats, languages, downloaded) in [
        ("root", 0, "", 0, "", false),
        ("branch", route, "", 0, "", false),
        ("title", route, "book 1", 0, "", false),
        ("subject_label", parent, label, 0, "", false),
        ("miss", route, "qzx-no-match", 0, "", false),
        ("cross_filters", route, "", 2, ",en,", true),
        ("filtered_search", route, "book 1", 2, ",en,", true),
    ] {
        assert_eq!(combined_rows(db, selected, search, formats, languages, downloaded), split_rows(db, selected, search, formats, languages, downloaded));
        let mut before = Vec::new();
        let mut after = Vec::new();
        for i in 0..11 {
            for old in [i % 2 == 0, i % 2 != 0] {
                let t = Instant::now();
                let rows = if old { split_rows(db, selected, search, formats, languages, downloaded) } else { combined_rows(db, selected, search, formats, languages, downloaded) };
                std::hint::black_box(rows);
                let elapsed = t.elapsed();
                if i > 0 {
                    if old { before.push(elapsed) } else { after.push(elapsed) }
                }
            }
        }
        println!("FACETS books={size} case={case} split_us={} combined_us={}", median(before).as_micros(), median(after).as_micros());
    }
}

#[test]
#[ignore = "release comparison of split and combined facet queries"]
fn subject_facet_combination_performance() {
    for size in [1000, 10000] {
        let (db, route, label) = fixture(size);
        benchmark_facets(&db, size, route, &label);
    }
}
