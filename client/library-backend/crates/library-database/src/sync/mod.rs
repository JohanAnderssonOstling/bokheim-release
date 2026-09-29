//! Replica sync: pull commits, cursors, outbox, sync functions.

use crate::shared_sql::SharedSql;
pub(crate) mod apply;
pub(crate) mod functions;
mod outbox;
pub(crate) mod state;
pub use state::CURSOR_RECOVERY_PENDING_KEY;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod convergence_tests;
#[cfg(test)]
mod creation_tests;
#[cfg(test)]
mod projection_repair_tests;

use crate::DatabaseError;
use include_sqlite_sql::include_sql;

include_sql!("src/sync/sql/schema.sql");
include_sql!("src/sync/sql/triggers.sql");
include_sql!("src/sync/sql/sync_apply.sql");

include_sql!("src/sync/sql/sync_state.sql");

include_sql!("src/sync/sql/sync_outbox.sql");

pub(crate) fn invalid_text_column(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())))
}

pub(crate) fn invalid_integer_column(column: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Integer, Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())))
}

pub(crate) fn nonnegative_u64(value: i64, column: usize, name: &str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|_| invalid_integer_column(column, format!("{name} must not be negative")))
}

pub(crate) fn bounded_u8(value: i64, column: usize, name: &str) -> rusqlite::Result<u8> {
    u8::try_from(value).map_err(|_| invalid_integer_column(column, format!("{name} is outside its storage range")))
}

pub(crate) fn storage_i64(value: u64, name: &str) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{name} exceeds the database integer range")))))
}

pub(crate) fn parse_uuid_column(value: String, column: usize) -> rusqlite::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(&value).map_err(|error| invalid_text_column(column, error.to_string()))
}

pub(crate) fn toc_entry_count(entries: &[library_model::BookTocEntry]) -> usize {
    entries.iter().map(|entry| 1 + toc_entry_count(&entry.children)).sum()
}

fn parse_revision(value: Option<String>) -> Result<Option<sync_common::LibraryRevision>, DatabaseError> {
    value
        .map(|value| {
            let raw = value.parse::<u64>().map_err(|error| DatabaseError::message(format!("stored revision is not readable: {error}")))?;
            sync_common::LibraryRevision::new(raw).map_err(|error| DatabaseError::message(format!("stored revision is not readable: {error}")))
        })
        .transpose()
}

fn read_cursor_revision(conn: &rusqlite::Connection, key: PullCursorKey) -> Result<Option<String>, DatabaseError> {
    // Single-statement read used twice by the commit (pre-transaction fast
    // path and in-transaction recheck). It takes the connection rather than
    // the transaction: it never manages transaction boundaries.
    let mut revision = None;
    match key {
        PullCursorKey::State => conn.shared_cursor_state_select(|row| {
            revision = row.get::<_, Option<String>>(0)?;
            Ok(())
        })?,
        PullCursorKey::Reading => conn.shared_cursor_reading_select(|row| {
            revision = row.get::<_, Option<String>>(0)?;
            Ok(())
        })?,
    };
    Ok(revision)
}

#[derive(Clone, Copy)]
enum PullCursorKey {
    State,
    Reading,
}

fn write_cursor_revision(tx: &rusqlite::Connection, key: PullCursorKey, revision: Option<sync_common::LibraryRevision>) -> Result<(), DatabaseError> {
    // Counterpart to the read above: one statement, no boundary management.
    // It takes the connection rather than the transaction for the same
    // reason; the commit passes its transaction, which dereferences.
    match (key, revision) {
        (PullCursorKey::State, Some(revision)) => {
            tx.cursor_state_upsert(&revision.get().to_string()).map_err(DatabaseError::operation)?;
        }
        (PullCursorKey::State, None) => {
            tx.cursor_state_clear().map_err(DatabaseError::operation)?;
        }
        (PullCursorKey::Reading, Some(revision)) => {
            tx.cursor_reading_upsert(&revision.get().to_string()).map_err(DatabaseError::operation)?;
        }
        (PullCursorKey::Reading, None) => {
            tx.cursor_reading_clear().map_err(DatabaseError::operation)?;
        }
    }
    Ok(())
}

// --- Metadata projection helpers. All pure: no statement here touches
// storage. The commit method below runs every write inline. ---

pub(crate) fn decode_book_metadata(raw: &[u8]) -> Result<book_model::BookMetadata, DatabaseError> {
    let invalid = |error: String| DatabaseError::message(format!("stored book payload is not readable: {error}"));
    if raw.is_empty() {
        return Ok(book_model::BookMetadata::default());
    }
    sync_common::wire::decode(raw, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(|error| invalid(error.to_string()))
}

pub(crate) fn normalized_description(description: &str) -> String {
    let mut normalized = String::with_capacity(description.len().min(65_536));
    for character in description.chars().filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t')) {
        if normalized.len() + character.len_utf8() > 65_536 {
            break;
        }
        normalized.push(character);
    }
    normalized
}

pub(crate) fn identifier_scheme_label(scheme: &book_model::Scheme) -> String {
    use book_model::Scheme::*;
    match scheme {
        Unspecified => "unspecified".to_owned(),
        Isbn => "isbn".to_owned(),
        Asin => "asin".to_owned(),
        Doi => "doi".to_owned(),
        Issn => "issn".to_owned(),
        Oclc => "oclc".to_owned(),
        Lccn => "lccn".to_owned(),
        Uuid => "uuid".to_owned(),
        Calibre => "calibre".to_owned(),
        Google => "google".to_owned(),
        Goodreads => "goodreads".to_owned(),
        Kobo => "kobo".to_owned(),
        Other(value) => format!("other:{value}"),
    }
}

pub(crate) fn identifier_scope_label(scope: book_model::Scope) -> &'static str {
    use book_model::Scope::*;
    match scope {
        Book => "book",
        Edition => "edition",
        Work => "work",
    }
}

pub(crate) fn parse_author_name_opt(author_name: &str) -> Result<Option<book_model::AuthorName>, DatabaseError> {
    if author_name.trim().is_empty() {
        return Ok(None);
    }
    let name = book_model::AuthorName::parse(author_name).map_err(|error| DatabaseError::message(format!("author name is not readable: {error}")))?;
    if name.match_key().as_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(name))
}

pub(crate) fn unix_millis() -> Result<i64, DatabaseError> {
    let millis = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_err(DatabaseError::operation)?.as_millis();
    i64::try_from(millis).map_err(DatabaseError::operation)
}

#[cfg(test)]
mod randomized_convergence_tests;

#[cfg(test)]
mod adversarial_convergence_tests;
