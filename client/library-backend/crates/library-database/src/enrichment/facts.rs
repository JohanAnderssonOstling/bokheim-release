//! Enrich local drafts once, then publish a complete immutable book addition.
use crate::sync::apply::registers::{canonical_body, record_local};
use crate::{Database, DatabaseError};
use book_model::BookRecord;
use include_sqlite_sql::include_sql;
use library_replica::{EnrichmentResult, LibraryStateEntry};
use metadata_contract::WorkPopularity;
use metadata_contract::books::{BookEnrichment, BookEnrichmentStatus};
use sync_common::BookId;
use sync_common::StateKey;

pub const FAILURE_RETRY_SECONDS: i64 = 60;

/// What the device stores about a book the metadata server hasn't answered.
#[derive(Clone, Debug)]
pub struct EnrichmentCandidate {
    pub content_hash: BookId,
    pub request_key: String,
    /// `description` is empty: the file's own description always wins.
    pub record: BookRecord,
    pub audiobook: bool,
    pub recording: Option<metadata_contract::audible::LookupRequest>,
    /// Local, never synchronized.
    pub filenames: Vec<String>,
}

/// Immutable draft bytes identify the exact lookup input, including recording evidence.
fn facts_input(connection: &rusqlite::Connection, hash: &BookId) -> Result<Option<Vec<u8>>, DatabaseError> {
    let mut body = None;
    connection.facts_metadata_body(hash, |row| {
        body = Some(row.get(0)?);
        Ok(())
    })?;
    Ok(body)
}

fn request_key(body: &[u8]) -> String {
    book_model::agent_id_from_migration_seed(body).to_string()
}

/// The immutable result shared by every device in this library.
pub fn current_answer(connection: &rusqlite::Connection, hash: &BookId) -> Result<Option<EnrichmentResult>, DatabaseError> {
    let Some((bytes, _)) = canonical_body(connection, "book_add", &StateKey::Book(*hash), "")? else { return Ok(None) };
    match sync_common::wire::decode::<LibraryStateEntry>(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
        LibraryStateEntry::BookAdd { value, .. } => Ok(value.enrichment),
        _ => Err(DatabaseError::message("invalid stored enrichment result")),
    }
}

impl Database {
    /// Books in `scope` whose current facts have no definitive answer.
    /// Successful answers and definite misses do not expire.
    pub fn enrichment_candidates(&self, scope: &[BookId], now: i64) -> Result<Vec<EnrichmentCandidate>, DatabaseError> {
        let scope = serde_json::to_string(scope).map_err(DatabaseError::operation)?;
        let mut rows = Vec::new();
        self.connection.facts_candidates(&scope, |row| {
            rows.push((row.get::<_, BookId>(0)?, row.get::<_, Option<i64>>(1)?, row.get::<_, Option<i64>>(2)?));
            Ok(())
        })?;
        let mut candidates = Vec::new();
        for (hash, format, retry_at) in rows {
            if retry_at.is_some_and(|at| at > now) {
                continue;
            }
            let Some(body) = facts_input(&self.connection, &hash)? else { continue };
            let key = request_key(&body);
            let LibraryStateEntry::BookAdd { value, .. } = sync_common::wire::decode::<LibraryStateEntry>(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? else {
                continue;
            };
            let mut filenames = Vec::new();
            self.connection.facts_filenames(&hash, |row| {
                filenames.push(row.get(0)?);
                Ok(())
            })?;
            let recording = value.recording;
            let audiobook = matches!(format.and_then(book_model::BookFormat::from_code), Some(book_model::BookFormat::M4b | book_model::BookFormat::Mp3Folder));
            let record = BookRecord { metadata: value.record.metadata, description: String::new() };
            candidates.push(EnrichmentCandidate { content_hash: hash, request_key: key, record, audiobook, recording, filenames });
        }
        Ok(candidates)
    }

    /// Stores the server's answer for the facts sent under `request_key`.
    pub fn record_enrichment(&self, hash: &BookId, request_key: &str, result: &BookEnrichment, now: i64) -> Result<(), DatabaseError> {
        self.write(|transaction| {
            if current_answer(transaction, hash)?.is_some() {
                return Ok(());
            }
            let Some(body) = facts_input(transaction, hash)? else { return Ok(()) };
            if request_key != self::request_key(&body) {
                return Ok(()); // The addition changed while this request was in flight.
            }
            let value = match result.status {
                BookEnrichmentStatus::Failed => {
                    transaction.facts_record_failure(hash, now.saturating_add(FAILURE_RETRY_SECONDS))?;
                    return Ok(());
                }
                BookEnrichmentStatus::Found => EnrichmentResult::Found {
                    toc: result.toc.clone(),
                    cover_id: result.cover.as_ref().map(|cover| cover.id.clone()),
                    popularity: result.popularity.clone(),
                },
                BookEnrichmentStatus::NoMatch => EnrichmentResult::NoMatch,
            };
            let LibraryStateEntry::BookAdd { value: mut addition, .. } = sync_common::wire::decode::<LibraryStateEntry>(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? else {
                return Err(DatabaseError::message("invalid local book draft"));
            };
            if result.status == BookEnrichmentStatus::Found {
                let record = result.record.clone().ok_or_else(|| DatabaseError::message("found enrichment has no record"))?;
                addition.apply_enrichment(record, value);
            } else {
                addition.enrichment = Some(value);
            }
            record_local(transaction, LibraryStateEntry::BookAdd { content_hash: *hash, value: addition })?;
            Ok(())
        })
    }

    /// Records a catalog book's record as its answer, so the metadata server
    /// is not asked about it. The catalog already enriched and curated it.
    /// Does nothing once the book has an answer.
    pub fn record_catalog_answer(&self, hash: &BookId, record: BookRecord) -> Result<(), DatabaseError> {
        self.write(|transaction| record_catalog_answer(transaction, hash, record))
    }

    /// Answered books in `scope` still eligible for direct cover lookup.
    pub fn enrichment_cover_candidates(&self, scope: &[BookId]) -> Result<Vec<BookId>, DatabaseError> {
        let scope = serde_json::to_string(scope).map_err(DatabaseError::operation)?;
        let mut hashes = Vec::new();
        self.connection.facts_cover_candidates(&scope, |row| {
            hashes.push(row.get::<_, BookId>(0)?);
            Ok(())
        })?;
        Ok(hashes)
    }

    /// Where the current answer says the book's cover can be fetched.
    pub fn enrichment_cover_source(&self, hash: &BookId) -> Result<Option<metadata_contract::books::CoverSource>, DatabaseError> {
        Ok(match current_answer(&self.connection, hash)? {
            Some(EnrichmentResult::Found { cover_id: Some(id), .. }) => Some(metadata_contract::books::CoverSource { id }),
            _ => None,
        })
    }

    /// The finalized record used by the library projection.
    /// A definitive metadata miss still permits a direct cover-provider lookup.
    pub fn enrichment_cover_record(&self, hash: &BookId) -> Result<Option<BookRecord>, DatabaseError> {
        let Some((bytes, _)) = canonical_body(&self.connection, "book_add", &StateKey::Book(*hash), "")? else { return Ok(None) };
        let LibraryStateEntry::BookAdd { value, .. } = sync_common::wire::decode::<LibraryStateEntry>(&bytes, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? else {
            return Err(DatabaseError::message("invalid stored book addition"));
        };
        let mut record = value.record;
        record.description.clear();
        Ok(Some(record))
    }

    /// Popularity observed when this book's immutable enrichment answer was built.
    pub fn enrichment_popularity(&self, hash: &BookId) -> Result<Option<WorkPopularity>, DatabaseError> {
        Ok(match current_answer(&self.connection, hash)? {
            Some(EnrichmentResult::Found { popularity, .. }) => popularity,
            _ => None,
        })
    }

    /// The earliest retry time after a live book's enrichment request failed.
    pub fn enrichment_next_retry(&self) -> Result<Option<i64>, DatabaseError> {
        Ok(self.connection.facts_next_retry(|row| row.get::<_, Option<i64>>(0))?)
    }

    pub fn mark_enrichment_cover_checked(&self, hash: &BookId) -> Result<(), DatabaseError> {
        self.connection.facts_mark_cover_checked(hash)?;
        Ok(())
    }
}

include_sql!("src/enrichment/sql/facts.sql");

/// Completes an import's catalog answer under the caller's writer transaction.
pub(crate) fn record_catalog_answer(transaction: &rusqlite::Transaction<'_>, hash: &BookId, record: BookRecord) -> Result<(), DatabaseError> {
    if current_answer(transaction, hash)?.is_some() {
        return Ok(());
    }
    let Some(body) = facts_input(transaction, hash)? else { return Ok(()) };
    let LibraryStateEntry::BookAdd { value: mut addition, .. } = sync_common::wire::decode::<LibraryStateEntry>(&body, sync_common::wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? else {
        return Err(DatabaseError::message("invalid local book draft"));
    };
    addition.apply_enrichment(record, EnrichmentResult::Found { toc: None, cover_id: None, popularity: None });
    record_local(transaction, LibraryStateEntry::BookAdd { content_hash: *hash, value: addition })?;
    Ok(())
}
