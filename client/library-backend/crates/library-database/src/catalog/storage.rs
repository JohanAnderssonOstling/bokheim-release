//! SQLite ownership and the persisted browse projection of a catalog snapshot.
use super::popularity::{read_popularity, read_popularity_for};
use super::{CatalogBook, FORMAT_VERSION, read_sources, read_sources_for, release};
use crate::{DatabaseError, attach_taxonomy, taxonomy_update};
use book_model::{AUTHOR_MARC_RELATOR_CODE, AuthorId, BookDetails, BookFormat, BookMetadata, BookRecord, BookSubject, Contributor, Identifier, LanguageTag, Scheme, Scope, normalize_search_text};
use content_address::BookId;
use library_model::CardKey;
use rusqlite::{
    Connection, Error as SqlError, OptionalExtension, Row,
    backup::{Backup, StepResult},
    functions::FunctionFlags,
    params, params_from_iter,
    types::{Type, Value},
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use url::Url;

struct ActiveCatalog {
    connection: Connection,
    path: Option<PathBuf>,
    generation: u64,
}
static ACTIVE: OnceLock<Mutex<ActiveCatalog>> = OnceLock::new();
const PROJECTION_VERSION: i64 = 16;
const SCHEMA: &str = include_str!("schema.sql");
const COMPACT_SCHEMA: &str = include_str!("compact.sql");
// An inactive catalog exposes empty browse tables without allocating source records.
const EMPTY_AUTHORS: &str = "CREATE TABLE catalog_browse_author(author_id BLOB PRIMARY KEY CHECK(length(author_id)=16),name TEXT NOT NULL) STRICT, WITHOUT ROWID;
CREATE TABLE catalog_browse_author_book(book_row_id INTEGER NOT NULL,author_id BLOB NOT NULL,PRIMARY KEY(book_row_id,author_id)) STRICT, WITHOUT ROWID;";

fn read_metadata(db: &Connection, key: CardKey, year: Option<i32>) -> Result<BookMetadata, DatabaseError> {
    let mut metadata = BookMetadata { year, ..Default::default() };
    let identity: i64 = db.query_row("SELECT row_id FROM book WHERE key=?1", [&key.0[..]], |row| row.get(0))?;
    let identifiers = db
        .prepare_cached("SELECT value,scheme,scheme_label,scope FROM book_identifier WHERE book_row_id=?1 ORDER BY scheme,scheme_label,value,scope")?
        .query_map([&identity], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (value, scheme, label, scope) in identifiers {
        let scheme = match scheme {
            0 => Scheme::Unspecified,
            1 => Scheme::Isbn,
            2 => Scheme::Asin,
            3 => Scheme::Doi,
            4 => Scheme::Issn,
            5 => Scheme::Oclc,
            6 => Scheme::Lccn,
            7 => Scheme::Uuid,
            8 => Scheme::Calibre,
            9 => Scheme::Google,
            10 => Scheme::Goodreads,
            11 => Scheme::Kobo,
            12 => Scheme::Other(label),
            _ => return Err(DatabaseError::message("catalog identifier has an unsupported scheme")),
        };
        let scope = match scope {
            0 => Scope::Book,
            1 => Scope::Edition,
            2 => Scope::Work,
            _ => return Err(DatabaseError::message("catalog identifier has an unsupported scope")),
        };
        metadata.identifiers.push(Identifier::new(value, scheme, scope).map_err(DatabaseError::operation)?);
    }
    let languages = db.prepare_cached("SELECT language FROM book_language WHERE book_row_id=?1 ORDER BY language")?.query_map([&identity], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    metadata.languages = languages.into_iter().map(|language| LanguageTag::parse(language).map_err(DatabaseError::operation)).collect::<Result<Vec<_>, _>>()?;
    metadata.subjects = db
        .prepare_cached("SELECT subject.id,subject.name,sort_name,source.name,authority.name,code FROM book_subject JOIN subject USING(subject_id) JOIN subject_source source ON source.id=subject.source_id LEFT JOIN subject_authority authority ON authority.id=subject.authority_id WHERE book_row_id=?1 ORDER BY subject_id")?
        .query_map([&identity], |row| {
            BookSubject::with_details(row.get(0)?, row.get::<_, String>(1)?, row.get(2)?, row.get::<_, String>(3)?, row.get(4)?, row.get(5)?).map_err(|error| SqlError::FromSqlConversionFailure(1, Type::Text, Box::new(error)))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(metadata)
}

fn decode_record(db: &Connection, key: CardKey, title: &str, subtitle: Option<&str>, description: String, year: Option<i32>, contributors: Vec<Contributor>) -> Result<BookRecord, DatabaseError> {
    let book = read_metadata(db, key, year)?;
    Ok(BookRecord { metadata: BookDetails { title: title.to_owned(), subtitle: subtitle.map(str::to_owned), contributors, book }, description })
}

fn author_from_row(row: &Row<'_>) -> rusqlite::Result<Contributor> {
    let identity: Vec<u8> = row.get(0)?;
    let id = AuthorId::from_slice(&identity).map_err(|error| SqlError::FromSqlConversionFailure(0, Type::Blob, Box::new(error)))?;
    Contributor::with_id(id, row.get::<_, String>(1)?, AUTHOR_MARC_RELATOR_CODE).map_err(|error| SqlError::FromSqlConversionFailure(1, Type::Text, Box::new(error)))
}

fn book_authors(db: &Connection, key: CardKey) -> Result<Vec<Contributor>, DatabaseError> {
    Ok(db
        .prepare_cached("SELECT author.author_id,name FROM book_author credit JOIN author ON author.row_id=credit.author_row_id JOIN book ON book.row_id=credit.book_row_id WHERE book.key=?1 ORDER BY name,author.author_id")?
        .query_map([&key.0[..]], author_from_row)?
        .collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn read<T>(query: impl FnOnce(&Connection) -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
    read_snapshot(|connection, _| query(connection))
}

/// Hold the catalog lock for selection, metadata and card hydration together.
pub(crate) fn read_snapshot<T>(query: impl FnOnce(&Connection, u64) -> Result<T, DatabaseError>) -> Result<T, DatabaseError> {
    match ACTIVE.get() {
        Some(active) => {
            let database = active.lock().map_err(|_| DatabaseError::message("catalog database lock is poisoned"))?;
            let snapshot = database.connection.unchecked_transaction()?;
            let result = query(&snapshot, database.generation)?;
            snapshot.commit()?;
            Ok(result)
        }
        None => {
            let empty = Connection::open_in_memory()?;
            empty.execute_batch(SCHEMA)?;
            empty.execute_batch(EMPTY_AUTHORS)?;
            empty.execute_batch("CREATE VIEW catalog_hosted AS SELECT NULL AS identity,NULL AS title,NULL AS subtitle,NULL AS description,NULL AS year WHERE 0; CREATE VIEW catalog_external AS SELECT NULL AS identity,NULL AS title,NULL AS subtitle,NULL AS description,NULL AS year,NULL AS external_url,NULL AS download_url,NULL AS format WHERE 0")?;
            configure_title_matching(&empty)?;
            query(&empty, 0)
        }
    }
}

fn configure_title_matching(connection: &Connection) -> Result<(), DatabaseError> {
    connection.create_scalar_function("catalog_title_key", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |context| Ok(context.get::<String>(0)?.trim().to_ascii_lowercase()))?;
    connection.create_scalar_function("normalize_search_text", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |context| Ok(normalize_search_text(&context.get::<String>(0)?)))?;
    Ok(())
}

fn activate_connection(connection: Connection, path: Option<PathBuf>) -> Result<(), DatabaseError> {
    configure_title_matching(&connection)?;
    connection.set_prepared_statement_cache_capacity(128);
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "cache_size", -2048)?;
    ACTIVE.set(Mutex::new(ActiveCatalog { connection, path, generation: 0 })).map_err(|_| DatabaseError::message("the catalog can only be activated once per process"))
}

/// Reads and indexes one catalog database; records and navigation have one
/// on-disk copy. Browse tables are rebuilt only for a changed schema or taxonomy.
pub fn activate_path(path: &Path) -> Result<Option<u64>, DatabaseError> {
    if ACTIVE.get().is_some() {
        return Err(DatabaseError::message("the catalog can only be activated once per process"));
    }
    let Some((release, connection)) = prepare_path(path)? else { return Ok(None) };
    activate_connection(connection, Some(path.to_owned()))?;
    Ok(Some(release))
}

fn prepare_path(path: &Path) -> Result<Option<(u64, Connection)>, DatabaseError> {
    let Some(mut connection) = open_existing_catalog(path)? else { return Ok(None) };
    let Ok((release_id, format)) = release(&connection) else { return Ok(None) };
    if format != FORMAT_VERSION {
        return Ok(None);
    }
    connection.pragma_update(None, "cache_size", -2048)?;
    attach_taxonomy(&connection)?;
    let revision = taxonomy_update::revision(&connection)?;
    let cached = connection
        .query_row("SELECT version,release_id,format_version,taxonomy_revision FROM catalog_browse_revision WHERE singleton=1", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, u32>(2)?, row.get::<_, String>(3)?))
        })
        .optional();
    let overridden = std::env::var_os("BOKHEIM_CATALOG_FILE").is_some_and(|local| Path::new(&local) == path);
    if overridden || !matches!(cached, Ok(Some((version, source_release, source_format, ref taxonomy))) if version == PROJECTION_VERSION && source_release == release_id && source_format == format && taxonomy == &revision) {
        build_projection(&mut connection, &revision)?;
    }
    Ok(Some((release_id, connection)))
}

/// Catalog reads may first need rollback recovery. Detach a published hard link
/// before opening it writable, then let SQLite recover before probing its schema.
fn open_existing_catalog(path: &Path) -> Result<Option<Connection>, DatabaseError> {
    #[cfg(unix)]
    {
        if !path.try_exists().map_err(DatabaseError::operation)? {
            return Ok(None);
        }
        make_private(path)?;
    }
    crate::sqlite::open_existing_writable(path).map_err(DatabaseError::message)
}

/// Published snapshots can be hard-linked into the app-data directory. Detach
/// that link before adding local tables so published release bytes stay intact.
#[cfg(unix)]
fn make_private(path: &Path) -> Result<(), DatabaseError> {
    use std::os::unix::fs::MetadataExt;
    if std::fs::metadata(path).map_err(DatabaseError::operation)?.nlink() > 1 {
        let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        std::fs::copy(path, &temporary).map_err(DatabaseError::operation)?;
        std::fs::rename(&temporary, path).map_err(DatabaseError::operation)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_private(_path: &Path) -> Result<(), DatabaseError> {
    Ok(())
}

#[derive(Default)]
struct StableIds {
    books: HashMap<CardKey, i64>,
}

fn previous_ids(db: &Connection) -> Result<StableIds, DatabaseError> {
    let has_projection: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='catalog_browse_book')", [], |row| row.get(0))?;
    if !has_projection {
        return Ok(StableIds::default());
    }
    let mut ids = StableIds::default();
    let mut rows = db.prepare("SELECT key,row_id FROM catalog_browse_book")?;
    for row in rows.query_map([], |row| Ok((CardKey::from(row.get::<_, BookId>(0)?), row.get::<_, i64>(1)?)))? {
        let (key, id) = row?;
        ids.books.insert(key, id);
    }
    Ok(ids)
}

pub(super) fn store_snapshot(source: &Connection, path: &Path) -> Result<(), DatabaseError> {
    let started = web_time::Instant::now();
    // Copy only the stable browse IDs while holding the catalog lock. Source
    // import and taxonomy projection must not block the library actor that
    // answers book-open requests.
    let active_ids = if let Some(active) = ACTIVE.get() {
        let active = active.lock().map_err(|_| DatabaseError::message("catalog database lock is poisoned"))?;
        if active.path.as_deref() == Some(path) { Some((previous_ids(&active.connection)?, active.generation)) } else { None }
    } else {
        None
    };
    let (ids, generation) = match active_ids {
        Some((ids, generation)) => (ids, Some(generation)),
        None => {
            let ids = match open_existing_catalog(path)? {
                Some(connection) => previous_ids(&connection)?,
                None => StableIds::default(),
            };
            (ids, None)
        }
    };
    let mut prepared = Connection::open_in_memory()?;
    prepared.pragma_update(None, "foreign_keys", true)?;
    attach_taxonomy(&prepared)?;
    install_snapshot(&mut prepared, source, ids)?;
    log::debug!(target: "book_startup", "catalog_update phase=prepare_complete elapsed_ms={}", started.elapsed().as_millis());

    if let Some(active) = ACTIVE.get() {
        let mut active = active.lock().map_err(|_| DatabaseError::message("catalog database lock is poisoned"))?;
        if active.path.as_deref() == Some(path) {
            if generation != Some(active.generation) {
                return Err(DatabaseError::message("catalog changed while its update was being prepared"));
            }
            let connection = &mut active.connection;
            connection.pragma_update(None, "query_only", false)?;
            let result = publish_snapshot(&prepared, connection);
            let restore = connection.pragma_update(None, "query_only", true);
            restore?;
            if result.is_ok() {
                active.generation = active.generation.wrapping_add(1);
            }
            return result;
        }
    }
    let mut destination = match open_existing_catalog(path)? {
        Some(connection) => connection,
        None => Connection::open(path)?,
    };
    destination.pragma_update(None, "cache_size", -2048)?;
    publish_snapshot(&prepared, &mut destination)
}

/// SQLite publishes the prepared pages as one transaction into the existing
/// file, so library connections that attached it also see the new snapshot.
/// Replacing pages avoids dropping old parent tables with live foreign-key
/// relationships, which can otherwise perform quadratic constraint work.
fn publish_snapshot(source: &Connection, destination: &mut Connection) -> Result<(), DatabaseError> {
    let started = web_time::Instant::now();
    destination.flush_prepared_statement_cache();
    let backup = Backup::new(source, destination)?;
    match backup.step(-1)? {
        StepResult::Done => {
            log::debug!(target: "book_startup", "catalog_update phase=publish_complete elapsed_ms={}", started.elapsed().as_millis());
            Ok(())
        }
        _ => Err(DatabaseError::message("catalog is busy; its update was not published")),
    }
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('\"', "\"\""))
}

/// Replacing source rows and derived rows is one SQLite transaction. Readers
/// cannot observe a new snapshot with missing or stale browse tables.
fn install_snapshot(destination: &mut Connection, source: &Connection, ids: StableIds) -> Result<(), DatabaseError> {
    let page_size: u32 = source.pragma_query_value(None, "page_size", |row| row.get(0))?;
    let previous_page_size: u32 = destination.pragma_query_value(None, "page_size", |row| row.get(0))?;
    destination.pragma_update(None, "page_size", page_size)?;
    let revision = taxonomy_update::revision(destination)?;
    let schema = source
        .prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 ELSE 3 END")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let tx = destination.transaction()?;
    // Source and projection tables are replaced together. Parent rows may
    // disappear temporarily; validate the restored relationships at commit.
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    let old = tx
        .prepare("SELECT type,name FROM sqlite_schema WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY CASE type WHEN 'view' THEN 0 WHEN 'trigger' THEN 1 WHEN 'index' THEN 2 ELSE 3 END")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (kind, name) in old {
        tx.execute_batch(&format!("DROP {} IF EXISTS {}", kind.to_uppercase(), quote_identifier(&name)))?;
    }
    for (kind, name, sql) in &schema {
        if kind != "table" {
            continue;
        }
        tx.execute_batch(sql)?;
        let columns = source.prepare("SELECT name FROM pragma_table_info(?1)")?.query_map([name], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let columns_sql = columns.iter().map(|name| quote_identifier(name)).collect::<Vec<_>>().join(",");
        let parameters = (1..=columns.len()).map(|index| format!("?{index}")).collect::<Vec<_>>().join(",");
        let mut select = source.prepare(&format!("SELECT {columns_sql} FROM {}", quote_identifier(name)))?;
        let mut insert = tx.prepare(&format!("INSERT INTO {}({columns_sql}) VALUES({parameters})", quote_identifier(name)))?;
        let mut rows = select.query([])?;
        while let Some(row) = rows.next()? {
            let values = (0..columns.len()).map(|index| row.get::<_, Value>(index)).collect::<Result<Vec<_>, _>>()?;
            insert.execute(params_from_iter(values))?;
        }
    }
    for (kind, _, sql) in &schema {
        if kind != "table" {
            tx.execute_batch(sql)?;
        }
    }
    build_projection_in(&tx, &revision, ids)?;
    tx.commit()?;
    if previous_page_size != page_size {
        destination.execute_batch("VACUUM")?;
    }
    Ok(())
}

/// Activation entry point for callers supplying a small SQLite image.
/// Application startup uses activate_path, including in the browser's OPFS VFS.
pub fn activate(image: Vec<u8>) -> Result<(), DatabaseError> {
    let mut connection = Connection::open_in_memory()?;
    connection.deserialize_read_exact("main", image.as_slice(), image.len(), false)?;
    drop(image);
    if release(&connection)?.1 != FORMAT_VERSION {
        return Err(DatabaseError::message("unsupported catalog snapshot format"));
    }
    attach_taxonomy(&connection)?;
    let revision = taxonomy_update::revision(&connection)?;
    build_projection(&mut connection, &revision)?;
    activate_connection(connection, None)
}

fn source_view(db: &Connection) -> Result<(), DatabaseError> {
    db.execute_batch(
        "DROP VIEW IF EXISTS temp.catalog_source; DROP VIEW IF EXISTS catalog_hosted; DROP VIEW IF EXISTS catalog_external;
        CREATE VIEW catalog_hosted AS SELECT key AS identity,title,subtitle,description.text AS description,year
            FROM book JOIN description ON description.row_id=book.description_id WHERE external=0;
        CREATE VIEW catalog_external AS SELECT key AS identity,title,subtitle,description.text AS description,year,entry_url AS external_url,download_url,format
            FROM book JOIN description ON description.row_id=book.description_id WHERE external=1;
        CREATE TEMP VIEW catalog_source AS SELECT identity,title,subtitle,description,year,0 AS external,NULL AS external_url,NULL AS download_url,1 AS format FROM catalog_hosted
            UNION ALL SELECT identity,title,subtitle,description,year,1,external_url,download_url,format FROM catalog_external;",
    )?;
    Ok(())
}

pub(super) fn build_projection(db: &mut Connection, taxonomy_revision: &str) -> Result<(), DatabaseError> {
    let ids = previous_ids(db)?;
    let tx = db.transaction()?;
    build_projection_in(&tx, taxonomy_revision, ids)?;
    tx.commit()?;
    Ok(())
}

fn published_concepts(db: &Connection, taxonomy_revision: &str) -> Result<Option<HashMap<CardKey, Vec<i64>>>, DatabaseError> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='subject_projection')", [], |row| row.get(0))?;
    if !exists {
        return Ok(None);
    }
    let revision: Option<String> = db.query_row("SELECT taxonomy_revision FROM subject_projection WHERE singleton=1", [], |row| row.get(0)).optional()?;
    if revision.as_deref() != Some(taxonomy_revision) {
        return Ok(None);
    }
    let mut concepts = HashMap::<CardKey, Vec<i64>>::new();
    let mut statement = db.prepare("SELECT b.key,c.concept_id FROM book_concept c JOIN book b ON b.row_id=c.book_row_id ORDER BY b.row_id,c.concept_id")?;
    for row in statement.query_map([], |row| Ok((CardKey::from(row.get::<_, BookId>(0)?), row.get(1)?)))? {
        let (key, concept) = row?;
        concepts.entry(key).or_default().push(concept);
    }
    Ok(Some(concepts))
}

#[cfg(test)]
mod published_concept_tests {
    use super::*;

    #[test]
    fn stored_assignments_are_used_only_for_their_taxonomy_and_keep_empty_results() {
        let db = Connection::open_in_memory().unwrap();
        assert!(published_concepts(&db, "current:145").unwrap().is_none());
        db.execute_batch(
            "CREATE TABLE book(row_id INTEGER PRIMARY KEY,key BLOB); CREATE TABLE book_concept(book_row_id INTEGER,concept_id INTEGER); CREATE TABLE subject_projection(singleton INTEGER PRIMARY KEY,taxonomy_revision TEXT);
            INSERT INTO subject_projection VALUES(1,'current:145'); INSERT INTO book VALUES(1,zeroblob(32)); INSERT INTO book_concept VALUES(1,3915); INSERT INTO book_concept VALUES(1,4034);",
        )
        .unwrap();
        assert_eq!(published_concepts(&db, "current:145").unwrap(), Some(HashMap::from([(CardKey([0; 32]), vec![3915, 4034])])));
        assert!(published_concepts(&db, "changed:145").unwrap().is_none());
        db.execute("DELETE FROM book_concept", []).unwrap();
        assert_eq!(published_concepts(&db, "current:145").unwrap(), Some(HashMap::new()));
    }
}

fn build_projection_in(db: &Connection, taxonomy_revision: &str, ids: StableIds) -> Result<(), DatabaseError> {
    if release(db)?.1 != FORMAT_VERSION {
        return Err(DatabaseError::message("unsupported catalog snapshot format"));
    }
    source_view(db)?;
    let mut sources = read_sources(db)?;
    let mut popularity = read_popularity(db)?;
    let mut concepts = published_concepts(db, taxonomy_revision)?;
    let keys = db.prepare("SELECT identity FROM catalog_source ORDER BY identity")?.query_map([], |row| row.get::<_, BookId>(0))?.collect::<Result<Vec<_>, _>>()?;
    for table in ["catalog_browse_author_book", "catalog_browse_author", "catalog_browse_language", "catalog_browse_concept", "catalog_browse_book", "catalog_browse_search", "catalog_browse_author_search", "catalog_browse_revision"] {
        if let Some(kind) = db.query_row("SELECT type FROM sqlite_schema WHERE name=?1", [table], |row| row.get::<_, String>(0)).optional()? {
            db.execute_batch(&format!("DROP {} {table}", kind.to_uppercase()))?;
        }
    }
    // Format-8 snapshots published before audio durations remain readable.
    db.execute_batch("CREATE TABLE IF NOT EXISTS book_audiobook_duration(book_row_id INTEGER PRIMARY KEY REFERENCES book(row_id),duration_ms INTEGER NOT NULL CHECK(duration_ms>0)) STRICT;")?;
    db.execute_batch(COMPACT_SCHEMA)?;
    let mut next_id = ids.books.values().copied().max().map_or(0, |id| id + 1);
    for identity in keys {
        let key = CardKey::from(identity);
        let index = ids.books.get(&key).copied().unwrap_or_else(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        let (title, subtitle, external, external_url, download_url, format, description, year) =
            db.query_row("SELECT title,subtitle,external,external_url,download_url,format,description,year FROM catalog_source WHERE identity=?1", [&key.0[..]], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i32>>(7)?,
                ))
            })?;
        let format = BookFormat::from_code(format).ok_or_else(|| DatabaseError::message("catalog entry has an unsupported file format"))?;
        if external {
            let url = external_url.as_deref().ok_or_else(|| DatabaseError::message("external catalog entry has no URL"))?;
            for url in std::iter::once(url).chain(download_url.as_deref()) {
                let url = Url::parse(url).map_err(DatabaseError::operation)?;
                if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
                    return Err(DatabaseError::message("external catalog URL must use HTTP or HTTPS without credentials"));
                }
            }
        }
        let contributors = book_authors(db, key)?;
        let author = contributors.iter().map(|credit| credit.name()).collect::<Vec<_>>().join(", ");
        let record = decode_record(db, key, &title, subtitle.as_deref(), description, year, contributors)?;
        let text_key = identity.to_string();
        let mut book = CatalogBook {
            hash: (!external).then_some(identity),
            key,
            external_url,
            download_url,
            file_format: format,
            audiobook_duration_ms: None,
            sources: sources.remove(&text_key).unwrap_or_default(),
            title,
            subtitle,
            author,
            search_title: String::new(),
            search_author: String::new(),
            languages: Vec::new(),
            concepts: Vec::new(),
            record,
            toc: Vec::new(),
            popularity: popularity.remove(&text_key),
        };
        book.search_title = crate::browse::text::search_title(Some(&book.title), book.subtitle.as_deref());
        book.search_author = normalize_search_text(&book.author);
        book.concepts = match &mut concepts {
            Some(concepts) => concepts.remove(&key).unwrap_or_default(),
            None => crate::sync::apply::project_book(&book.record.book)?.concepts.into_iter().map(|(_, concept)| concept).collect(),
        };
        book.concepts.sort_unstable();
        book.concepts.dedup();
        let source_id: i64 = db.query_row("SELECT row_id FROM book WHERE key=?1", [&key.0[..]], |row| row.get(0))?;
        db.prepare_cached("INSERT INTO catalog_browse_search(row_id,book_row_id,search_title,search_author,source_priority,popularity_score) VALUES(?1,?2,?3,?4,?5,?6)")?.execute(params![
            index,
            source_id,
            book.search_title,
            book.search_author,
            book.source_priority(),
            book.popularity.as_ref().and_then(|value| value.score())
        ])?;
        for concept in &book.concepts {
            db.execute("INSERT INTO catalog_browse_concept VALUES(?1,?2)", params![index, concept])?;
        }
    }
    let (release, format) = release(db)?;
    db.execute("INSERT INTO catalog_browse_revision VALUES(1,?1,?2,?3,?4)", params![PROJECTION_VERSION, i64::try_from(release).map_err(DatabaseError::operation)?, format, taxonomy_revision])?;
    db.execute_batch("DROP VIEW catalog_source")?;
    Ok(())
}

pub fn contains(key: CardKey, external: Option<bool>) -> Result<bool, DatabaseError> {
    read(|db| Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM catalog_browse_book WHERE key=?1 AND (?2 IS NULL OR external=?2))", params![&key.0[..], external], |row| row.get(0))?))
}

pub fn audiobook_url(key: CardKey) -> Result<Option<String>, DatabaseError> {
    read(|db| {
        Ok(db
            .query_row(
                "SELECT source.download_url FROM catalog_browse_book book JOIN catalog_external source ON source.identity=book.key WHERE book.key=?1 AND book.external=1 AND book.format=?2",
                params![&key.0[..], BookFormat::Mp3Folder as i64],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    })
}

/// Loads one catalog record. Chapters come from the added book's own files.
pub fn entry(key: CardKey) -> Result<Option<CatalogBook>, DatabaseError> {
    read(|db| load_entry(db, key))
}

/// The entry's row in the browse projection, which is what the catalog's
/// subject tree lists books by.
pub(crate) fn browse_row(key: CardKey) -> Result<Option<i64>, DatabaseError> {
    read(|db| Ok(db.query_row("SELECT row_id FROM catalog_browse_book WHERE key=?1", [&key.0[..]], |row| row.get(0)).optional()?))
}

pub(super) fn load_entry(db: &Connection, key: CardKey) -> Result<Option<CatalogBook>, DatabaseError> {
    let found = db
        .query_row("SELECT external,format,search_title,search_author,duration_ms FROM catalog_browse_book WHERE key=?1", [&key.0[..]], |row| {
            let duration = row.get::<_, Option<i64>>(4)?.map(u64::try_from).transpose().map_err(|error| SqlError::FromSqlConversionFailure(4, Type::Integer, Box::new(error)))?;
            Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, duration))
        })
        .optional()?;
    let Some((external, format, search_title, search_author, audiobook_duration_ms)) = found else { return Ok(None) };
    let view = if external { "catalog_external" } else { "catalog_hosted" };
    let (title, subtitle, description, year) = db.query_row(&format!("SELECT title,subtitle,description,year FROM {view} WHERE identity=?1"), [&key.0[..]], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<i32>>(3)?))
    })?;
    let (external_url, download_url) =
        if external { db.query_row("SELECT external_url,download_url FROM catalog_external WHERE identity=?1", [&key.0[..]], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?)))? } else { (None, None) };
    let contributors = book_authors(db, key)?;
    let author = contributors.iter().map(|credit| credit.name()).collect::<Vec<_>>().join(", ");
    let record = decode_record(db, key, &title, subtitle.as_deref(), description, year, contributors)?;
    let concepts = db
        .prepare_cached("SELECT concept_id FROM catalog_browse_concept WHERE book_row_id=(SELECT row_id FROM catalog_browse_book WHERE key=?1) ORDER BY concept_id")?
        .query_map([&key.0[..]], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let identity = key.to_string();
    let mut sources = read_sources_for(db, Some(&identity))?;
    let popularity = read_popularity_for(db, Some(&identity))?.remove(&identity);
    let languages = record.book.languages.iter().map(|language| crate::browse::options::base_language(language.as_str())).filter(|language| !language.is_empty()).collect();
    Ok(Some(CatalogBook {
        hash: (!external).then(|| BookId::from_bytes(key.0)),
        key,
        external_url,
        download_url,
        file_format: BookFormat::from_code(format).ok_or_else(|| DatabaseError::message("catalog entry has an unsupported file format"))?,
        audiobook_duration_ms,
        sources: sources.remove(&identity).unwrap_or_default(),
        title,
        subtitle,
        author,
        search_title,
        search_author,
        languages,
        concepts,
        record,
        toc: Vec::new(),
        popularity,
    }))
}

/// Only candidate recordings of this title need full author/language records.
pub fn audiobook_candidates(title: &str) -> Result<Vec<CatalogBook>, DatabaseError> {
    read(|db| audiobook_candidates_in(db, title))
}

fn audiobook_candidates_in(db: &Connection, title: &str) -> Result<Vec<CatalogBook>, DatabaseError> {
    let keys = db
        .prepare(
            "SELECT book.key FROM catalog_browse_book book JOIN catalog_external source ON source.identity=book.key WHERE book.external=1 AND book.format=?1 AND catalog_title_key(source.title)=catalog_title_key(?2) ORDER BY book.row_id",
        )?
        .query_map(params![BookFormat::Mp3Folder as i64, title], |row| row.get::<_, BookId>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    keys.into_iter().map(|key| load_entry(db, CardKey::from(key))?.ok_or_else(|| DatabaseError::message("catalog entry is missing"))).collect()
}
