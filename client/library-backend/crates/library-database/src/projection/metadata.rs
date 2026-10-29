//! Complete metadata sets written in bounded SQL batches on every platform.

use super::popularity_rating_count;
use crate::DatabaseError;
use crate::browse::{options::base_language, text::search_title};
use crate::contributors::{ContributorNameMatcher, resolve_credits};
use crate::sync::apply::{project_book, retained_subject_concepts};
use crate::sync::storage_i64;
use book_model::{AUTHOR_MARC_RELATOR_CODE, normalize_search_text};
use content_address::BookId;
use library_replica::{BookAddition, BookDetails};
use rusqlite::{Transaction, limits::Limit, params_from_iter, types::Value};
use std::collections::{BTreeSet, HashMap};

const BATCH_ROWS: usize = 1024;
const BOOK_COLUMNS: &str = "format,added_at,popularity_rating_count,title,subtitle,search_title,author,search_author";

fn batch_rows(tx: &Transaction<'_>, width: usize) -> Result<usize, DatabaseError> {
    let rows = tx.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER)? as usize / width;
    if rows == 0 {
        return Err(DatabaseError::message("SQLite parameter limit cannot fit one metadata row"));
    }
    Ok(rows.min(BATCH_ROWS))
}

fn placeholders(rows: usize, width: usize) -> String {
    let row = format!("({})", vec!["?"; width].join(","));
    vec![row; rows].join(",")
}

/// SQL fragments are fixed at the call sites; all incoming data is bound.
fn write_rows<const N: usize>(tx: &Transaction<'_>, rows: &[[Value; N]], prefix: &str, suffix: &str) -> Result<(), DatabaseError> {
    for chunk in rows.chunks(batch_rows(tx, N)?) {
        let sql = format!("{prefix}{}{suffix}", placeholders(chunk.len(), N));
        tx.execute(&sql, params_from_iter(chunk.iter().flatten()))?;
    }
    Ok(())
}

pub(super) fn project<'a>(tx: &Transaction<'_>, inputs: impl Iterator<Item = (&'a BookId, Option<&'a BookAddition>)>) -> Result<(), DatabaseError> {
    let inputs = inputs.collect::<Vec<_>>();
    if inputs.is_empty() {
        return Ok(());
    }
    // Resolve local row IDs once per group instead of embedding hash lookups
    // in every credit, language and subject write. Missing books stay absent.
    let mut ids = HashMap::<BookId, i64>::new();
    for chunk in inputs.chunks(batch_rows(tx, 1)?) {
        let sql = format!("SELECT content_hash,row_id FROM book WHERE content_hash IN ({})", vec!["?"; chunk.len()].join(","));
        let mut statement = tx.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(chunk.iter().map(|(hash, _)| *hash)), |row| Ok((row.get(0)?, row.get(1)?)))?;
        for row in rows {
            let (hash, id) = row?;
            ids.insert(hash, id);
        }
    }
    let mut books = Vec::new();
    let mut credit_counts = Vec::new();
    let mut credits = Vec::new();
    let mut languages = Vec::new();
    let mut subjects = Vec::new();
    let mut concepts = Vec::new();
    let mut evidence = Vec::new();
    for (hash, addition) in inputs {
        let Some(&id) = ids.get(hash) else { continue };
        let empty = BookDetails::default();
        let metadata = addition.map_or(&empty, |addition| &addition.record.metadata);
        let projected = project_book(&metadata.book)?;
        let resolved = resolve_credits(tx, &metadata.contributors, Some(ContributorNameMatcher::Normalized))?;
        let author = resolved.iter().filter(|(_, credit)| credit.role_code() == AUTHOR_MARC_RELATOR_CODE).map(|(_, credit)| credit.name()).collect::<Vec<_>>().join(", ");
        let title = addition.map(|_| metadata.title.clone());
        let subtitle = metadata.subtitle.clone();
        let search = search_title(title.as_deref(), subtitle.as_deref());
        let added_at = addition.map(|addition| storage_i64(addition.facts.added_at, "book addition timestamp")).transpose()?;
        books.push([
            id.into(),
            addition.map(|addition| addition.facts.format as i64).into(),
            added_at.into(),
            addition.and_then(popularity_rating_count).into(),
            title.into(),
            subtitle.into(),
            search.into(),
            author.clone().into(),
            normalize_search_text(&author).into(),
        ]);
        credit_counts.push([id.into(), (resolved.len() as i64).into()]);
        for (position, (identity, credit)) in resolved.into_iter().enumerate() {
            credits.push([id.into(), identity.into(), (position as i64).into(), credit.name().to_owned().into(), credit.role_code().0.to_vec().into()]);
        }
        // Languages can collapse to the same base code (e.g. en-US and en-GB).
        for language in projected.metadata.languages.iter().map(|language| base_language(language.as_str())).filter(|language| !language.is_empty()).collect::<BTreeSet<_>>() {
            languages.push([id.into(), language.into()]);
        }
        // Retained identities must be read before any old evidence is cleared.
        let assignments = retained_subject_concepts(tx, id, &projected)?;
        for (position, subject) in projected.metadata.subjects.iter().enumerate() {
            subjects.push([id.into(), (position as i64).into(), subject.name().to_owned().into(), subject.source().to_owned().into(), subject.authority().map(str::to_owned).into(), subject.code().map(str::to_owned).into()]);
        }
        for concept in assignments.iter().map(|(_, concept)| *concept).collect::<BTreeSet<_>>() {
            concepts.push([id.into(), concept.into(), subject_projection::UNIFIED_MATCHER_VERSION.into()]);
        }
        for (index, concept) in assignments {
            let code = &projected.subject_codes[index];
            for position in code.evidence_positions() {
                evidence.push([id.into(), concept.into(), (*position as i64).into(), code.system_id().to_owned().into(), code.code().to_owned().into()]);
            }
        }
    }
    // Combining the scalar updates avoids rewriting the book row and firing
    // the same browse invalidations several times. Unchanged rows are skipped.
    let set = BOOK_COLUMNS.split(',').map(|column| format!("{column}=incoming.{column}")).collect::<Vec<_>>().join(",");
    let incoming = BOOK_COLUMNS.split(',').map(|column| format!("incoming.{column}")).collect::<Vec<_>>().join(",");
    let previous = BOOK_COLUMNS.split(',').map(|column| format!("book.{column}")).collect::<Vec<_>>().join(",");
    write_rows(tx, &books, &format!("WITH incoming(row_id,{BOOK_COLUMNS}) AS (VALUES "), &format!(") UPDATE book SET {set} FROM incoming WHERE book.row_id=incoming.row_id AND ({previous}) IS NOT ({incoming})"))?;
    write_rows(
        tx,
        &credit_counts,
        "WITH incoming(book_row_id,credit_count) AS (VALUES ",
        ") DELETE FROM book_contributor WHERE (book_row_id,position) IN (SELECT c.book_row_id,c.position FROM incoming i JOIN book_contributor c ON c.book_row_id=i.book_row_id WHERE c.position>=i.credit_count)",
    )?;
    // Upsert credits rather than deleting unchanged credits on rebuilds.
    write_rows(
        tx,
        &credits,
        "INSERT INTO book_contributor(book_row_id,author_identity_id,position,name,role) VALUES ",
        " ON CONFLICT(book_row_id,position) DO UPDATE SET author_identity_id=excluded.author_identity_id,name=excluded.name,role=excluded.role WHERE (book_contributor.author_identity_id,book_contributor.name,book_contributor.role) IS NOT (excluded.author_identity_id,excluded.name,excluded.role)",
    )?;
    let affected = books.iter().map(|row| [row[0].clone()]).collect::<Vec<_>>();
    for table in ["book_language", "book_subject", "book_unified_concept"] {
        write_rows(tx, &affected, &format!("DELETE FROM {table} WHERE book_row_id IN (VALUES "), ")")?;
    }
    write_rows(tx, &languages, "INSERT INTO book_language(book_row_id,language) VALUES ", " ON CONFLICT DO NOTHING")?;
    write_rows(tx, &subjects, "INSERT INTO book_subject(book_row_id,position,name,source,authority,code) VALUES ", "")?;
    write_rows(tx, &concepts, "INSERT INTO book_unified_concept(book_row_id,concept_id,mapper_version) VALUES ", "")?;
    write_rows(tx, &evidence, "INSERT INTO book_unified_concept_evidence(book_row_id,concept_id,subject_position,source_system_id,source_code) VALUES ", " ON CONFLICT DO NOTHING")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use book_model::{BookFormat, BookRecord, BookSubject, Contributor, LanguageTag, MarcRelatorCode};
    use library_replica::BookFacts;
    use rusqlite::OptionalExtension;

    #[test]
    fn metadata_batches_replace_complete_sets_preserve_credit_roles_and_roll_back() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let hashes = (1..=25).map(|index| BookId::new(&format!("{index:064x}"))).collect::<Vec<_>>();
        for hash in &hashes {
            db.connection.execute("INSERT INTO book(content_hash) VALUES(?1)", [hash]).unwrap();
        }
        let author = Contributor::new("First Author", AUTHOR_MARC_RELATOR_CODE).unwrap();
        let editor = Contributor::new("An Editor", MarcRelatorCode(*b"edt")).unwrap();
        let mut addition = BookAddition {
            facts: BookFacts { added_at: 42, format: BookFormat::Epub },
            record: BookRecord {
                metadata: BookDetails {
                    title: "A title".into(),
                    subtitle: Some("A subtitle".into()),
                    contributors: vec![author.clone(), editor],
                    book: book_model::BookMetadata {
                        languages: vec![LanguageTag::parse("en-US").unwrap(), LanguageTag::parse("en-GB").unwrap()],
                        subjects: vec![BookSubject::new(None, "A subject", "file", None, None).unwrap()],
                        ..Default::default()
                    },
                },
                description: String::new(),
            },
            navigation: None,
            pdf: None,
            recording: None,
            enrichment: None,
        };
        db.connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 18).unwrap();
        db.write(|tx| project(tx, hashes.iter().map(|hash| (hash, Some(&addition))))).unwrap();
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_contributor", [], |row| row.get::<_, i64>(0)).unwrap(), 50);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_language", [], |row| row.get::<_, i64>(0)).unwrap(), 25);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book WHERE author='First Author' AND title='A title' AND added_at=42", [], |row| row.get::<_, i64>(0)).unwrap(), 25);
        let identities: i64 = db.connection.query_row("SELECT count(*) FROM author_identity", [], |row| row.get(0)).unwrap();
        addition.record.metadata.contributors = vec![author];
        addition.record.metadata.book.languages.clear();
        addition.record.metadata.book.subjects.clear();
        addition.record.metadata.title = "Changed title".into();
        db.connection.execute_batch("CREATE TEMP TRIGGER reject_metadata BEFORE UPDATE OF title ON book WHEN NEW.title='Changed title' BEGIN SELECT RAISE(ABORT,'injected metadata failure'); END;").unwrap();
        assert!(db.write(|tx| project(tx, hashes.iter().map(|hash| (hash, Some(&addition))))).is_err());
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book WHERE title='A title'", [], |row| row.get::<_, i64>(0)).unwrap(), 25);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_contributor", [], |row| row.get::<_, i64>(0)).unwrap(), 50);
        db.connection.execute_batch("DROP TRIGGER reject_metadata").unwrap();
        db.write(|tx| project(tx, hashes.iter().map(|hash| (hash, Some(&addition))))).unwrap();
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_contributor", [], |row| row.get::<_, i64>(0)).unwrap(), 25);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_language", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book_subject", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(db.connection.query_row("SELECT count(*) FROM author_identity", [], |row| row.get::<_, i64>(0)).unwrap(), identities);
        db.write(|tx| project(tx, hashes.iter().map(|hash| (hash, None)))).unwrap();
        assert_eq!(db.connection.query_row("SELECT count(*) FROM book WHERE title IS NULL AND format IS NULL AND author=''", [], |row| row.get::<_, i64>(0)).unwrap(), 25);
        assert!(db.connection.query_row("PRAGMA foreign_key_check", [], |_| Ok(())).optional().unwrap().is_none());
    }

    #[test]
    fn multirow_updates_and_inserts_respect_a_lowered_parameter_limit() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE items(id INTEGER PRIMARY KEY,name TEXT); INSERT INTO items VALUES(1,'old'),(2,'old'),(3,'old');").unwrap();
        db.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 4).unwrap();
        let tx = db.transaction().unwrap();
        let rows = vec![[1.into(), "one".to_owned().into()], [2.into(), "two".to_owned().into()], [3.into(), "three".to_owned().into()]];
        write_rows(&tx, &rows, "WITH incoming(id,name) AS (VALUES ", ") UPDATE items SET name=incoming.name FROM incoming WHERE items.id=incoming.id").unwrap();
        let names = tx.prepare("SELECT name FROM items ORDER BY id").unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(names, ["one", "two", "three"]);
        write_rows(&tx, &[[4.into(), "four".to_owned().into()]], "INSERT INTO items(id,name) VALUES ", "").unwrap();
        assert_eq!(tx.query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0)).unwrap(), 4);
    }
}
