//! Ingest sources directly into the authority database's existing relational tables.
use crate::*;
use metadata_contract::matching::{author_match_keys, bibliographic_match_key, bibliographic_title_match_key};
use rusqlite::{params, OptionalExtension};
use std::path::Path;

pub(crate) fn available(c: &Connection) -> Result<bool, MetadataError> {
    let exists: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='wikidata_ingestion')", [], |r| r.get(0)).map_err(error)?;
    if !exists {
        return Ok(false);
    }
    let status: String = c.query_row("SELECT status FROM wikidata_ingestion WHERE id=1", [], |r| r.get(0)).map_err(error)?;
    if status != "complete" {
        return Err(MetadataError("authority ingestion is incomplete".into()));
    }
    Ok(true)
}
fn qkey(qid: &str) -> Result<i64, MetadataError> {
    qid.strip_prefix('Q').and_then(|q| q.parse::<i64>().ok()).filter(|q| *q > 0).map(|q| -q).ok_or_else(|| MetadataError("invalid Wikidata QID".into()))
}
fn add_column(c: &Connection, table: &str, column: &str, kind: &str) -> Result<(), MetadataError> {
    let has: bool = c.query_row(&format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1)"), [column], |r| r.get(0)).map_err(error)?;
    if !has {
        c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}")).map_err(error)?;
    }
    Ok(())
}
fn schema(c: &Connection) -> Result<(), MetadataError> {
    c.execute_batch("PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS edition_description(edition_id INTEGER PRIMARY KEY,description TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS wikidata_ingestion(id INTEGER PRIMARY KEY CHECK(id=1),signature TEXT NOT NULL,status TEXT NOT NULL,after_isbn TEXT NOT NULL,after_author TEXT NOT NULL DEFAULT '');
    CREATE TABLE IF NOT EXISTS isbn_classification(isbn13 INTEGER NOT NULL,scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),notation TEXT NOT NULL,PRIMARY KEY(isbn13,scheme,notation)) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS isbn_classification_by_code ON isbn_classification(scheme,notation,isbn13);
    CREATE TABLE IF NOT EXISTS title_author_classification(normalized_title TEXT NOT NULL,normalized_subtitle TEXT NOT NULL,normalized_author TEXT NOT NULL,book_year INTEGER NOT NULL,normalized_publisher TEXT NOT NULL,scheme INTEGER NOT NULL CHECK(scheme IN (1,2)),notation TEXT NOT NULL,PRIMARY KEY(normalized_title,normalized_subtitle,normalized_author,book_year,normalized_publisher,scheme,notation)) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS title_author_classification_lookup ON title_author_classification(normalized_title,normalized_subtitle,normalized_author,book_year,normalized_publisher);
    CREATE INDEX IF NOT EXISTS author_identifier_by_external ON author_identifier(authority,external_id,author_id);").map_err(error)?;
    for (column, kind) in [("aliases", "TEXT"), ("birth_year", "INTEGER"), ("death_year", "INTEGER"), ("image", "TEXT"), ("description", "TEXT")] {
        add_column(c, "author", column, kind)?;
    }
    add_column(c, "edition_bibliography", "description", "TEXT")?;
    add_column(c, "wikidata_ingestion", "after_book", "TEXT NOT NULL DEFAULT ''")?;
    Ok(())
}
fn author(c: &Connection, a: &metadata_contract::WikidataAuthorEvidence) -> Result<i64, MetadataError> {
    let accepted = matches!(a.identity_match.as_str(), "authority" | "corroborated");
    let linked = if accepted { a.open_library_author_id.as_deref().and_then(|id| open_library_id(id, 'A')) } else { None };
    let known = c
        .prepare("SELECT DISTINCT author_id FROM author_identifier WHERE authority='wikidata' AND external_id=?1 AND author_id>0 LIMIT 2")
        .map_err(error)?
        .query_map([&a.wikidata_id], |r| r.get::<_, i64>(0))
        .map_err(error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(error)?;
    let id = linked.or_else(|| (known.len() == 1 && a.identity_match != "conflict").then(|| known[0])).unwrap_or(qkey(&a.wikidata_id)?);
    // Uncertain matches retain an independent identity; names never merge rows.
    c.execute("INSERT INTO author(author_id,name) VALUES(?1,?2) ON CONFLICT(author_id) DO UPDATE SET name=COALESCE(NULLIF(author.name,''),excluded.name)", params![id, a.name]).map_err(error)?;
    c.execute("INSERT OR IGNORE INTO author_identifier VALUES(?1,'wikidata',?2)", params![id, a.wikidata_id]).map_err(error)?;
    for i in &a.identifiers {
        if i.authority == "openlibrary" && open_library_id(&i.value, 'A').is_none() {
            continue;
        }
        c.execute("INSERT OR IGNORE INTO author_identifier VALUES(?1,?2,?3)", params![id, i.authority, i.value]).map_err(error)?;
    }
    let aliases: Option<String> = c.query_row("SELECT aliases FROM author WHERE author_id=?1", [id], |r| r.get(0)).map_err(error)?;
    let mut names = aliases.as_deref().and_then(|s| serde_json::from_str::<BTreeSet<String>>(s).ok()).unwrap_or_default();
    names.extend(a.aliases.iter().cloned());
    c.execute(
        "UPDATE author SET aliases=?2,birth_year=COALESCE(birth_year,?3),death_year=COALESCE(death_year,?4),image=COALESCE(image,?5) WHERE author_id=?1",
        params![id, serde_json::to_string(&names).map_err(error)?, a.birth_year, a.death_year, a.image],
    )
    .map_err(error)?;
    Ok(id)
}
fn put_author_profile(c: &Connection, input: &Connection, a: &metadata_contract::WikidataAuthorEvidence) -> Result<i64, MetadataError> {
    let mut profile = a.clone();
    let extra: Option<(String, Option<String>)> = input.query_row("SELECT aliases,description FROM profile WHERE qid=?1", [&a.wikidata_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(error)?;
    if let Some((aliases, _)) = &extra {
        profile.aliases = serde_json::from_str(aliases).map_err(error)?;
    }
    if profile.identity_match == "unlinked" {
        let mut ids = BTreeSet::new();
        for identifier in &profile.identifiers {
            if identifier.authority == "openlibrary" {
                if let Some(id) = open_library_id(&identifier.value, 'A') {
                    ids.insert(id);
                }
            } else {
                let found = c
                    .prepare_cached("SELECT author_id FROM author_identifier WHERE authority=?1 AND external_id=?2 AND author_id>0 LIMIT 16")
                    .map_err(error)?
                    .query_map(params![identifier.authority, identifier.value], |r| r.get::<_, i64>(0))
                    .map_err(error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(error)?;
                ids.extend(found);
            }
        }
        let mut candidates = Vec::new();
        let names = crate::enrichment::load_author_names(c, &ids)?;
        let identifiers = crate::enrichment::load_author_identifiers(c, &ids)?;
        for (id, name) in names {
            candidates.push(AuthorMetadata { open_library_author_id: format!("OL{id}A"), name, identifiers: identifiers.get(&id).cloned().unwrap_or_default(), source: AuthorSource::ExactEdition, position: 0 });
        }
        if !candidates.is_empty() {
            let mut evidence = vec![metadata_contract::WikidataBookEvidence { authors: vec![profile.clone()], ..Default::default() }];
            crate::wikidata_books::reconcile(input, c, &mut candidates, &mut evidence)?;
            profile = evidence.remove(0).authors.remove(0);
        }
    }
    let id = author(c, &profile)?;
    if let Some((_, description)) = extra {
        c.execute("UPDATE author SET description=COALESCE(description,?2) WHERE author_id=?1", params![id, description]).map_err(error)?;
    }
    Ok(id)
}
fn put_code(c: &Connection, isbn: i64, edition: i64, scheme: i64, notation: &str, global: bool) -> Result<(), MetadataError> {
    c.execute("INSERT OR IGNORE INTO edition_classification(edition_id,scheme,notation) VALUES(?1,?2,?3)", params![edition, scheme, notation]).map_err(error)?;
    if global {
        c.execute("INSERT OR IGNORE INTO isbn_classification(isbn13,scheme,notation) VALUES(?1,?2,?3)", params![isbn, scheme, notation]).map_err(error)?;
    }
    let bibliography: Option<(String, Option<i32>)> = c.query_row("SELECT title,book_year FROM edition_bibliography WHERE edition_id=?1", [edition], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(error)?;
    if let Some((title, year)) = bibliography {
        let subtitle: Option<String> = c.query_row("SELECT subtitle FROM edition_subtitle WHERE edition_id=?1", [edition], |r| r.get(0)).optional().map_err(error)?;
        let publishers = c.prepare("SELECT normalized_name FROM edition_publisher WHERE edition_id=?1").map_err(error)?.query_map([edition], |r| r.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        let publishers = if publishers.is_empty() { vec![String::new()] } else { publishers };
        let authors=c.prepare("SELECT p.name FROM author p WHERE p.name IS NOT NULL AND p.author_id IN (SELECT author_id FROM edition_author WHERE edition_id=?1 UNION SELECT a.author_id FROM work_author a JOIN edition e USING(work_id) WHERE e.edition_id=?1 AND NOT EXISTS(SELECT 1 FROM edition_author WHERE edition_id=?1))").map_err(error)?.query_map([edition],|r|r.get::<_,String>(0)).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
        for name in authors {
            for key in author_match_keys(&name) {
                for publisher in &publishers {
                    c.execute(
                        "INSERT OR IGNORE INTO title_author_classification VALUES(?1,?2,?3,?4,?5,?6,?7)",
                        params![bibliographic_title_match_key(&title), bibliographic_title_match_key(subtitle.as_deref().unwrap_or("")), key, year.unwrap_or(0), publisher, scheme, notation],
                    )
                    .map_err(error)?;
                }
            }
        }
    }
    Ok(())
}
fn put_bibliography(c: &Connection, input: &Connection, id: i64, book: &metadata_contract::WikidataBookEvidence) -> Result<(), MetadataError> {
    let has: bool = input.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='bibliography')", [], |r| r.get(0)).map_err(error)?;
    let extra: Option<(String, Option<String>, Option<i32>)> =
        if has { input.query_row("SELECT title,subtitle,book_year FROM bibliography WHERE book=?1", [&book.wikidata_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(error)? } else { None };
    let title = extra.as_ref().map(|r| r.0.as_str()).or(book.title.as_deref());
    if let Some(title) = title {
        c.execute("INSERT INTO edition_bibliography(edition_id,title,normalized_title,book_year,description) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(edition_id) DO UPDATE SET book_year=COALESCE(edition_bibliography.book_year,excluded.book_year),description=COALESCE(NULLIF(trim(edition_bibliography.description),''),excluded.description)",params![id,title,bibliographic_title_match_key(title),extra.as_ref().and_then(|r|r.2),book.description]).map_err(error)?;
        if let Some(subtitle) = extra.as_ref().and_then(|r| r.1.as_ref()) {
            let existing: String = c.query_row("SELECT title FROM edition_bibliography WHERE edition_id=?1", [id], |r| r.get(0)).map_err(error)?;
            if bibliographic_title_match_key(&existing) == bibliographic_title_match_key(title) {
                c.execute("INSERT OR IGNORE INTO edition_subtitle VALUES(?1,?2,?3)", params![id, subtitle, bibliographic_title_match_key(&format!("{existing}: {subtitle}"))]).map_err(error)?;
            }
        }
    }
    if let Some(description) = book.description.as_deref().filter(|d| !d.trim().is_empty()) {
        c.execute("INSERT INTO edition_description VALUES(?1,?2) ON CONFLICT(edition_id) DO UPDATE SET description=COALESCE(NULLIF(trim(edition_description.description),''),excluded.description)", params![id, description])
            .map_err(error)?;
    }
    if has {
        let publishers = input
            .prepare_cached("SELECT name FROM book_publisher WHERE book=?1 AND name IS NOT NULL ORDER BY publisher")
            .map_err(error)?
            .query_map([&book.wikidata_id], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let populated: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM edition_publisher WHERE edition_id=?1)", [id], |r| r.get(0)).map_err(error)?;
        if !populated {
            for (position, name) in publishers.iter().enumerate() {
                c.execute("INSERT INTO edition_publisher VALUES(?1,?2,?3,?4)", params![id, position as i64, name, bibliographic_match_key(name)]).map_err(error)?;
            }
        }
    }
    Ok(())
}
fn write_book(c: &Connection, input: &Connection, isbn: i64, book: &metadata_contract::WikidataBookEvidence, existing: &RichIsbnMetadataResult, merged: &RichIsbnMetadataResult) -> Result<(), MetadataError> {
    let new = existing.matches.is_empty();
    if !new && existing.matches.len() != 1 {
        return Ok(());
    }
    let mut editions = if new { vec![qkey(&book.wikidata_id)?] } else { existing.matches[0].exact_edition_ids.iter().filter_map(|s| crate::openlibrary::authority_edition_id(s)).collect() };
    editions.sort();
    editions.dedup();
    if new {
        let id = editions[0];
        c.execute("INSERT OR IGNORE INTO edition(edition_id,work_id) VALUES(?1,NULL)", [id]).map_err(error)?;
        c.execute("INSERT OR IGNORE INTO edition_isbn(isbn13,edition_id) VALUES(?1,?2)", params![isbn, id]).map_err(error)?;
    }
    for edition in &editions {
        put_bibliography(c, input, *edition, book)?;
    }
    for (position, a) in book.authors.iter().enumerate() {
        let id = put_author_profile(c, input, a)?;
        if new || existing.matches[0].authors.is_empty() {
            for edition in &editions {
                c.execute("INSERT OR IGNORE INTO edition_author(edition_id,position,author_id) VALUES(?1,?2,?3)", params![edition, position as i64, id]).map_err(error)?;
            }
        }
    }
    let codes = crate::wikidata_books::direct_codes(book, &isbn.to_string());
    let accepted = merged.matches.iter().flat_map(|m| &m.classifications).map(|v| (v.scheme, v.notation.as_str())).collect::<BTreeSet<_>>();
    for code in codes {
        let scheme = match code.scheme {
            ClassificationScheme::LibraryOfCongress => LCC_SCHEME,
            ClassificationScheme::DeweyDecimal => 1,
            _ => continue,
        };
        if !new && !accepted.contains(&(code.scheme, code.notation.as_str())) {
            continue;
        }
        if !new {
            let prior=c.prepare("SELECT c.notation FROM edition_classification c JOIN edition_isbn i USING(edition_id) WHERE i.isbn13=?1 AND c.scheme=?2 UNION SELECT c.notation FROM work_classification c JOIN edition e USING(work_id) JOIN edition_isbn i USING(edition_id) WHERE i.isbn13=?1 AND c.scheme=?2").map_err(error)?.query_map(params![isbn,scheme],|r|r.get::<_,String>(0)).map_err(error)?.collect::<Result<Vec<_>,_>>().map_err(error)?;
            let valid = prior.iter().filter(|n| metadata_contract::classification_consensus_notation(code.scheme, n).is_some()).collect::<Vec<_>>();
            if !valid.is_empty() && !valid.iter().any(|n| **n == code.notation) {
                continue;
            }
        }
        for edition in &editions {
            put_code(c, isbn, *edition, scheme, &code.notation, merged.wikidata_books.len() == 1)?;
        }
    }
    Ok(())
}
/// Target is a staging copy. Writes existing relational tables, not a response cache.
pub fn ingest_wikidata_authorities(service: &MetadataService, evidence: &Path, target: &Path) -> Result<(), MetadataError> {
    if service.state.database.canonicalize().map_err(error)? == target.canonicalize().map_err(error)? {
        return Err(MetadataError("ingestion requires a staging authority copy".into()));
    }
    let input = Connection::open_with_flags(evidence, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(error)?;
    let value = |key: &str| input.query_row("SELECT value FROM state WHERE key=?1", [key], |r| r.get::<_, String>(0)).map_err(error);
    if value("status")? != "complete" || value("schema_version")? != "1" || value("ol_dump_date")? != service.state.snapshot.dump_date {
        return Err(MetadataError("incomplete or incompatible Wikidata input".into()));
    }
    let isbn_only = value("ingestion_scope").ok().as_deref() == Some("isbn");
    let bibliography_status: Option<String> = input.query_row("SELECT value FROM state WHERE key='bibliography_status'", [], |r| r.get(0)).optional().map_err(error)?;
    if !isbn_only && bibliography_status.as_deref().is_some_and(|s| s != "complete") {
        return Err(MetadataError("bibliography input is incomplete".into()));
    }
    let signature = serde_json::to_string(&("relational-v3", isbn_only, value("source").unwrap_or_else(|_| evidence.display().to_string()), &service.state.snapshot)).map_err(error)?;
    let mut c = Connection::open(target).map_err(error)?;
    let date: String = c.query_row("SELECT dump_date FROM snapshot WHERE singleton=1", [], |r| r.get(0)).map_err(error)?;
    if date != service.state.snapshot.dump_date {
        return Err(MetadataError("staging snapshot differs from ingestion source".into()));
    }
    schema(&c)?;
    let previous: Option<(String, String)> = c.query_row("SELECT signature,after_isbn FROM wikidata_ingestion WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(error)?;
    let mut after = match previous {
        Some((old, cursor)) if old == signature => cursor,
        Some(_) => return Err(MetadataError("ingestion source changed".into())),
        None => String::new(),
    };
    c.execute("INSERT INTO wikidata_ingestion(id,signature,status,after_isbn) VALUES(1,?1,'building','') ON CONFLICT(id) DO UPDATE SET status='building'", [signature]).map_err(error)?;
    loop {
        let isbns = input.prepare("SELECT DISTINCT isbn13 FROM isbn WHERE isbn13>?1 ORDER BY isbn13 LIMIT 64").map_err(error)?.query_map([&after], |r| r.get::<_, String>(0)).map_err(error)?.collect::<Result<Vec<_>, _>>().map_err(error)?;
        if isbns.is_empty() {
            break;
        }
        let before = service.enrich_rich(MetadataEnrichmentRequest { isbns: isbns.clone() })?;
        let mut merged = before.clone();
        let identities = query_connection(&service.state.identity_pool)?;
        crate::wikidata_books::enrich(&input, &identities, &mut merged)?;
        drop(identities);
        let tx = c.transaction().map_err(error)?;
        for (old, result) in before.results.iter().zip(&merged.results) {
            let isbn = canonical_isbn13(&result.requested_isbn).ok_or_else(|| MetadataError("invalid ingestion ISBN".into()))?;
            if !old.matches.is_empty() && result.wikidata_books.len() != 1 {
                continue;
            }
            for book in &result.wikidata_books {
                write_book(&tx, &input, isbn, book, old, result)?;
            }
        }
        after = isbns.last().unwrap().clone();
        tx.execute("UPDATE wikidata_ingestion SET after_isbn=?1 WHERE id=1", [&after]).map_err(error)?;
        tx.commit().map_err(error)?;
        eprintln!("ingested relational ISBN batch through {after}");
    }
    if !isbn_only && bibliography_status.as_deref() == Some("complete") {
        let mut cursor: String = c.query_row("SELECT after_book FROM wikidata_ingestion WHERE id=1", [], |r| r.get(0)).map_err(error)?;
        loop {
            let books = input
                .prepare("SELECT b.book,b.title,p.description FROM bibliography b LEFT JOIN profile p ON p.qid=b.book WHERE b.book>?1 AND NOT EXISTS(SELECT 1 FROM isbn i WHERE i.book=b.book) ORDER BY b.book LIMIT 64")
                .map_err(error)?
                .query_map([&cursor], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            if books.is_empty() {
                break;
            }
            let tx = c.transaction().map_err(error)?;
            for (qid, title, description) in &books {
                let id = qkey(qid)?;
                tx.execute("INSERT OR IGNORE INTO edition VALUES(?1,NULL)", [id]).map_err(error)?;
                let book = metadata_contract::WikidataBookEvidence { wikidata_id: qid.clone(), title: Some(title.clone()), description: description.clone(), ..Default::default() };
                put_bibliography(&tx, &input, id, &book)?;
                let credits = input
                    .prepare_cached("SELECT author FROM credit WHERE book=?1 UNION SELECT c.author FROM edition_work w JOIN credit c ON c.book=w.work WHERE w.book=?1 AND NOT EXISTS(SELECT 1 FROM credit WHERE book=?1)")
                    .map_err(error)?
                    .query_map([qid], |r| r.get::<_, String>(0))
                    .map_err(error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(error)?;
                for (position, q) in credits.iter().enumerate() {
                    let a = crate::wikidata_books::profile(&input, q, q)?;
                    let author = put_author_profile(&tx, &input, &a)?;
                    tx.execute("INSERT OR IGNORE INTO edition_author VALUES(?1,?2,?3)", params![id, position as i64, author]).map_err(error)?;
                }
                let codes = input
                    .prepare_cached(
                        "SELECT scheme,notation FROM classification WHERE book=?1 AND topic_scope=0 UNION SELECT c.scheme,c.notation FROM edition_work w JOIN classification c ON c.book=w.work WHERE w.book=?1 AND c.topic_scope=0",
                    )
                    .map_err(error)?
                    .query_map([qid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .map_err(error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(error)?;
                for (scheme, notation) in codes {
                    let (number, scheme) = match scheme.as_str() {
                        "lcc" => (LCC_SCHEME, ClassificationScheme::LibraryOfCongress),
                        "ddc" => (1, ClassificationScheme::DeweyDecimal),
                        _ => continue,
                    };
                    if metadata_contract::classification_consensus_notation(scheme, &notation).is_some() {
                        put_code(&tx, 0, id, number, &notation, false)?;
                    }
                }
            }
            cursor = books.last().unwrap().0.clone();
            tx.execute("UPDATE wikidata_ingestion SET after_book=?1", [&cursor]).map_err(error)?;
            tx.commit().map_err(error)?;
            eprintln!("ingested books without ISBN through {cursor}");
        }
    }
    // Retain all credited Wikidata author profiles in the existing author
    // tables, including authors of books without an ISBN. Names alone
    // do not merge these with Open Library people.
    if !isbn_only {
        let mut after_author: String = c.query_row("SELECT after_author FROM wikidata_ingestion WHERE id=1", [], |r| r.get(0)).map_err(error)?;
        loop {
            let authors = input
                .prepare("SELECT DISTINCT author FROM credit WHERE author>?1 ORDER BY author LIMIT 256")
                .map_err(error)?
                .query_map([&after_author], |r| r.get::<_, String>(0))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            if authors.is_empty() {
                break;
            }
            let tx = c.transaction().map_err(error)?;
            for qid in &authors {
                let a = crate::wikidata_books::profile(&input, qid, qid)?;
                put_author_profile(&tx, &input, &a)?;
            }
            after_author = authors.last().unwrap().clone();
            tx.execute("UPDATE wikidata_ingestion SET after_author=?1 WHERE id=1", [&after_author]).map_err(error)?;
            tx.commit().map_err(error)?;
            eprintln!("ingested author batch through {after_author}");
        }
    }
    c.execute("UPDATE snapshot SET imported_at_ms=CAST(strftime('%s','now') AS INTEGER)*1000 WHERE singleton=1", []).map_err(error)?;
    c.execute("UPDATE wikidata_ingestion SET status='complete' WHERE id=1", []).map_err(error)?;
    c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;").map_err(error)?;
    Ok(())
}

/// ISBN-less books contribute subjects without asserting an edition identity.
pub(crate) fn lookup_title_author(c: &Connection, q: &EditionIdentityQuery) -> Result<Option<metadata_contract::AuthoritySubjectMatch>, MetadataError> {
    use metadata_contract::matching::{matching_author_count, title_qualifiers};
    let supported: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='edition_subtitle')", [], |r| r.get(0)).map_err(error)?;
    if !supported {
        return Ok(None);
    }
    let normalized = metadata_contract::matching::NormalizedTitle::new(&q.title);
    if normalized.full.is_empty() || q.authors.is_empty() {
        return Ok(None);
    }
    let mut stmt=c.prepare("SELECT b.edition_id,b.title,s.subtitle FROM edition_bibliography b LEFT JOIN edition_subtitle s USING(edition_id) WHERE b.edition_id IN (SELECT edition_id FROM edition_bibliography WHERE normalized_title=?1 UNION SELECT edition_id FROM edition_subtitle WHERE normalized_title=?1) AND b.edition_id<0 AND NOT EXISTS(SELECT 1 FROM edition_isbn i WHERE i.edition_id=b.edition_id) LIMIT 129").map_err(error)?;
    let mut rows = BTreeMap::new();
    for key in normalized.lookup_keys() {
        for row in stmt.query_map([key], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))).map_err(error)? {
            let (id, title, subtitle) = row.map_err(error)?;
            rows.insert(id, (title, subtitle));
        }
    }
    if rows.len() > 128 {
        return Ok(None);
    }
    let mut result: Option<metadata_contract::AuthoritySubjectMatch> = None;
    let mut collected = BTreeMap::<String, Vec<metadata_contract::ClassificationEvidence>>::new();
    let mut best_author_count = 0;
    for (id, (title, subtitle)) in rows {
        let full = subtitle.as_ref().map(|s| format!("{title}: {s}")).unwrap_or_else(|| title.clone());
        if !normalized.matches(&metadata_contract::matching::NormalizedTitle::new(&full)) && !normalized.matches(&metadata_contract::matching::NormalizedTitle::new(&title)) {
            continue;
        }
        if title_qualifiers(&q.title) != title_qualifiers(&full) {
            continue;
        }
        let authors = c
            .prepare_cached("SELECT a.name FROM edition_author e JOIN author a USING(author_id) WHERE e.edition_id=?1 AND a.name IS NOT NULL ORDER BY e.position")
            .map_err(error)?
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let author_count = matching_author_count(&q.authors, &authors);
        if author_count == 0 || author_count < best_author_count {
            continue;
        }
        let codes = c
            .prepare_cached("SELECT notation FROM edition_classification WHERE edition_id=?1 AND scheme=2")
            .map_err(error)?
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?
            .into_iter()
            .filter_map(|n| metadata_contract::classification_consensus_notation(ClassificationScheme::LibraryOfCongress, &n))
            .collect::<BTreeSet<_>>();
        if codes.is_empty() {
            continue;
        }
        if author_count > best_author_count {
            collected.clear();
            result = None;
            best_author_count = author_count;
        }
        for code in codes {
            collected.entry(code).or_default().push(metadata_contract::ClassificationEvidence {
                method: format!("wikidata_title_author:Q{}", -id),
                isbn13: String::new(),
                open_library_work_id: String::new(),
                open_library_edition_id: String::new(),
            });
        }
        let matched = result.get_or_insert_with(|| metadata_contract::AuthoritySubjectMatch { main_title: title, provider_id: "wikidata".into(), record_ids: vec![], title: full, authors, classifications: vec![] });
        matched.record_ids.push(format!("Q{}", -id));
    }
    if let Some(matched) = &mut result {
        matched.classifications = collected.into_iter().map(|(notation, evidence)| Classification { scheme: ClassificationScheme::LibraryOfCongress, notation, source: ClassificationSource::Work, evidence }).collect();
        if matched.classifications.is_empty() {
            return Ok(None);
        }
    }
    Ok(result)
}
