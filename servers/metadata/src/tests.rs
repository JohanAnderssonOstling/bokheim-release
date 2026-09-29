use super::*;
use crate::http::load_operational_stats;
use crate::identity::edition_candidate_query;
use crate::import::core::{completed_phase, configure_import, create_schema, import_editions, register_import_inputs};
use crate::query::install_query_deadline;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Write;

fn dump(path: &Path, records: &[serde_json::Value]) {
    let file = File::create(path).unwrap();
    let mut gzip = GzEncoder::new(file, Compression::fast());
    for record in records {
        writeln!(gzip, "/type\t/key\t1\t2026-08-01T00:00:00.000000\t{record}").unwrap();
    }
    gzip.finish().unwrap();
}

pub(super) fn snapshot_from(directory: &Path, edition_records: &[serde_json::Value], work_records: &[serde_json::Value], author_records: &[serde_json::Value]) -> PathBuf {
    let editions = directory.join("ol_dump_editions_2026-08-01.txt.gz");
    let works = directory.join("ol_dump_works_2026-08-01.txt.gz");
    let authors = directory.join("ol_dump_authors_2026-08-01.txt.gz");
    let database = directory.join("snapshot.sqlite");
    dump(&editions, edition_records);
    dump(&works, work_records);
    dump(&authors, author_records);
    import_snapshot(&editions, &works, &authors, &database, None).unwrap();
    database
}

fn edition_query(title: &str, publishers: Vec<String>, book_year: Option<i32>) -> EditionIdentityRequest {
    EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "query".to_owned(), title: title.to_owned(), authors: vec!["Alex Smith".to_owned()], publishers, book_year }] }
}

#[test]
fn edition_identity_candidate_query_uses_title_and_work_indexes() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
            "title": "The Example Book", "isbn_13": ["9780306406157"]
        })],
        &[serde_json::json!({"key": "/works/OL10W", "title": "Canonical Example Work"})],
        &[],
    );
    let connection = Connection::open(database).unwrap();
    // A one-row fixture makes SQLite rationally scan the outer detail
    // table. Give the planner production-like cardinality so this test
    // distinguishes that harmless fixture choice from the old candidate
    // full scan.
    connection
        .execute_batch(
            "WITH RECURSIVE ids(value) AS (SELECT 2 UNION ALL SELECT value+1 FROM ids WHERE value<10000)
                 INSERT INTO edition(edition_id,work_id) SELECT value,NULL FROM ids;
                 ANALYZE;",
        )
        .unwrap();
    let query = edition_candidate_query(1, "book_year");
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {query}")).unwrap();
    let plan = statement
        .query_map(params![0, "the example book", 16, i64::try_from(MAX_RAW_TITLE_CANDIDATES).unwrap(), i64::try_from(MAX_RAW_TITLE_CANDIDATES).unwrap(), i64::try_from(MAX_REVERSE_TITLE_CANDIDATES + 1).unwrap()], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");

    assert!(plan.contains("edition_bibliography_by_title"), "edition title index missing from plan:\n{plan}");
    assert!(plan.contains("work_bibliography_by_title"), "work title index missing from plan:\n{plan}");
    assert!(plan.contains("edition_by_work"), "work-to-edition index missing from plan:\n{plan}");
    assert!(plan.contains("SEARCH bibliography USING COVERING INDEX edition_bibliography_by_title"), "edition candidate lookup does not seek by title:\n{plan}");
    assert!(plan.contains("SEARCH work_bibliography USING COVERING INDEX work_bibliography_by_title"), "work candidate lookup does not seek by title:\n{plan}");
    assert!(plan.contains("SEARCH edition USING COVERING INDEX edition_by_work"), "work-to-edition lookup does not seek by work:\n{plan}");
    assert!(!plan.lines().any(|line| line.trim() == "SCAN edition"), "candidate lookup full-scans edition:\n{plan}");
}

#[test]
fn split_snapshot_routes_subject_rich_and_identity_workloads() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
            "title": "The Example Book", "publishers": ["Example Press"],
            "authors": [{"key": "/authors/OL20A"}],
            "isbn_13": ["9780306406157"]
        })],
        &[serde_json::json!({
            "key": "/works/OL10W", "title": "Canonical Example Work",
            "authors": [{"author": {"key": "/authors/OL20A"}}]
        })],
        &[serde_json::json!({"key": "/authors/OL20A", "name": "Alex Smith"})],
    );
    let subjects = temporary.path().join("subjects.sqlite");
    let rich = temporary.path().join("rich.sqlite");
    let identities = temporary.path().join("identities.sqlite");
    split_snapshot(&database, &subjects, &rich, &identities).unwrap();

    let service = MetadataService::open_with_stores(&subjects, &rich, &identities, None::<&Path>).unwrap();
    let classified = service.lookup(ClassificationRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    assert_eq!(classified.results[0].status, LookupStatus::Matched);
    assert!(classified.results[0].matches[0].classifications.is_empty());

    let enriched = service.enrich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    assert_eq!(enriched.results[0].matches[0].authors[0].name.as_deref(), Some("Alex Smith"));

    assert!(Connection::open(subjects).unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='author')", (), |row| row.get::<_, bool>(0)).unwrap() == false);
    assert!(Connection::open(rich).unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='edition_bibliography')", (), |row| row.get::<_, bool>(0)).unwrap() == false);
    assert!(Connection::open(identities).unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='edition_bibliography')", (), |row| row.get::<_, bool>(0)).unwrap());
}

#[test]
fn memory_mapped_subject_index_matches_sqlite_lookup() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
                "isbn_13": ["9780306406157"], "lc_classifications": ["QA76.73.R3"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}],
                "isbn_13": ["9780131103627"], "lc_classifications": ["QA76.73.R3"]
            }),
            serde_json::json!({
                "key": "/books/OL3M", "works": [{"key": "/works/OL11W"}],
                "isbn_13": ["9780306406157"], "dewey_decimal_class": ["940.3"]
            }),
        ],
        &[serde_json::json!({"key": "/works/OL10W", "dewey_number": ["005.133"]}), serde_json::json!({"key": "/works/OL11W", "lc_classifications": ["D521"]})],
        &[],
    );
    let index = temporary.path().join("subjects.idx");
    build_subject_index(&database, &index).unwrap();
    let request = ClassificationRequest { isbns: vec!["0-306-40615-2".to_owned(), "9780131103627".to_owned(), "9780000000002".to_owned(), "invalid".to_owned()] };
    let sqlite = MetadataService::open(&database).unwrap().lookup(request.clone()).unwrap();
    let indexed = MetadataService::open(&database).unwrap().with_subject_index(&index).unwrap().lookup(request).unwrap();
    assert_eq!(serde_json::to_value(indexed).unwrap(), serde_json::to_value(sqlite).unwrap());
    assert!(index.metadata().unwrap().len() < database.metadata().unwrap().len());
    assert!(MetadataService::open(&database).unwrap().with_subject_index(&index).unwrap().state.pool.is_none());
}

#[test]
fn subject_index_rejects_corrupt_files() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(temporary.path(), &[], &[], &[]);
    let index = temporary.path().join("bad.idx");
    std::fs::write(&index, b"not an index").unwrap();
    let failure = MetadataService::open(database).unwrap().with_subject_index(index).err().expect("corrupt index must be rejected");
    assert!(failure.to_string().contains("invalid subject index header"));
}

#[test]
fn read_pool_bounds_connections_and_sqlite_memory_budgets() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(temporary.path(), &[], &[], &[]);
    let service = MetadataService::open(database).unwrap();
    let connections = (0..QUERY_CONCURRENCY).map(|_| service.connection().unwrap()).collect::<Vec<_>>();

    assert!(service.state.pool.as_ref().unwrap().try_get().is_none(), "pool exceeded its configured connection ceiling");
    assert_eq!(service.state.query_slots.available_permits(), QUERY_CONCURRENCY as usize);
    assert_eq!(connections[0].query_row("PRAGMA cache_size", (), |row| row.get::<_, i64>(0)).unwrap(), -(READ_CACHE_KIB as i64));
    assert_eq!(connections[0].query_row("PRAGMA mmap_size", (), |row| row.get::<_, i64>(0)).unwrap(), READ_MMAP_BYTES as i64);
}

#[test]
fn description_capability_follows_the_snapshot_table() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(temporary.path(), &[], &[], &[]);
    assert!(MetadataService::open(&database).unwrap().state.descriptions_available);

    let connection = Connection::open(&database).unwrap();
    connection.execute("DROP TABLE work_description", ()).unwrap();
    drop(connection);

    assert!(!MetadataService::open(&database).unwrap().state.descriptions_available);
}

#[test]
fn description_sidecar_enriches_a_core_without_embedded_descriptions() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
            "isbn_13": ["9780306406157"], "dewey_decimal_class": ["500"]
        })],
        &[serde_json::json!({
            "key": "/works/OL10W",
            "description": {"value": "Sidecar description"},
            "subjects": ["History", "Europe", "Great Britain", "Unrelated noisy heading"]
        })],
        &[],
    );
    let connection = Connection::open(&database).unwrap();
    connection.execute("DROP TABLE work_description", ()).unwrap();
    drop(connection);

    let sidecar = temporary.path().join("descriptions.sqlite");
    import_description_snapshot(&temporary.path().join("ol_dump_works_2026-08-01.txt.gz"), &sidecar, "2026-08-01", None).unwrap();
    let service = MetadataService::open_with_descriptions(&database, Some(&sidecar)).unwrap();
    let response = service.classify_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();

    assert!(service.state.descriptions_available);
    assert_eq!(response.results[0].matches[0].description.as_deref(), Some("Sidecar description"));
    assert!(response.results[0].matches[0].classifications.iter().any(|classification| classification.scheme == ClassificationScheme::Bisac && classification.source == ClassificationSource::Work));
    let sidecar_connection = Connection::open(sidecar).unwrap();
    assert_eq!(sidecar_connection.query_row("SELECT description_records FROM description_snapshot", (), |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(sidecar_connection.query_row("SELECT subject_heading_records FROM description_snapshot", (), |row| row.get::<_, i64>(0)).unwrap(), 4);
    assert_eq!(sidecar_connection.query_row("SELECT matched_subject_heading_records FROM description_snapshot", (), |row| row.get::<_, i64>(0)).unwrap(), 3);
    assert!(sidecar_connection.query_row("SELECT bisac_assignment_records FROM description_snapshot", (), |row| row.get::<_, i64>(0)).unwrap() >= 1);
    assert_eq!(sidecar_connection.query_row("SELECT COUNT(*) FROM work_bisac_subject WHERE source_subject='Unrelated noisy heading'", (), |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(sidecar_connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='edition')", (), |row| row.get::<_, bool>(0)).unwrap(), false);
}

#[test]
fn snapshot_enrichment_links_explicit_authorship() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
            "authors": [{"key": "/authors/OL20A"}], "isbn_13": ["9780306406157"]
        })],
        &[serde_json::json!({
            "key": "/works/OL10W", "authors": [{"author": {"key": "/authors/OL20A"}}]
        })],
        &[serde_json::json!({
            "key": "/authors/OL20A", "name": "Alex Smith"
        })],
    );

    let service = MetadataService::open(&database).unwrap();
    let response = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    let author = &response.results[0].matches[0].authors[0];
    assert_eq!(author.open_library_author_id, "OL20A");
    assert_eq!(author.name.as_deref(), Some("Alex Smith"));

    let core_connection = Connection::open(database).unwrap();
    assert!(!core_connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('author') WHERE name='biography')", (), |row| row.get::<_, bool>(0)).unwrap());
    assert_eq!(core_connection.query_row("SELECT COUNT(*) FROM work_author WHERE work_id=10 AND author_id=20", (), |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(core_connection.query_row("SELECT COUNT(*) FROM edition_author WHERE edition_id=1 AND author_id=20", (), |row| row.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn description_sidecar_must_match_the_core_dump_date() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(temporary.path(), &[], &[], &[]);
    let works = temporary.path().join("sidecar-works.txt.gz");
    dump(&works, &[]);
    let sidecar = temporary.path().join("descriptions.sqlite");
    import_description_snapshot(&works, &sidecar, "2026-07-31", None).unwrap();

    let failure = MetadataService::open_with_descriptions(&database, Some(&sidecar)).err().expect("mismatched sidecar must be rejected");
    assert!(failure.to_string().contains("does not match metadata snapshot"));
}

#[test]
fn database_query_deadline_interrupts_expensive_sql() {
    let connection = Connection::open_in_memory().unwrap();
    install_query_deadline(&connection, Duration::ZERO);
    let failure = connection.query_row("WITH RECURSIVE numbers(value) AS (VALUES(1) UNION ALL SELECT value+1 FROM numbers WHERE value<10000000) SELECT SUM(value) FROM numbers", (), |row| row.get::<_, i64>(0)).unwrap_err();
    assert!(matches!(failure, rusqlite::Error::SqliteFailure(error, _) if error.code == rusqlite::ErrorCode::OperationInterrupted));
}

#[test]
fn snapshot_import_uses_current_schema() {
    let temporary = tempfile::tempdir().unwrap();
    let editions = temporary.path().join("ol_dump_editions_2026-08-01.txt.gz");
    let works = temporary.path().join("ol_dump_works_2026-08-01.txt.gz");
    let authors = temporary.path().join("ol_dump_authors_2026-08-01.txt.gz");
    let database = temporary.path().join("snapshot.sqlite");
    dump(
        &editions,
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
            "title": "Example", "isbn_13": ["9780306406157"]
        })],
    );
    dump(&works, &[serde_json::json!({"key": "/works/OL10W", "title": "Example", "description": {"type": "/type/text", "value": "Open Library summary."}})]);
    dump(&authors, &[]);

    import_snapshot(&editions, &works, &authors, &database, None).unwrap();
    let validation = Connection::open(&database).unwrap();
    assert_eq!(validation.query_row("SELECT schema_version FROM snapshot WHERE singleton=1", (), |row| row.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    assert_eq!(
        validation.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE '%_stage'", (), |row| row.get::<_, i64>(0)).unwrap(),
        0,
        "all append-only import staging tables must be removed from the immutable snapshot"
    );
    drop(validation);

    let service = MetadataService::open(&database).unwrap();
    let response = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    assert_eq!(response.results[0].matches[0].description.as_deref(), Some("Open Library summary."));
    let classified = service.classify_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    assert_eq!(classified.results[0].matches[0].description.as_deref(), Some("Open Library summary."));
}

#[test]
fn snapshot_enrichment_resolves_work_authors_and_external_ids() {
    let temporary = tempfile::tempdir().unwrap();
    let editions = temporary.path().join("ol_dump_editions_2026-08-01.txt.gz");
    let works = temporary.path().join("ol_dump_works_2026-08-01.txt.gz");
    let authors = temporary.path().join("ol_dump_authors_2026-08-01.txt.gz");
    let database = temporary.path().join("snapshot.sqlite");
    dump(
        &editions,
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "publishers": ["North Wind Press"], "publish_date": "2004",
                "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}],
                "dewey_decimal_class": ["999"], "lc_classifications": ["QA76.73.R87"]
            }),
            serde_json::json!({
                "key": "/books/OL3M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "publishers": ["North Wind Press"], "publish_date": "2005", "isbn_13": ["9780821338278"]
            }),
        ],
    );
    dump(
        &works,
        &[serde_json::json!({
            "key": "/works/OL10W", "title": "Canonical Example Work", "authors": [{"author": {"key": "/authors/OL100A"}}], "lc_classifications": ["QA76.6"]
        })],
    );
    dump(
        &authors,
        &[serde_json::json!({
            "key": "/authors/OL100A", "name": "Alex Smith", "remote_ids": {"viaf": "12345", "wikidata": "Q42"}
        })],
    );
    import_snapshot(&editions, &works, &authors, &database, None).unwrap();
    let validation = Connection::open(&database).unwrap();
    assert_eq!(validation.query_row("SELECT schema_version FROM snapshot WHERE singleton=1", (), |row| row.get::<_, i64>(0)).unwrap(), SCHEMA_VERSION);
    assert_eq!(
        validation.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE '%_stage'", (), |row| row.get::<_, i64>(0)).unwrap(),
        0,
        "all append-only import staging tables must be removed from the immutable snapshot"
    );
    assert_eq!(
        validation.query_row("SELECT COUNT(*) FROM import_checkpoint WHERE phase IN ('editions_materialize','works_materialize','work_titles_materialize','authors_materialize') AND completed=1", (), |row| row.get::<_, i64>(0)).unwrap(),
        4
    );
    let service = MetadataService::open(&database).unwrap();
    let operations = load_operational_stats(&service.state).unwrap();
    assert_eq!(operations.status, "available");
    assert_eq!(operations.schema_version, SCHEMA_VERSION);
    assert!(operations.database_bytes > 0);
    assert!(operations.edition_records > 0);
    assert!(operations.work_records > 0);
    assert!(operations.author_records > 0);
    assert!(!operations.metrics.is_empty());
    let response = service.enrich(MetadataEnrichmentRequest { isbns: vec!["978-0-306-40615-7".to_owned()] }).unwrap();
    assert_eq!(response.snapshot.dump_date, "2026-08-01");
    assert_eq!(response.results[0].status, LookupStatus::Matched);
    let matched = &response.results[0].matches[0];
    assert_eq!(matched.open_library_work_id.as_deref(), Some("OL10W"));
    assert_eq!(matched.authors.len(), 1);
    assert_eq!(matched.authors[0].open_library_author_id, "OL100A");
    assert_eq!(matched.authors[0].name.as_deref(), Some("Alex Smith"));
    assert_eq!(matched.authors[0].source, AuthorSource::ExactEdition);
    assert_eq!(matched.authors[0].position, 0);
    assert_eq!(matched.authors[0].identifiers, vec![AuthorIdentifier { authority: "viaf".to_owned(), value: "12345".to_owned() }, AuthorIdentifier { authority: "wikidata".to_owned(), value: "Q42".to_owned() }]);
    assert!(matched.classifications.iter().all(|classification| classification.scheme != ClassificationScheme::DeweyDecimal));
    assert!(matched.classifications.iter().any(|classification| classification.notation == "QA76.6" && classification.source == ClassificationSource::Work));
    assert!(!matched.classifications.iter().any(|classification| classification.notation == "QA76.73.R87" || classification.notation == "999"));

    let rich = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    let matched = &rich.results[0].matches[0];
    assert_eq!(matched.authors.len(), 1);
    let identity = service
        .resolve_editions(EditionIdentityRequest {
            queries: vec![
                EditionIdentityQuery { query_id: "local-1".to_owned(), title: "The Example Book".to_owned(), authors: vec!["Smith, Alex".to_owned()], publishers: vec!["North-Wind Press".to_owned()], book_year: Some(2004) },
                EditionIdentityQuery { query_id: "work-title".to_owned(), title: "Canonical Example Work".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: vec!["North Wind Press".to_owned()], book_year: Some(2004) },
                EditionIdentityQuery { query_id: "ambiguous".to_owned(), title: "The Example Book".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: vec!["North Wind Press".to_owned()], book_year: None },
                EditionIdentityQuery { query_id: "insufficient".to_owned(), title: "The Example Book".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: Vec::new(), book_year: None },
            ],
        })
        .unwrap();
    assert_eq!(identity.results[0].status, EditionIdentityStatus::Matched);
    let identity_match = identity.results[0].matched.as_ref().unwrap();
    assert_eq!(identity_match.canonical_isbn13, "9780306406157");
    assert_eq!(identity_match.open_library_edition_id, "OL1M");
    assert_eq!(identity_match.title_match, EditionTitleMatch::Edition);
    assert!(identity_match.matched_publisher && identity_match.matched_book_year);
    assert_eq!(identity.results[1].status, EditionIdentityStatus::Matched);
    assert_eq!(identity.results[1].matched.as_ref().unwrap().title_match, EditionTitleMatch::Work);
    for result in &identity.results[2..=3] {
        assert_eq!(result.status, EditionIdentityStatus::Matched);
        assert_eq!(result.matched.as_ref().and_then(|matched| matched.open_library_work_id.as_deref()), Some("OL10W"));
    }
}

#[test]
fn open_library_scan_resumes_from_committed_record_checkpoint() {
    let temporary = tempfile::tempdir().unwrap();
    let editions = temporary.path().join("editions.txt.gz");
    dump(&editions, &[serde_json::json!({"key":"/books/OL1M","isbn_13":["9780306406157"]}), serde_json::json!({"key":"/books/OL2M","isbn_13":["9780821338278"]})]);
    let database = temporary.path().join("resumable-open-library.sqlite");
    let mut connection = Connection::open(&database).unwrap();
    configure_import(&connection).unwrap();
    create_schema(&connection).unwrap();
    connection
        .execute_batch(
            "INSERT INTO edition_stage(edition_id) VALUES(1);
                 INSERT INTO edition_isbn_stage(isbn13,edition_id) VALUES(9780306406157,1);
                 INSERT INTO import_checkpoint(phase,records_read,completed) VALUES('editions',1,0);",
        )
        .unwrap();

    assert_eq!(import_editions(&mut connection, &editions, None).unwrap(), 2);
    assert_eq!(completed_phase(&connection, "editions").unwrap(), Some(2));
    assert_eq!(completed_phase(&connection, "editions_materialize").unwrap(), Some(0));
    let retained = connection.query_row("SELECT group_concat(edition_id,',') FROM (SELECT edition_id FROM edition ORDER BY edition_id)", (), |row| row.get::<_, String>(0)).unwrap();
    assert_eq!(retained, "1,2");
    assert_eq!(import_editions(&mut connection, &editions, None).unwrap(), 2, "a completed materialized phase is idempotent");
}

/// An edition with no `/works` link leaves `work_bibliography` unmatched by
/// the candidate LEFT JOIN, so its title-comparison column is NULL rather
/// than false. Reading it as a plain `bool` fails the entire batch request.
#[test]
fn edition_identity_resolves_an_edition_that_has_no_linked_work() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "authors": [{"key": "/authors/OL100A"}],
            "title": "Standalone Edition", "publishers": ["North Wind Press"], "publish_date": "2004",
            "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
        })],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("Standalone Edition", vec!["North Wind Press".to_owned()], Some(2004))).expect("an edition without a work must not fail the request");
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    let matched = response.results[0].matched.as_ref().unwrap();
    assert_eq!(matched.open_library_edition_id, "OL1M");
    assert_eq!(matched.open_library_work_id, None);
    assert_eq!(matched.title_match, EditionTitleMatch::Edition);
}

#[test]
fn edition_identity_excludes_ddc_only_and_invalid_lcc_candidates_before_ranking() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({"key":"/books/OL1M", "title":"Example LCC Book", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780306406157"], "lc_classifications":["E757 .A37 1985"]}),
            serde_json::json!({"key":"/books/OL2M", "title":"Example LCC Book", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780821338278"], "dewey_decimal_class":["B"]}),
            serde_json::json!({"key":"/books/OL3M", "title":"Example LCC Book", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9783161484100"], "lc_classifications":["invalid"], "dewey_decimal_class":["973"]}),
        ],
        &[],
        &[serde_json::json!({"key":"/authors/OL100A", "name":"Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();
    let response = service.resolve_editions(edition_query("Example LCC Book", Vec::new(), None)).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL1M");
    assert!(response.results[0].candidates.is_empty());
}

#[test]
fn edition_identity_rejects_an_edition_without_lcc_or_ddc() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "title": "Unclassified Edition", "isbn_13": ["9780306406157"]
        })],
        &[],
        &[],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("Unclassified Edition", Vec::new(), None)).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::NoMatch);
    assert!(response.results[0].matched.is_none());
}

/// An untitled sibling edition of the same work has no `edition_bibliography`
/// row, so its title-comparison column is NULL. One such row in the
/// candidate set must not destroy the lookup for the edition that does match.
#[test]
fn edition_identity_survives_an_untitled_sibling_edition_of_the_same_work() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "publishers": ["North Wind Press"], "publish_date": "2004",
                "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[serde_json::json!({"key": "/works/OL10W", "title": "Canonical Example Work", "authors": [{"author": {"key": "/authors/OL100A"}}]})],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("Canonical Example Work", vec!["North Wind Press".to_owned()], Some(2004))).expect("an untitled candidate must not fail the request");
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    let matched = response.results[0].matched.as_ref().unwrap();
    assert_eq!(matched.open_library_edition_id, "OL1M");
    assert_eq!(matched.title_match, EditionTitleMatch::Work);
}

/// Matches are emitted per ISBN but the match/ambiguity decision counts
/// them, so a single edition that carries more than one ISBN is reported
/// ambiguous and callers that require `Matched` silently give up on it.
#[test]
fn edition_identity_matches_a_single_edition_that_carries_several_isbns() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
            "title": "The Example Book", "publishers": ["North Wind Press"], "publish_date": "2004",
            "isbn_13": ["9780306406157"], "isbn_10": ["0821338277"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
        })],
        &[serde_json::json!({"key": "/works/OL10W", "title": "Canonical Example Work", "authors": [{"author": {"key": "/authors/OL100A"}}]})],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("The Example Book", vec!["North Wind Press".to_owned()], Some(2004))).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched, "two ISBNs on one edition are still one edition");
    let matched = response.results[0].matched.as_ref().unwrap();
    assert_eq!(matched.open_library_edition_id, "OL1M");
    assert!(matched.matched_publisher && matched.matched_book_year);
}

#[test]
fn edition_identity_accepts_a_local_subtitle_and_newer_book_details() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
            "title": "The Example Book", "publishers": ["Old Press"], "publish_date": "2004", "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
        })],
        &[serde_json::json!({"key": "/works/OL10W", "title": "The Example Book", "authors": [{"author": {"key": "/authors/OL100A"}}]})],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("Cambridge concise histories: The Example Book (Routledge Classics)", vec!["New Press".to_owned()], Some(2026))).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    let matched = response.results[0].matched.as_ref().unwrap();
    assert_eq!(matched.open_library_edition_id, "OL1M");
    assert!(!matched.matched_publisher);
    assert!(!matched.matched_book_year);
}

#[test]
fn edition_identity_prefers_the_longest_title_match_before_the_review_limit() {
    let temporary = tempfile::tempdir().unwrap();
    let mut editions = (1..=300)
        .map(|edition_id| {
            serde_json::json!({
                "key": format!("/books/OL{edition_id}M"),
                "authors": [{"key": "/authors/OL100A"}],
                "title": "Physical Chemistry",
                "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            })
        })
        .collect::<Vec<_>>();
    editions.push(serde_json::json!({
        "key": "/books/OL1000M",
        "authors": [{"key": "/authors/OL100A"}],
        "title": "Physical Chemistry: A Very Short Introduction",
        "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
    }));
    let database = snapshot_from(temporary.path(), &editions, &[], &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})]);
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("Physical Chemistry: A Very Short Introduction", Vec::new(), None)).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    assert!(!response.results[0].candidates_truncated);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL1000M");
}

#[test]
fn edition_identity_uses_author_only_to_break_a_longest_substring_tie() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "authors": [{"key": "/authors/OL100A"}],
                "title": "A Concise History of Spain", "publishers": ["First Press"], "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "authors": [{"key": "/authors/OL200A"}],
                "title": "A Concise History of Spain", "publishers": ["Preferred Press"], "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"}), serde_json::json!({"key": "/authors/OL200A", "name": "Jamie Jones"})],
    );
    let service = MetadataService::open(&database).unwrap();
    let title = "Cambridge concise histories: A Concise History of Spain (New edition)".to_owned();
    let response = service
        .resolve_editions(EditionIdentityRequest {
            queries: vec![
                EditionIdentityQuery { query_id: "title-only".to_owned(), title: title.clone(), authors: Vec::new(), publishers: vec!["Preferred Press".to_owned()], book_year: None },
                EditionIdentityQuery { query_id: "with-author".to_owned(), title, authors: vec!["Alex Smith".to_owned()], publishers: vec!["Preferred Press".to_owned()], book_year: None },
            ],
        })
        .unwrap();

    assert_eq!(response.results[0].status, EditionIdentityStatus::Ambiguous);
    assert_eq!(response.results[0].candidates.len(), 2);
    assert_eq!(response.results[1].status, EditionIdentityStatus::Matched);
    assert_eq!(response.results[1].matched.as_ref().unwrap().open_library_edition_id, "OL1M");
}

#[test]
fn edition_identity_requires_an_author_match_for_automatic_acceptance() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({
            "key": "/books/OL1M", "authors": [{"key": "/authors/OL200A"}],
            "title": "Complete Works Of", "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
        })],
        &[],
        &[serde_json::json!({"key": "/authors/OL200A", "name": "Jamie Jones"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service
        .resolve_editions(EditionIdentityRequest {
            queries: vec![EditionIdentityQuery { query_id: "wrong-author".to_owned(), title: "Delphi Complete Works Of Athenaeus".to_owned(), authors: vec!["Athenaeus".to_owned()], publishers: Vec::new(), book_year: None }],
        })
        .unwrap();

    assert_eq!(response.results[0].status, EditionIdentityStatus::NoMatch);
    assert!(response.results[0].matched.is_none());
    assert!(response.results[0].candidates.is_empty());
}

#[test]
fn edition_identity_accepts_one_work_after_author_matched_editions_are_collapsed() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "Complete Works of Homer", "publishers": ["First Press"], "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "Complete Works of Homer", "publishers": ["Second Press"], "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[serde_json::json!({"key": "/works/OL10W", "title": "Complete Works of Homer", "authors": [{"author": {"key": "/authors/OL100A"}}]})],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Homer"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service
        .resolve_editions(EditionIdentityRequest {
            queries: vec![EditionIdentityQuery { query_id: "same-work".to_owned(), title: "Delphi Complete Works of Homer".to_owned(), authors: vec!["Homer".to_owned()], publishers: vec!["Delphi Classics".to_owned()], book_year: None }],
        })
        .unwrap();

    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_work_id.as_deref(), Some("OL10W"));
    assert!(!response.results[0].matched.as_ref().unwrap().matched_publisher);
}

#[test]
fn edition_identity_combines_inferred_author_and_year_to_remove_other_candidates() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "authors": [{"key": "/authors/OL100A"}],
                "title": "Algorithms", "publish_date": "1999", "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "authors": [{"key": "/authors/OL100A"}],
                "title": "Algorithms", "publish_date": "2019", "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL3M", "authors": [{"key": "/authors/OL200A"}],
                "title": "Algorithms", "publish_date": "2019", "isbn_13": ["9781501168697"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Jeff Erickson"}), serde_json::json!({"key": "/authors/OL200A", "name": "Fethi Rabhi"})],
    );
    let service = MetadataService::open(&database).unwrap();
    let request =
        EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "noisy-filename".to_owned(), title: "Algorithms".to_owned(), authors: vec!["Jeff Erickson".to_owned()], publishers: Vec::new(), book_year: Some(2019) }] };

    let response = service.resolve_editions(request).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL2M");
}

#[test]
fn edition_identity_batch_keeps_results_attached_to_each_query() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "authors": [{"key": "/authors/OL100A"}],
                "title": "First Book", "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "authors": [{"key": "/authors/OL100A"}],
                "title": "Second Book", "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();
    let request = EditionIdentityRequest {
        queries: vec![
            EditionIdentityQuery { query_id: "second".to_owned(), title: "Second Book".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: Vec::new(), book_year: None },
            EditionIdentityQuery { query_id: "missing".to_owned(), title: "Missing Book".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: Vec::new(), book_year: None },
            EditionIdentityQuery { query_id: "first".to_owned(), title: "First Book".to_owned(), authors: vec!["Alex Smith".to_owned()], publishers: Vec::new(), book_year: None },
        ],
    };

    let response = service.resolve_editions(request).unwrap();
    assert_eq!(response.results.iter().map(|result| result.query_id.as_str()).collect::<Vec<_>>(), vec!["second", "missing", "first"]);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL2M");
    assert_eq!(response.results[1].status, EditionIdentityStatus::NoMatch);
    assert_eq!(response.results[2].matched.as_ref().unwrap().open_library_edition_id, "OL1M");
}

#[test]
fn edition_identity_uses_publisher_only_to_break_a_title_author_tie() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "publishers": ["Old Press"], "publish_date": "2004", "isbn_13": ["9780306406157"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}], "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "publishers": ["Preferred Press"], "publish_date": "2010", "isbn_13": ["9780821338278"], "dewey_decimal_class": ["005.1"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[serde_json::json!({"key": "/works/OL10W", "title": "The Example Book", "authors": [{"author": {"key": "/authors/OL100A"}}]})],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("The Example Book", vec!["Preferred Press".to_owned()], None)).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL2M");
}

#[test]
fn edition_identity_keeps_distinct_isbns_unresolved_despite_identical_codes() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "isbn_13": ["9780306406157"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "isbn_13": ["9780821338278"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("The Example Book", Vec::new(), None)).unwrap();

    assert_eq!(response.results[0].status, EditionIdentityStatus::Ambiguous);
    assert!(response.results[0].matched.is_none());
    assert_eq!(response.results[0].candidates.len(), 2);
}

#[test]
fn edition_identity_keeps_equal_title_author_candidates_with_different_codes_ambiguous() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "isbn_13": ["9780306406157"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "isbn_13": ["9780821338278"], "lc_classifications": ["QA76.73.R87"]
            }),
        ],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("The Example Book", Vec::new(), None)).unwrap();

    assert_eq!(response.results[0].status, EditionIdentityStatus::Ambiguous);
    assert_eq!(response.results[0].candidates.len(), 2);
}

#[test]
fn edition_identity_combines_classification_sources_and_suggests_only_one_edition_per_work() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
                "title": "The Example Book", "isbn_13": ["9780306406157"],
                "dewey_decimal_class": ["005.1"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}],
                "title": "The Example Book", "isbn_13": ["9780821338278"],
                "lc_classifications": ["QA76.73.R87"]
            }),
        ],
        &[serde_json::json!({"key": "/works/OL10W", "title": "The Example Book", "lc_classifications": ["QA76.6"]})],
        &[],
    );
    let service = MetadataService::open(&database).unwrap();

    let response =
        service.resolve_editions(EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "query".to_owned(), title: "The Example Book".to_owned(), authors: Vec::new(), publishers: Vec::new(), book_year: None }] }).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Ambiguous);
    assert_eq!(response.results[0].candidates.len(), 1);
    assert_eq!(response.results[0].candidates[0].open_library_work_id.as_deref(), Some("OL10W"));
    assert!(!response.results[0].candidates[0].classifications.iter().any(|classification| classification.scheme == ClassificationScheme::DeweyDecimal));
    assert!(response.results[0].candidates[0].classifications.iter().any(|classification| classification.notation == "QA76.6" && classification.source == ClassificationSource::Work));
    assert!(!response.results[0].candidates[0].classifications.iter().any(|classification| classification.notation == "QA76.73.R87"));
}

#[test]
fn classification_lookup_requires_consensus_when_sibling_evidence_is_sparse() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "works": [{"key": "/works/OL10W"}],
                "title": "The Example Book", "isbn_13": ["9780306406157"],
                "dewey_decimal_class": ["005.1"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "works": [{"key": "/works/OL10W"}],
                "title": "The Example Book", "isbn_13": ["9780821338278"],
                "lc_classifications": ["QA76.73.R87"]
            }),
        ],
        &[serde_json::json!({"key": "/works/OL10W", "title": "The Example Book"})],
        &[],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.lookup(ClassificationRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    assert_eq!(response.results[0].matches[0].classifications, vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "QA76.73".into(), source: ClassificationSource::Work }]);
}

#[test]
fn classification_lookup_prefers_direct_work_codes_to_sibling_codes() {
    let temporary = tempfile::tempdir().unwrap();
    let editions = [
        serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9780306406157"]}),
        serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9780821338278"],"lc_classifications":["QA76.6.A1"],"dewey_decimal_class":["005.1"]}),
        serde_json::json!({"key":"/books/OL3M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9783161484100"],"lc_classifications":["QA76.6.B2"],"dewey_decimal_class":["005.1"]}),
        serde_json::json!({"key":"/books/OL4M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9781861972712"],"lc_classifications":["PN1993.A1"],"dewey_decimal_class":["791.43"]}),
        serde_json::json!({"key":"/books/OL5M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9780140328721"],"lc_classifications":["PN1993.B2"],"dewey_decimal_class":["791.43"]}),
    ];
    let noisy = (1..=13).map(|number| format!("B{number}")).collect::<Vec<_>>();
    let works = [serde_json::json!({"key":"/works/OL10W","title":"Example","lc_classifications":noisy})];
    let database = snapshot_from(temporary.path(), &editions, &works, &[]);
    let service = MetadataService::open(&database).unwrap();

    let response = service.lookup(ClassificationRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    let classifications = &response.results[0].matches[0].classifications;
    assert_eq!(classifications, &[Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "PN1993".to_owned(), source: ClassificationSource::Work },]);
}

#[test]
fn classification_lookup_prefers_exact_edition_codes_to_work_and_sibling_codes() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key":"/books/OL1M", "works":[{"key":"/works/OL10W"}], "title":"Albion's Seed",
                "isbn_13":["9780306406157"], "lc_classifications":["E169.1.F539"]
            }),
            serde_json::json!({
                "key":"/books/OL2M", "works":[{"key":"/works/OL10W"}], "title":"Albion's Seed",
                "isbn_13":["9780821338278"], "lc_classifications":["E163"]
            }),
        ],
        &[serde_json::json!({"key":"/works/OL10W", "title":"Albion's Seed", "lc_classifications":["E162"]})],
        &[],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.lookup(ClassificationRequest { isbns: vec!["9780306406157".to_owned()] }).unwrap();
    assert_eq!(response.results[0].matches[0].classifications, vec![Classification { evidence: Vec::new(), scheme: ClassificationScheme::LibraryOfCongress, notation: "E169.1.F539".to_owned(), source: ClassificationSource::ExactEdition }]);
}

#[test]
fn serving_rejects_a_snapshot_outside_the_supported_schema_set() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(temporary.path(), &[], &[], &[]);
    let connection = Connection::open(&database).unwrap();
    connection.execute("UPDATE snapshot SET schema_version=?1 WHERE singleton=1", [SCHEMA_VERSION + 1]).unwrap();
    drop(connection);

    let failure = match MetadataService::open(&database) {
        Ok(_) => panic!("an unsupported snapshot must not be served"),
        Err(failure) => failure,
    };
    assert!(failure.to_string().contains(&format!("schema {}", SCHEMA_VERSION + 1)), "unexpected rejection: {failure}");
    assert!(failure.to_string().contains(&format!("expected {SCHEMA_VERSION}")), "unexpected rejection: {failure}");
}

/// Only the current builder version is supported, so an unfinished build
/// from an older builder must be rejected without mutating its checkpoints.
#[test]
fn unfinished_build_from_an_older_builder_is_rejected_without_being_mutated() {
    let temporary = tempfile::tempdir().unwrap();
    let editions = temporary.path().join("ol_dump_editions_2026-08-01.txt.gz");
    let works = temporary.path().join("ol_dump_works_2026-08-01.txt.gz");
    let authors = temporary.path().join("ol_dump_authors_2026-08-01.txt.gz");
    dump(&editions, &[serde_json::json!({"key": "/books/OL1M", "works": [{"key": "/works/OL10W"}], "title": "The Example Book", "isbn_13": ["9780306406157"]})]);
    dump(&works, &[serde_json::json!({"key": "/works/OL10W", "title": "Canonical Example Work"})]);
    dump(&authors, &[]);
    let database = temporary.path().join("snapshot.sqlite");
    let building = building_path(&database);

    // Model an unfinished v6 build by removing the later work-title tables.
    let connection = Connection::open(&building).unwrap();
    configure_import(&connection).unwrap();
    create_schema(&connection).unwrap();
    register_import_inputs(&connection, &editions, &works, &authors, None).unwrap();
    connection
        .execute_batch(
            "DROP INDEX work_bibliography_by_title;
                 DROP TABLE work_bibliography;
                 DROP TABLE work_bibliography_stage;
                 UPDATE metadata_metric SET value=6 WHERE name='builder_schema_version';",
        )
        .unwrap();
    drop(connection);

    let failure = import_snapshot(&editions, &works, &authors, &database, None).expect_err("an unfinished build from an older builder must be rejected");
    assert!(failure.to_string().contains("builder schema"), "unexpected rejection: {failure}");

    let connection = Connection::open(&building).unwrap();
    assert_eq!(connection.query_row("SELECT value FROM metadata_metric WHERE name='builder_schema_version'", (), |row| row.get::<_, i64>(0)).unwrap(), 6, "a rejected resume must not restamp the builder version");
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='work_bibliography'", (), |row| row.get::<_, i64>(0)).unwrap(), 0, "a rejected resume must not commit schema changes");
}

#[test]
fn loc_dump_extends_existing_sqlite_and_mmap_isbn_lookup() {
    let temp = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temp.path(),
        &[
            serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL1W"}],"isbn_13":["9780306406157"],"dewey_decimal_class":["940"]}),
            serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL2W"}],"isbn_13":["9780062097729"]}),
            serde_json::json!({"key":"/books/OL3M","works":[{"key":"/works/OL3W"}],"isbn_13":["9780062097729"]}),
        ],
        &[],
        &[],
    );
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE isbn_lcc(isbn13 INTEGER,notation TEXT,lc_record_id TEXT,PRIMARY KEY(isbn13,notation,lc_record_id)) WITHOUT ROWID;
        INSERT INTO isbn_lcc VALUES(9780306406157,'DC161','lc1'),(9780306406157,'DC161','lc2'),(9780131103627,'QA76.73.C15','lc3'),(9780062097729,'E169','lc4');",
        )
        .unwrap();
    connection
        .execute_batch(
            "CREATE TABLE lc_record(lc_record_id TEXT PRIMARY KEY,metadata TEXT); CREATE TABLE lc_record_isbn(isbn13 INTEGER,lc_record_id TEXT,PRIMARY KEY(isbn13,lc_record_id)) WITHOUT ROWID;
        INSERT INTO lc_record_isbn VALUES(9780306406157,'lc1'),(9780131103627,'lc3');",
        )
        .unwrap();
    let extra = serde_json::json!({"descriptions":["An LC authority summary."],"subjects":[{"source":"library_of_congress","authority":"lcsh","components":["France","History"],"authority_uri":null}]}).to_string();
    connection.execute("INSERT INTO lc_record VALUES('lc1',?1),('lc3',?1)", [extra]).unwrap();
    drop(connection);
    let service = MetadataService::open(&database).unwrap();
    let request = ClassificationRequest { isbns: vec!["0-306-40615-2".into(), "9780131103627".into(), "9780062097729".into(), "invalid".into()] };
    let response = service.lookup(request.clone()).unwrap();
    assert_eq!(response.results[0].status, LookupStatus::Matched);
    assert_eq!(response.results[0].matches[0].exact_edition_ids, vec!["OL1M"]);
    let codes = &response.results[0].matches[0].classifications;
    assert_eq!(codes.iter().filter(|c| c.notation == "DC161").count(), 1);
    assert!(codes.iter().all(|c| c.scheme != ClassificationScheme::DeweyDecimal));
    assert_eq!(response.results[1].status, LookupStatus::Matched);
    assert_eq!(response.results[1].matches[0].open_library_work_id, None);
    assert!(response.results[1].matches[0].exact_edition_ids.is_empty());
    assert_eq!(response.results[1].matches[0].classifications[0].notation, "QA76.73.C15");
    assert_eq!(response.results[2].status, LookupStatus::Ambiguous);
    assert!(response.results[2].matches.iter().all(|m| m.classifications.is_empty()));
    assert_eq!(response.results[3].status, LookupStatus::InvalidIsbn);
    for enrich in [service.enrich_rich(MetadataEnrichmentRequest { isbns: request.isbns.clone() }).unwrap(), service.classify_rich(MetadataEnrichmentRequest { isbns: request.isbns.clone() }).unwrap()] {
        assert!(enrich.results[0].matches[0].classifications.iter().any(|c| c.notation == "DC161"));
        assert_eq!(enrich.results[1].matches[0].classifications[0].notation, "QA76.73.C15");
        assert_eq!(enrich.results[1].matches[0].description.as_deref(), Some("An LC authority summary."));
        assert_eq!(enrich.results[1].matches[0].subjects[0].source, "library_of_congress");
    }
    let index = temp.path().join("subjects.idx");
    build_subject_index(&database, &index).unwrap();
    let indexed = MetadataService::open(&database).unwrap().with_subject_index(&index).unwrap();
    assert_eq!(serde_json::to_value(response).unwrap(), serde_json::to_value(indexed.lookup(request.clone()).unwrap()).unwrap());
    let subjects = temp.path().join("subjects.sqlite");
    let rich = temp.path().join("rich.sqlite");
    let identities = temp.path().join("identities.sqlite");
    split_snapshot(&database, &subjects, &rich, &identities).unwrap();
    let split = MetadataService::open_with_stores(subjects, rich, identities, None::<PathBuf>).unwrap();
    assert_eq!(split.lookup(request.clone()).unwrap().results[1].matches[0].classifications[0].notation, "QA76.73.C15");
    assert_eq!(split.enrich_rich(MetadataEnrichmentRequest { isbns: request.isbns }).unwrap().results[1].matches[0].description.as_deref(), Some("An LC authority summary."));
}

pub(super) fn duplicate_work_fixture(path: &Path, candidate_title: &str, candidate_author: &str) -> PathBuf {
    snapshot_from(
        path,
        &[
            serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Empire of Wealth","isbn_13":["9780061847646"],"authors":[{"key":"/authors/OL1A"}]}),
            serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL20W"}],"title":candidate_title,"isbn_13":["9780060093624"],"authors":[{"key":"/authors/OL2A"}],"lc_classifications":["HC103 .G673 2004"]}),
        ],
        &[
            serde_json::json!({"key":"/works/OL10W","title":"Empire of Wealth","authors":[{"author":{"key":"/authors/OL1A"}}]}),
            serde_json::json!({"key":"/works/OL20W","title":"An Empire of Wealth","authors":[{"author":{"key":"/authors/OL2A"}}]}),
        ],
        &[serde_json::json!({"key":"/authors/OL1A","name":"John Steele Gordon"}), serde_json::json!({"key":"/authors/OL2A","name":candidate_author})],
    )
}

#[test]
fn duplicate_work_recovers_lcc_preserving_identity_and_provenance_in_all_stores() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth: The Epic History of American Economic Power", "Gordon, John Steele");
    let subjects = temp.path().join("subjects.sqlite");
    let rich = temp.path().join("rich.sqlite");
    let identity = temp.path().join("identity.sqlite");
    split_snapshot(&database, &subjects, &rich, &identity).unwrap();
    let index = temp.path().join("subjects.idx");
    build_subject_index_limited(&subjects, &index, None).unwrap();
    for service in [MetadataService::open(&database).unwrap(), MetadataService::open_with_stores(&subjects, &rich, &identity, None::<PathBuf>).unwrap().with_subject_index(&index).unwrap()] {
        let response = service.lookup(ClassificationRequest { isbns: vec!["9780061847646".into()] }).unwrap();
        let matched = &response.results[0].matches[0];
        assert_eq!(response.results[0].canonical_isbn13.as_deref(), Some("9780061847646"));
        assert_eq!(matched.open_library_work_id.as_deref(), Some("OL10W"));
        assert_eq!(matched.exact_edition_ids, vec!["OL1M"]);
        assert_eq!(matched.classifications.len(), 1);
        let c = &matched.classifications[0];
        assert_eq!(c.notation, "HC103");
        assert_eq!(c.source, ClassificationSource::Work);
        assert_eq!(c.evidence[0].isbn13, "9780060093624");
        assert_eq!(c.evidence[0].open_library_work_id, "OL20W");
        for enriched in [service.classify_rich(MetadataEnrichmentRequest { isbns: vec!["9780061847646".into()] }).unwrap(), service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780061847646".into()] }).unwrap()] {
            assert_eq!(enriched.results[0].matches[0].classifications, matched.classifications);
            let encoded = metadata_contract::encode_v2_response(&enriched).unwrap();
            let decoded = metadata_contract::decode_v2_response(&encoded, metadata_contract::MAX_RESPONSE_BYTES).unwrap();
            assert_eq!(decoded.results[0].matches[0].classifications, matched.classifications);
            let merged = metadata_contract::merge_rich_work_matches(&decoded.results[0].matches, |_, _| Vec::new());
            assert_eq!(merged.classifications, matched.classifications);
        }
    }
}

#[test]
fn duplicate_work_rejects_different_authors_adaptations_and_volumes() {
    for (title, author) in [
        ("An Empire of Wealth", "John Smith"),
        ("An Empire of Wealth: Study Guide", "John Steele Gordon"),
        ("An Empire of Wealth (Abridged)", "John Steele Gordon"),
        ("An Empire of Wealth II", "John Steele Gordon"),
        ("An Empire of Wealth: Volume 2", "John Steele Gordon"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let database = duplicate_work_fixture(temp.path(), title, author);
        let response = MetadataService::open(database).unwrap().lookup(ClassificationRequest { isbns: vec!["9780061847646".into()] }).unwrap();
        assert!(response.results[0].matches[0].classifications.is_empty(), "{title} / {author}");
    }
}

#[test]
fn duplicate_work_collects_differing_codes_and_keeps_existing_lcc() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth", "John Steele Gordon");
    let c = Connection::open(&database).unwrap();
    c.execute("INSERT INTO edition_classification(edition_id,scheme,notation) VALUES(2,?1,'D1')", [LCC_SCHEME]).unwrap();
    c.execute_batch("INSERT INTO edition VALUES(3,30); INSERT INTO edition_isbn VALUES(9780306406157,3); INSERT INTO edition_bibliography VALUES(3,'Empire of Wealth','empire of wealth',2005); INSERT INTO edition_author VALUES(3,0,2);")
        .unwrap();
    c.execute("INSERT INTO edition_classification(edition_id,scheme,notation) VALUES(3,?1,'HC103')", [LCC_SCHEME]).unwrap();
    let request = || ClassificationRequest { isbns: vec!["9780061847646".into()] };
    let recovered = MetadataService::open(&database).unwrap().lookup(request()).unwrap();
    let codes = &recovered.results[0].matches[0].classifications;
    assert!(codes.iter().any(|c| c.notation == "D1"));
    assert!(codes.iter().any(|c| c.notation == "HC103"));
    assert!(codes.iter().all(|c| !c.evidence.is_empty()));
    c.execute("INSERT INTO edition_classification(edition_id,scheme,notation) VALUES(1,?1,'E1')", [LCC_SCHEME]).unwrap();
    let response = MetadataService::open(database).unwrap().lookup(request()).unwrap();
    assert_eq!(response.results[0].matches[0].classifications[0].notation, "E1");
    assert!(response.results[0].matches[0].classifications[0].evidence.is_empty());
}

#[test]
fn duplicate_work_uses_lc_dump_evidence_in_split_identity_store() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth", "John Steele Gordon");
    let c = Connection::open(&database).unwrap();
    c.execute_batch("DELETE FROM edition_classification; DELETE FROM edition_work_classification; CREATE TABLE isbn_lcc(isbn13 INTEGER,notation TEXT,lc_record_id TEXT,PRIMARY KEY(isbn13,notation,lc_record_id)) WITHOUT ROWID; INSERT INTO isbn_lcc VALUES(9780060093624,'HC103','LC1');").unwrap();
    c.execute("INSERT INTO edition_classification VALUES(1,?1,'330.973')", [1_i64]).unwrap();
    let subjects = temp.path().join("subjects.sqlite");
    let rich = temp.path().join("rich.sqlite");
    let identity = temp.path().join("identity.sqlite");
    split_snapshot(&database, &subjects, &rich, &identity).unwrap();
    let service = MetadataService::open_with_stores(&subjects, &rich, &identity, None::<PathBuf>).unwrap();
    let result = service.lookup(ClassificationRequest { isbns: vec!["9780061847646".into()] }).unwrap();
    let codes = &result.results[0].matches[0].classifications;
    assert!(!codes.iter().any(|c| c.notation == "330.973"));
    assert!(codes.iter().any(|c| c.notation == "HC103" && c.evidence[0].isbn13 == "9780060093624"));
}

#[test]
fn duplicate_work_does_not_pick_an_arbitrary_match_from_truncated_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth", "John Steele Gordon");
    let c = Connection::open(&database).unwrap();
    for id in 100..230 {
        c.execute("INSERT INTO edition VALUES(?1,?1)", [id]).unwrap();
        c.execute("INSERT INTO edition_bibliography VALUES(?1,'Empire of Wealth','empire of wealth',2000)", [id]).unwrap();
        c.execute("INSERT INTO edition_author VALUES(?1,0,2)", [id]).unwrap();
    }
    let result = MetadataService::open(database).unwrap().lookup(ClassificationRequest { isbns: vec!["9780061847646".into()] }).unwrap();
    assert!(result.results[0].matches[0].classifications.is_empty());
}

#[test]
fn duplicate_work_filters_unrelated_authors_before_candidate_limit() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth", "John Steele Gordon");
    let c = Connection::open(&database).unwrap();
    c.execute("INSERT INTO author VALUES(99,'Unrelated Writer')", []).unwrap();
    for id in 100..400 {
        c.execute("INSERT INTO edition VALUES(?1,?1)", [id]).unwrap();
        c.execute("INSERT INTO edition_bibliography VALUES(?1,'Empire of Wealth','empire of wealth',2000)", [id]).unwrap();
        c.execute("INSERT INTO edition_author VALUES(?1,0,99)", [id]).unwrap();
    }
    let result = MetadataService::open(database).unwrap().lookup(ClassificationRequest { isbns: vec!["9780061847646".into()] }).unwrap();
    assert_eq!(result.results[0].matches[0].classifications[0].notation, "HC103");
}

#[test]
fn single_classified_sibling_is_used_until_another_sibling_has_codes() {
    let temp = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temp.path(),
        &[
            serde_json::json!({"key":"/books/OL1M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9780306406157"]}),
            serde_json::json!({"key":"/books/OL2M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9780821338278"],"lc_classifications":["HE2751 .A1","HE2751.A2"],"dewey_decimal_class":["385"]}),
            serde_json::json!({"key":"/books/OL3M","works":[{"key":"/works/OL10W"}],"title":"Example","isbn_13":["9783161484100"]}),
        ],
        &[serde_json::json!({"key":"/works/OL10W","title":"Example"})],
        &[],
    );
    for conflicting in [false, true] {
        if conflicting {
            Connection::open(&database).unwrap().execute("INSERT INTO edition_classification VALUES(3,?1,'D1')", [LCC_SCHEME]).unwrap();
        }
        let index = temp.path().join(if conflicting { "conflict.idx" } else { "single.idx" });
        build_subject_index_limited(&database, &index, None).unwrap();
        for service in [MetadataService::open(&database).unwrap(), MetadataService::open(&database).unwrap().with_subject_index(&index).unwrap()] {
            let response = service.lookup(ClassificationRequest { isbns: vec!["9780306406157".into()] }).unwrap();
            let codes = &response.results[0].matches[0].classifications;
            if conflicting {
                assert!(codes.is_empty());
            } else {
                assert_eq!(codes.len(), 1);
                assert!(codes.iter().any(|c| c.notation == "HE2751" && c.source == ClassificationSource::Work));
                assert!(!codes.iter().any(|c| c.notation == "385"));
            }
        }
    }
}

#[test]
fn edition_identity_tries_main_title_before_long_subtitle_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth", "John Steele Gordon");
    let response = MetadataService::open(database)
        .unwrap()
        .resolve_editions(EditionIdentityRequest {
            queries: vec![EditionIdentityQuery {
                query_id: "main".into(),
                title: format!("An Empire of Wealth: {}", "a very long descriptive subtitle about the political social and economic history of a nation ".repeat(3)),
                authors: vec!["John Steele Gordon".into()],
                publishers: Vec::new(),
                book_year: None,
            }],
        })
        .unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::Matched);
    assert_eq!(response.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL2M");
}

#[test]
fn edition_identity_preserves_volume_when_searching_main_title() {
    let temp = tempfile::tempdir().unwrap();
    let database = duplicate_work_fixture(temp.path(), "An Empire of Wealth: Volume 2", "John Steele Gordon");
    let response = MetadataService::open(database)
        .unwrap()
        .resolve_editions(EditionIdentityRequest {
            queries: vec![EditionIdentityQuery { query_id: "volume".into(), title: "An Empire of Wealth: Volume 4: A long subtitle".into(), authors: vec!["John Steele Gordon".into()], publishers: Vec::new(), book_year: None }],
        })
        .unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::NoMatch);
}

#[test]
fn edition_identity_preserves_conflicting_years_even_for_duplicate_isbns() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({
                "key": "/books/OL1M", "publish_date": "1999", "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "isbn_13": ["9780306406157"], "lc_classifications": ["QA76.6"]
            }),
            serde_json::json!({
                "key": "/books/OL2M", "publish_date": "2013", "authors": [{"key": "/authors/OL100A"}],
                "title": "The Example Book", "isbn_13": ["9780306406157"], "lc_classifications": ["QA76.6"]
            }),
        ],
        &[],
        &[serde_json::json!({"key": "/authors/OL100A", "name": "Alex Smith"})],
    );
    let service = MetadataService::open(&database).unwrap();

    let response = service.resolve_editions(edition_query("The Example Book", Vec::new(), None)).unwrap();

    assert_eq!(response.results[0].status, EditionIdentityStatus::Ambiguous);
    assert!(response.results[0].matched.is_none());
    assert_eq!(response.results[0].candidates.len(), 2);
}

#[test]
fn lc_title_author_fallback_works_in_combined_and_split_stores() {
    let temp = tempfile::tempdir().unwrap();
    let database = snapshot_from(temp.path(), &[], &[], &[]);
    let c = Connection::open(&database).unwrap();
    c.execute_batch("CREATE TABLE lc_record(lc_record_id TEXT PRIMARY KEY,metadata TEXT); CREATE TABLE lc_record_lcc(lc_record_id TEXT,notation TEXT); CREATE TABLE lc_title(title_key TEXT,lc_record_id TEXT,PRIMARY KEY(title_key,lc_record_id)); INSERT INTO lc_record_lcc VALUES('123','DC161'); INSERT INTO lc_title VALUES('french history','123');").unwrap();
    c.execute("INSERT INTO lc_record VALUES('123',?1)", [serde_json::json!({"title":"French history","main_title":"French history","authors":["Smith, Alex"]}).to_string()]).unwrap();
    drop(c);
    let request = edition_query("French history", vec![], None);
    let service = MetadataService::open(&database).unwrap();
    let response = service.resolve_editions(request.clone()).unwrap();
    assert_eq!(response.results[0].status, EditionIdentityStatus::NoMatch);
    assert!(response.results[0].matched.is_none());
    assert_eq!(response.results[0].authority_subjects.as_ref().unwrap().record_ids, vec!["123"]);
    let subjects = temp.path().join("subjects.sqlite");
    let rich = temp.path().join("rich.sqlite");
    let identities = temp.path().join("identities.sqlite");
    split_snapshot(&database, &subjects, &rich, &identities).unwrap();
    let split = MetadataService::open_with_stores(subjects, rich, identities, None::<PathBuf>).unwrap();
    assert_eq!(serde_json::to_value(response).unwrap(), serde_json::to_value(split.resolve_editions(request).unwrap()).unwrap());
}

#[test]
fn edition_identity_returns_separate_subtitle_and_prefers_full_title() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({"key":"/books/OL1M", "title":"Logic", "subtitle":"A Very Short Introduction", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780306406157"], "lc_classifications":["BC71 .P75 2000"]}),
            serde_json::json!({"key":"/books/OL2M", "title":"Logic", "authors":[{"key":"/authors/OL200A"}], "isbn_13":["9780821338278"], "lc_classifications":["BC108"]}),
        ],
        &[],
        &[serde_json::json!({"key":"/authors/OL100A", "name":"Graham Priest"}), serde_json::json!({"key":"/authors/OL200A", "name":"Other Author"})],
    );
    let service = MetadataService::open(&database).unwrap();
    let response =
        service.resolve_editions(EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "logic".into(), title: "Logic: A Very Short Introduction".into(), authors: vec![], publishers: vec![], book_year: None }] }).unwrap();
    let result = &response.results[0];
    let candidates = result.matched.iter().chain(result.candidates.iter()).collect::<Vec<_>>();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].title, "Logic");
    assert_eq!(candidates[0].subtitle.as_deref(), Some("A Very Short Introduction"));
    assert_eq!(candidates[0].authors, ["Graham Priest"]);
    let encoded = metadata_contract::encode_v2_edition_identity_response(&response).unwrap();
    let decoded = metadata_contract::decode_v2_edition_identity_response(&encoded, metadata_contract::MAX_RESPONSE_BYTES).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::to_value(response).unwrap());
}

#[test]
fn edition_identity_matches_possessive_apostrophe_before_substrings() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({"key":"/books/OL1M", "title":"The Startup Owners Manual", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780984999309"], "lc_classifications":["HD62.5 .B53 2012"]}),
            serde_json::json!({"key":"/books/OL2M", "title":"Manual", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780306406157"], "lc_classifications":["BC108"]}),
        ],
        &[],
        &[serde_json::json!({"key":"/authors/OL100A", "name":"Steve Blank and Bob Dorf"})],
    );
    let service = MetadataService::open(&database).unwrap();
    for apostrophe in ["'", "’", "ʼ"] {
        let response = service
            .resolve_editions(EditionIdentityRequest {
                queries: vec![EditionIdentityQuery {
                    query_id: "startup".into(),
                    title: format!("The Startup Owner{apostrophe}s Manual: The Step-by-Step Guide for Building a Great Company"),
                    authors: vec!["Blank, Steve".into(), "Dorf, Bob".into()],
                    publishers: vec![],
                    book_year: None,
                }],
            })
            .unwrap();
        let matched = response.results[0].matched.as_ref().unwrap();
        assert_eq!(matched.open_library_edition_id, "OL1M");
    }
}

#[test]
fn rebuild_existing_titles_backfills_subtitles_without_changing_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = snapshot_from(
        temp.path(),
        &[serde_json::json!({"key":"/books/OL1M","title":"Owner’s Manual","subtitle":"A Practical Guide","works":[{"key":"/works/OL10W"}],"isbn_13":["9780306406157"],"lc_classifications":["HD62.5"]})],
        &[serde_json::json!({"key":"/works/OL10W","title":"Owner’s Manual","subtitle":"A Practical Guide"})],
        &[],
    );
    let c = Connection::open(&source).unwrap();
    c.execute_batch("DROP TABLE edition_subtitle; DROP TABLE work_subtitle; UPDATE edition_bibliography SET normalized_title='owner s manual'; UPDATE work_bibliography SET normalized_title='owner s manual';").unwrap();
    drop(c);
    let original = std::fs::read(&source).unwrap();
    let output = temp.path().join("rebuilt.sqlite");
    crate::rebuild_titles(&source, &temp.path().join("ol_dump_editions_2026-08-01.txt.gz"), &temp.path().join("ol_dump_works_2026-08-01.txt.gz"), &output).unwrap();
    assert_eq!(original, std::fs::read(&source).unwrap());
    let c = Connection::open(&output).unwrap();
    assert_eq!(c.query_row("SELECT normalized_title FROM edition_bibliography", [], |r| r.get::<_, String>(0)).unwrap(), "owners manual");
    assert_eq!(c.query_row("SELECT normalized_title FROM work_subtitle", [], |r| r.get::<_, String>(0)).unwrap(), "owners manual a practical guide");
    for title in ["Owners Manual: A Practical Guide", "Owner's Manual: A Practical Guide", "Owner’s Manual: A Practical Guide"] {
        let result = MetadataService::open(&output)
            .unwrap()
            .resolve_editions(EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "test".into(), title: title.into(), authors: vec![], publishers: vec![], book_year: None }] })
            .unwrap();
        assert_eq!(result.results[0].candidates[0].subtitle.as_deref(), Some("A Practical Guide"));
    }
}

#[test]
fn edition_identity_fallback_waits_for_usable_candidates_and_preserves_ambiguity() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[
            serde_json::json!({"key":"/books/OL1M", "title":"Logic", "subtitle":"Rejected Author", "authors":[{"key":"/authors/OL200A"}], "isbn_13":["9780306406157"], "lc_classifications":["BC71"]}),
            serde_json::json!({"key":"/books/OL2M", "title":"Logic", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780821338278"], "lc_classifications":["BC108"]}),
            serde_json::json!({"key":"/books/OL3M", "title":"Reason", "subtitle":"Unusable Code", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9783161484100"], "lc_classifications":["BC71"]}),
            serde_json::json!({"key":"/books/OL4M", "title":"Reason", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780984999309"], "lc_classifications":["BC108"]}),
            serde_json::json!({"key":"/books/OL5M", "title":"Thinking", "subtitle":"Conflicting Subjects", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780306406157"], "lc_classifications":["BC71"]}),
            serde_json::json!({"key":"/books/OL6M", "title":"Thinking", "subtitle":"Conflicting Subjects", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780821338278"], "lc_classifications":["BC108"]}),
            serde_json::json!({"key":"/books/OL7M", "title":"Thinking", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9783161484100"], "lc_classifications":["BC177"], "publishers":["Preferred Publisher"]}),
        ],
        &[],
        &[serde_json::json!({"key":"/authors/OL100A", "name":"Alex Smith"}), serde_json::json!({"key":"/authors/OL200A", "name":"Other Writer"})],
    );
    // Leave a raw classification row so SQL finds the candidate, but the
    // evidence resolver must reject it rather than stopping title fallback.
    let connection = Connection::open(&database).unwrap();
    connection.execute("UPDATE edition_classification SET notation='invalid' WHERE edition_id=3", []).unwrap();
    drop(connection);
    let service = MetadataService::open(&database).unwrap();
    let queries = [
        ("author", "Logic: Rejected Author", vec!["Alex Smith"]),
        ("code", "Reason: Unusable Code", vec!["Alex Smith"]),
        ("ambiguous", "Thinking: Conflicting Subjects", vec!["Alex Smith"]),
        ("unknown-author", "Logic: Rejected Author", vec![]),
        ("exhausted", "Logic: Rejected Author", vec!["Unrelated Person"]),
        ("empty", "", vec![]),
    ]
    .into_iter()
    .map(|(id, title, authors)| EditionIdentityQuery { query_id: id.into(), title: title.into(), authors: authors.into_iter().map(str::to_owned).collect(), publishers: vec!["Preferred Publisher".into()], book_year: None })
    .collect();
    let results = service.resolve_editions(EditionIdentityRequest { queries }).unwrap().results;
    assert_eq!(results.iter().map(|r| r.query_id.as_str()).collect::<Vec<_>>(), vec!["author", "code", "ambiguous", "unknown-author", "exhausted", "empty"]);
    assert_eq!(results[0].matched.as_ref().unwrap().open_library_edition_id, "OL2M");
    assert_eq!(results[1].matched.as_ref().unwrap().open_library_edition_id, "OL4M");
    assert_eq!(results[2].status, EditionIdentityStatus::Ambiguous);
    assert_eq!(results[2].candidates.iter().map(|c| c.open_library_edition_id.as_str()).collect::<Vec<_>>(), vec!["OL5M", "OL6M"]);
    assert_eq!(results[3].status, EditionIdentityStatus::Ambiguous);
    assert_eq!(results[3].candidates.len(), 1);
    assert_eq!(results[3].candidates[0].open_library_edition_id, "OL1M");
    assert_eq!(results[4].status, EditionIdentityStatus::NoMatch);
    assert_eq!(results[5].status, EditionIdentityStatus::InsufficientMetadata);
}

#[test]
fn wikidata_ingestion_persists_books_and_authors_without_runtime_source() {
    let temp = tempfile::tempdir().unwrap();
    let database = snapshot_from(temp.path(), &[], &[], &[]);
    let sidecar = temp.path().join("wikidata.sqlite");
    let c = Connection::open(&sidecar).unwrap();
    c.execute_batch(include_str!("../wikidata-author-schema.sql")).unwrap();
    c.execute_batch("INSERT INTO state VALUES('status','building'),('schema_version','1'),('ol_dump_date','2026-08-01');INSERT INTO isbn VALUES('9780306406157','Q10');INSERT INTO profile VALUES('Q10','Wikidata book','[]',NULL,NULL,'[]',NULL,NULL);INSERT INTO classification VALUES('Q10','lcc','QA76.6','P8360',0,'Q10');INSERT INTO profile VALUES('Q1','Alex Smith','[]',NULL,NULL,'[]',NULL,NULL);INSERT INTO credit VALUES('Q10','Q1',0);INSERT INTO isbn_author VALUES('9780306406157','Q10','Q10','Q1','Q10');").unwrap();
    let staging = temp.path().join("ingested.sqlite");
    std::fs::copy(&database, &staging).unwrap();
    assert!(ingest_wikidata_authorities(&MetadataService::open(&database).unwrap(), &sidecar, &staging).is_err());
    c.execute("UPDATE state SET value='complete' WHERE key='status'", []).unwrap();
    drop(c);
    ingest_wikidata_authorities(&MetadataService::open(&database).unwrap(), &sidecar, &staging).unwrap();
    std::fs::remove_file(&sidecar).unwrap();
    let service = MetadataService::open(&staging).unwrap();
    let response = service.enrich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap();
    assert_eq!(response.results[0].status, LookupStatus::Matched);
    assert_eq!(response.results[0].matches[0].authors[0].identity_key(), "wikidata:Q1");
    let response = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap();
    assert_eq!(response.results[0].matches[0].exact_edition_ids, vec!["wikidata:Q10"]);
    let response = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["0-306-40615-2".into(), "bad".into(), "9780306406157".into()] }).unwrap();
    assert_eq!(response.results.len(), 3);
    assert_eq!(response.results[0].requested_isbn, "0-306-40615-2");
    assert_eq!(response.results[0].status, LookupStatus::Matched);
    assert_eq!(response.results[1].status, LookupStatus::InvalidIsbn);
    assert_eq!(response.results[2].matches[0].authors[0].identity_key(), "wikidata:Q1");
    drop(service);
    let service = MetadataService::open(&staging).unwrap();
    let response = service.lookup(ClassificationRequest { isbns: vec!["9780306406157".into()] }).unwrap();
    assert_eq!(response.results[0].status, LookupStatus::Matched);
    assert_eq!(Connection::open(&staging).unwrap().query_row("SELECT name FROM author WHERE author_id=-1", [], |r| r.get::<_, String>(0)).unwrap(), "Alex Smith");
    let c = Connection::open(&staging).unwrap();
    assert_eq!(c.query_row("SELECT edition_id FROM edition_isbn WHERE isbn13=9780306406157", [], |r| r.get::<_, i64>(0)).unwrap(), -10);
    assert_eq!(c.query_row("SELECT notation FROM isbn_classification WHERE isbn13=9780306406157 AND scheme=2", [], |r| r.get::<_, String>(0)).unwrap(), "QA76.6");
    assert!(c.query_row("SELECT COUNT(*) FROM title_author_classification WHERE normalized_title='wikidata book' AND scheme=2", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
    let response =
        service.resolve_editions(EditionIdentityRequest { queries: vec![EditionIdentityQuery { query_id: "wd".into(), title: "Wikidata book".into(), authors: vec!["Alex Smith".into()], publishers: vec![], book_year: None }] }).unwrap();
    assert_eq!(response.results[0].matched.as_ref().unwrap().canonical_isbn13, "9780306406157");
    assert!(response.results[0].matched.as_ref().unwrap().open_library_edition_id.is_empty());
}

#[test]
fn wikidata_author_corroboration_uses_identity_store_in_split_deployments() {
    let temp = tempfile::tempdir().unwrap();
    let database =
        snapshot_from(temp.path(), &[serde_json::json!({"key":"/books/OL1M","title":"Book","isbn_13":["9780306406157"],"authors":[{"key":"/authors/OL1A"}]})], &[], &[serde_json::json!({"key":"/authors/OL1A","name":"Alex Smith"})]);
    let sidecar = temp.path().join("wikidata.sqlite");
    let c = Connection::open(&sidecar).unwrap();
    c.execute_batch(include_str!("../wikidata-author-schema.sql")).unwrap();
    c.execute_batch("INSERT INTO state VALUES('status','complete'),('schema_version','1'),('ol_dump_date','2026-08-01');INSERT INTO isbn VALUES('9780306406157','Q10');INSERT INTO profile VALUES('Q1','Alex Smith','[]',NULL,NULL,'[]',NULL,NULL);INSERT INTO credit VALUES('Q10','Q1',0);INSERT INTO isbn_author VALUES('9780306406157','Q10','Q10','Q1','Q10');").unwrap();
    drop(c);
    let subjects = temp.path().join("subjects.sqlite");
    let rich = temp.path().join("rich.sqlite");
    let identities = temp.path().join("identities.sqlite");
    split_snapshot(&database, &subjects, &rich, &identities).unwrap();
    let mut results = Vec::new();
    for (i, service) in [MetadataService::open(&database).unwrap(), MetadataService::open_with_stores(subjects.clone(), rich.clone(), identities.clone(), None::<PathBuf>).unwrap()].into_iter().enumerate() {
        let target = temp.path().join(format!("ingested-{i}.sqlite"));
        std::fs::copy(&database, &target).unwrap();
        ingest_wikidata_authorities(&service, &sidecar, &target).unwrap();
        let service = MetadataService::open(&target).unwrap();
        let response = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap();
        assert!(response.results[0].matches[0].authors[0].identifiers.is_empty());
        results.push(serde_json::to_value(response).unwrap());
    }
    results[0]["snapshot"]["imported_at_ms"] = serde_json::json!(0);
    results[1]["snapshot"]["imported_at_ms"] = serde_json::json!(0);
    assert_eq!(results[0], results[1]);
}

#[test]
fn wikidata_ingestion_fills_missing_schemes_and_links_authors_without_overwriting_existing_data() {
    let temp = tempfile::tempdir().unwrap();
    let base = snapshot_from(
        temp.path(),
        &[serde_json::json!({"key":"/books/OL1M","title":"Existing book","isbn_13":["9780306406157"],"lc_classifications":["QA1"],"authors":[{"key":"/authors/OL1A"}]})],
        &[],
        &[serde_json::json!({"key":"/authors/OL1A","name":"Alex Smith"})],
    );
    let input = temp.path().join("source.sqlite");
    let c = Connection::open(&input).unwrap();
    c.execute_batch(include_str!("../wikidata-author-schema.sql")).unwrap();
    c.execute_batch(
        "INSERT INTO state VALUES('status','complete'),('schema_version','1'),('ol_dump_date','2026-08-01');
        INSERT INTO isbn VALUES('9780306406157','Q10');
        INSERT INTO profile VALUES('Q1','Alexander Smith','[\"A. Smith\"]',1900,1980,'[{\"authority\":\"openlibrary\",\"value\":\"OL1A\"}]','Portrait.jpg',NULL);
        INSERT INTO credit VALUES('Q10','Q1',0);
        INSERT INTO isbn_author VALUES('9780306406157','Q10','Q10','Q1','Q10');
        INSERT INTO classification VALUES('Q10','lcc','QA1','P8360',0,'Q10'),('Q10','lcc','QA2','P8360',0,'Q10'),('Q10','ddc','510','P1036',0,'Q10');",
    )
    .unwrap();
    drop(c);
    let target = temp.path().join("authorities.sqlite");
    std::fs::copy(&base, &target).unwrap();
    ingest_wikidata_authorities(&MetadataService::open(&base).unwrap(), &input, &target).unwrap();
    std::fs::remove_file(&input).unwrap();
    let service = MetadataService::open(&target).unwrap();
    let result = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap().results.remove(0);
    assert_eq!(result.matches[0].authors[0].name.as_deref(), Some("Alex Smith"));
    assert!(result.matches[0].authors[0].identifiers.iter().any(|i| i.authority == "wikidata" && i.value == "Q1"));
    assert!(result.matches[0].classifications.iter().any(|c| c.notation == "QA1"));
    assert!(!result.matches[0].classifications.iter().any(|c| c.notation == "QA2"));
    let db = Connection::open(&target).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM isbn_classification WHERE isbn13=9780306406157 AND scheme=2 AND notation='QA1'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(db.query_row("SELECT COUNT(*) FROM isbn_classification WHERE isbn13=9780306406157 AND scheme=1 AND notation='510'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert!(db.query_row("SELECT COUNT(*) FROM title_author_classification WHERE normalized_title='existing book' AND scheme=1 AND notation='510'", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
    assert_eq!(db.query_row("SELECT birth_year FROM author WHERE author_id=1", [], |r| r.get::<_, i64>(0)).unwrap(), 1900);
    drop(db);
    drop(service);
    let subjects = temp.path().join("split-subjects.sqlite");
    let rich = temp.path().join("split-rich.sqlite");
    let identities = temp.path().join("split-identity.sqlite");
    split_snapshot(&target, &subjects, &rich, &identities).unwrap();
    let split = MetadataService::open_with_stores(subjects, rich, identities, None::<PathBuf>).unwrap();
    let response = split.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap();
    assert_eq!(serde_json::to_value(&response.results[0]).unwrap(), serde_json::to_value(&result).unwrap());
    let c = Connection::open(&target).unwrap();
    c.execute("UPDATE wikidata_ingestion SET status='building'", []).unwrap();
    assert!(MetadataService::open(&target).is_err());
}

#[test]
fn wikidata_bibliography_descriptions_and_books_without_isbn_survive_split() {
    let temp = tempfile::tempdir().unwrap();
    let base = snapshot_from(temp.path(), &[], &[], &[]);
    let input = temp.path().join("input.sqlite");
    let c = Connection::open(&input).unwrap();
    c.execute_batch(include_str!("../wikidata-author-schema.sql")).unwrap();
    c.execute_batch(
        "INSERT INTO state VALUES('status','complete'),('schema_version','1'),('ol_dump_date','2026-08-01'),('bibliography_status','complete');
        CREATE TABLE bibliography(book TEXT PRIMARY KEY,title TEXT,subtitle TEXT,book_year INTEGER);
        CREATE TABLE book_publisher(book TEXT,publisher TEXT,name TEXT);
        INSERT INTO bibliography VALUES('Q10','Logic','A careful introduction',2001),('Q20','Ancient letters','Collected correspondence',1850);
        INSERT INTO book_publisher VALUES('Q10','Q200','Example Press');
        INSERT INTO profile VALUES('Q10','Old label','[]',NULL,NULL,'[]',NULL,'A book about logic'),('Q20','Ancient letters','[]',NULL,NULL,'[]',NULL,'Historical correspondence'),('Q1','Jane Smith','[]',NULL,NULL,'[]',NULL,NULL);
        INSERT INTO isbn VALUES('9780306406157','Q10');
        INSERT INTO credit VALUES('Q10','Q1',0),('Q20','Q1',0);
        INSERT INTO isbn_author VALUES('9780306406157','Q10','Q10','Q1','Q10');
        INSERT INTO classification VALUES('Q10','lcc','BC71','P8360',0,'Q10'),('Q20','lcc','D52','P8360',0,'Q20');",
    )
    .unwrap();
    drop(c);
    let target = temp.path().join("target.sqlite");
    std::fs::copy(&base, &target).unwrap();
    ingest_wikidata_authorities(&MetadataService::open(base).unwrap(), &input, &target).unwrap();
    let subjects = temp.path().join("subjects.sqlite");
    let rich = temp.path().join("rich.sqlite");
    let identity = temp.path().join("identity.sqlite");
    split_snapshot(&target, &subjects, &rich, &identity).unwrap();
    std::fs::remove_file(input).unwrap();
    for service in [MetadataService::open(&target).unwrap(), MetadataService::open_with_stores(subjects, rich, identity, None::<PathBuf>).unwrap()] {
        let response = service.enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap();
        assert_eq!(response.results[0].matches[0].description.as_deref(), Some("A book about logic"));
        let query = |title: &str, author: &str| EditionIdentityQuery { query_id: "test".into(), title: title.into(), authors: vec![author.into()], publishers: vec!["Example Press".into()], book_year: Some(2001) };
        let response = service.resolve_editions(EditionIdentityRequest { queries: vec![query("Logic: A careful introduction", "Jane Smith")] }).unwrap();
        let matched = response.results[0].matched.as_ref().unwrap();
        assert_eq!(matched.subtitle.as_deref(), Some("A careful introduction"));
        assert!(matched.matched_publisher && matched.matched_book_year);
        let q = query("Ancient letters: Collected correspondence", "Jane Smith");
        let response = service.resolve_editions(EditionIdentityRequest { queries: vec![q.clone()] }).unwrap();
        assert!(response.results[0].matched.is_none());
        assert!(response.results[0].candidates.is_empty());
        let matched = response.results[0].authority_subjects.as_ref().unwrap();
        assert_eq!(matched.record_ids, vec!["Q20"]);
        let metadata = metadata_contract::authority_subjects::classification_record(&q, matched).unwrap();
        assert!(metadata.exact_edition_ids.is_empty());
        assert_eq!(metadata.classifications[0].evidence[0].method, "wikidata_title_author:Q20");
        assert!(service.resolve_editions(EditionIdentityRequest { queries: vec![query("Ancient letters", "Unrelated Person")] }).unwrap().results[0].authority_subjects.is_none());
    }
    let c = Connection::open(target).unwrap();
    assert_eq!(c.query_row("SELECT COUNT(*) FROM edition_isbn WHERE edition_id=-20", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert!(c.query_row("SELECT COUNT(*) FROM title_author_classification WHERE normalized_title='ancient letters'", [], |r| r.get::<_, i64>(0)).unwrap() > 0);
}

#[test]
fn wikidata_isbn_fast_path_defers_unrelated_authors_and_incomplete_bibliography() {
    let temp = tempfile::tempdir().unwrap();
    let base = snapshot_from(temp.path(), &[], &[], &[]);
    let input = temp.path().join("input.sqlite");
    let c = Connection::open(&input).unwrap();
    c.execute_batch(include_str!("../wikidata-author-schema.sql")).unwrap();
    c.execute_batch(
        "INSERT INTO state VALUES('status','complete'),('schema_version','1'),('ol_dump_date','2026-08-01'),('bibliography_status','building');
        INSERT INTO profile VALUES('Q10','Fast book','[]',NULL,NULL,'[]',NULL,NULL),('Q1','Jane Smith','[]',NULL,NULL,'[]',NULL,NULL),('Q2','Other Author','[]',NULL,NULL,'[]',NULL,NULL);
        INSERT INTO isbn VALUES('9780306406157','Q10');
        INSERT INTO credit VALUES('Q10','Q1',0),('Q20','Q2',0);
        INSERT INTO isbn_author VALUES('9780306406157','Q10','Q10','Q1','Q10');
        INSERT INTO classification VALUES('Q10','lcc','BC71','P8360',0,'Q10');",
    )
    .unwrap();
    let target = temp.path().join("target.sqlite");
    std::fs::copy(&base, &target).unwrap();
    let service = MetadataService::open(base).unwrap();
    assert!(ingest_wikidata_authorities(&service, &input, &target).is_err());
    c.execute("INSERT INTO state VALUES('ingestion_scope','isbn')", []).unwrap();
    drop(c);
    ingest_wikidata_authorities(&service, &input, &target).unwrap();
    let response = MetadataService::open(&target).unwrap().enrich_rich(MetadataEnrichmentRequest { isbns: vec!["9780306406157".into()] }).unwrap();
    assert_eq!(response.results[0].matches[0].authors[0].identity_key(), "wikidata:Q1");
    assert_eq!(response.results[0].matches[0].classifications[0].notation, "BC71");
    let c = Connection::open(target).unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM author WHERE author_id=-2", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn edition_identity_supports_legacy_publication_year_with_and_without_subtitles() {
    let temporary = tempfile::tempdir().unwrap();
    let database = snapshot_from(
        temporary.path(),
        &[serde_json::json!({"key":"/books/OL1M", "title":"Example History", "subtitle":"A Short Guide", "publish_date":"2004", "authors":[{"key":"/authors/OL100A"}], "isbn_13":["9780306406157"], "lc_classifications":["DC38"]})],
        &[],
        &[serde_json::json!({"key":"/authors/OL100A", "name":"Alex Smith"})],
    );
    // DDC keeps the edition eligible, while its LCC must come from the
    // title/author supplement. This exercises both year-column reads.
    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "UPDATE edition_classification SET scheme=1,notation='944'; INSERT INTO title_author_classification VALUES('example history','a short guide','alex smith',2004,'',2,'DC38'),('example history','','alex smith',2004,'',2,'DC38');",
        )
        .unwrap();
    for legacy in [false, true] {
        if legacy {
            Connection::open(&database).unwrap().execute_batch("ALTER TABLE edition_bibliography RENAME COLUMN book_year TO publication_year; ALTER TABLE title_author_classification RENAME COLUMN book_year TO publication_year;").unwrap();
        }
        let service = MetadataService::open(&database).unwrap();
        let mut query = edition_query("Example History: A Short Guide", vec![], Some(2004));
        // Lookup and the classification supplement share omitted-suffix matching.
        query.queries[0].authors = vec!["Alex Smith Junior".into()];
        let response = service.resolve_editions(query).unwrap();
        let matched = response.results[0].matched.as_ref().expect("matching title, author and year");
        assert_eq!(matched.book_year, Some(2004));
        assert!(matched.matched_book_year);
        assert!(matched.classifications.iter().any(|c| c.notation == "DC38"));
        assert_eq!(matched.subtitle.as_deref(), Some("A Short Guide"));
    }
    Connection::open(&database).unwrap().execute_batch("DROP TABLE edition_subtitle; DROP TABLE work_subtitle;").unwrap();
    let service = MetadataService::open(&database).unwrap();
    let response = service.resolve_editions(edition_query("Example History", vec![], Some(2004))).unwrap();
    assert!(response.results[0].matched.as_ref().unwrap().matched_book_year);
}

#[test]
fn edition_identity_ranks_matching_authors_not_credit_list_length() {
    let tmp = tempfile::tempdir().unwrap();
    let db = snapshot_from(
        tmp.path(),
        &[
            serde_json::json!({"key":"/books/OL1M","title":"Example History","authors":[{"key":"/authors/OL100A"}],"isbn_13":["9780306406157"],"lc_classifications":["DC38"]}),
            serde_json::json!({"key":"/books/OL2M","title":"Example History","authors":[{"key":"/authors/OL100A"},{"key":"/authors/OL101A"},{"key":"/authors/OL102A"}],"isbn_13":["9780140328721"],"lc_classifications":["DC161"]}),
        ],
        &[],
        &[serde_json::json!({"key":"/authors/OL100A","name":"Alex Smith"}), serde_json::json!({"key":"/authors/OL101A","name":"Casey Roe"}), serde_json::json!({"key":"/authors/OL102A","name":"Narrator Person"})],
    );
    let service = MetadataService::open(&db).unwrap();
    let mut q = edition_query("Example History", vec![], None);
    q.queries[0].authors.push("Casey Roe".into());
    let r = service.resolve_editions(q.clone()).unwrap();
    assert_eq!(r.results[0].matched.as_ref().unwrap().open_library_edition_id, "OL2M");
    q.queries[0].authors.pop();
    assert_eq!(service.resolve_editions(q.clone()).unwrap().results[0].status, EditionIdentityStatus::Ambiguous);
    q.queries[0].authors = vec!["Jordan Smith".into()];
    assert_eq!(service.resolve_editions(q).unwrap().results[0].status, EditionIdentityStatus::NoMatch);
}
