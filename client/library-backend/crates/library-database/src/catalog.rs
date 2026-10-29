//! The catalog of hosted books and external source entries. The host stores the snapshot the signed update
//! manifest names and activates it once per process. A changed snapshot or
//! taxonomy rebuilds its browse projection one record at a time.
//! Browse queries read a persisted SQLite projection. Full records are assembled
//! only for a requested book. Chapters come from the added book's own files.
use crate::DatabaseError;
#[cfg(test)]
use book_model::Contributor;
use book_model::{BookFormat, BookRecord, BookTocEntry};
use content_address::BookId;
use library_model::CardKey;
use rusqlite::Connection;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

mod popularity;

mod storage;
pub use storage::{activate, activate_path, audiobook_candidates, audiobook_url, contains, entry};
pub(crate) use storage::{browse_row, read, read_snapshot};

/// Only identical hosted bytes of the same format supply an import answer.
/// Source-linked entries have catalog identities rather than content hashes.
pub fn import_record(hash: BookId, format: BookFormat) -> Result<Option<BookRecord>, DatabaseError> {
    Ok(entry(CardKey::from(hash))?.filter(|book| book.hash == Some(hash) && book.format() == format).map(|book| book.record))
}

/// The single catalog snapshot layout this application reads.
pub const FORMAT_VERSION: u32 = 8;

/// One catalog book, with what browsing filters, sorts and shows, and the
/// record a library adding it records as its enrichment answer.
pub struct CatalogBook {
    pub hash: Option<BookId>,
    pub key: CardKey,
    pub external_url: Option<String>,
    /// An external audiobook's archive on its source, which adding the
    /// entry downloads into the library.
    pub download_url: Option<String>,
    /// Source media format, independent of hosting or download permissions.
    pub file_format: BookFormat,
    /// Total recording time advertised by the source, when available.
    pub audiobook_duration_ms: Option<u64>,
    /// Acquisition sources retained by the snapshot, including every source
    /// credited for a hosted file shared by several catalogs.
    pub sources: Vec<String>,
    pub title: String,
    pub subtitle: Option<String>,
    pub author: String,
    pub search_title: String,
    pub search_author: String,
    /// Base language codes, as library books store them.
    pub languages: Vec<String>,
    /// The taxonomy concepts its subject codes project to.
    pub concepts: Vec<i64>,
    pub record: BookRecord,
    /// Catalog navigation is empty; chapters are inspected from added files.
    pub toc: Vec<BookTocEntry>,
    /// Source popularity is catalog evidence, separate from the book's metadata.
    pub popularity: Option<CatalogPopularity>,
}

impl CatalogBook {
    /// Public Domain Library editions lead catalog browsing by source quality.
    pub(crate) fn is_preferred_source(&self) -> bool {
        self.sources.iter().any(|source| source == "publicdomainlibrary.org")
    }

    pub(crate) fn source_priority(&self) -> u8 {
        self.sources.iter().map(|source| popularity::source_priority(source)).min().unwrap_or_else(|| popularity::source_priority(""))
    }

    pub fn format(&self) -> BookFormat {
        self.file_format
    }

    /// The current audio downloader accepts ZIPs of MP3 chapters only.
    pub fn audiobook_download_url(&self) -> Option<&str> {
        self.download_url.as_deref().filter(|_| self.file_format == BookFormat::Mp3Folder)
    }
}

pub use metadata_contract::catalog::{CatalogPopularity, CombinedPopularity};

fn read_sources(db: &Connection) -> Result<HashMap<String, Vec<String>>, DatabaseError> {
    read_sources_for(db, None)
}

fn key_bytes(key: Option<&str>) -> Result<Option<Vec<u8>>, DatabaseError> {
    key.map(|key| blake3::Hash::from_hex(key).map(|key| key.as_bytes().to_vec()).map_err(DatabaseError::operation)).transpose()
}

fn read_sources_for(db: &Connection, key: Option<&str>) -> Result<HashMap<String, Vec<String>>, DatabaseError> {
    let mut sources = HashMap::<String, Vec<String>>::new();
    let mut statement = db.prepare("SELECT lower(hex(book.key)),name FROM book_source credit JOIN book ON book.row_id=credit.book_row_id JOIN source ON source.id=credit.source_id WHERE (?1 IS NULL OR book.key=?1)
        UNION SELECT lower(hex(entry_key)),name FROM external_book JOIN source ON source.id=external_book.source_id WHERE (?1 IS NULL OR entry_key=?1)
        UNION SELECT lower(hex(book.key)),source.name FROM source_entry_source credit JOIN source_entry entry USING(source_id,entry_id) JOIN book ON book.row_id=entry.hosted_row_id JOIN source ON source.id=credit.original_source_id WHERE (?1 IS NULL OR book.key=?1)
        UNION SELECT lower(hex(book.key)),source.name FROM source_entry_source credit JOIN source_entry entry USING(source_id,entry_id) JOIN book ON book.row_id=entry.external_row_id JOIN source ON source.id=credit.original_source_id WHERE (?1 IS NULL OR book.key=?1)
        ORDER BY 1,2")?;
    for row in statement.query_map([key_bytes(key)?], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
        let (identity, source) = row?;
        sources.entry(identity).or_default().push(source);
    }
    Ok(sources)
}

/// Some older source records put a comma-separated list of all creators in
/// one author credit alongside their individual, role-specific credits.
/// Resolve a list only when every member is independently named somewhere
/// in the catalog. A surname-first credit therefore remains one person.
/// This is a read projection; the published snapshot remains immutable.
#[cfg(test)]
pub(crate) fn normalize_author_lists(books: &mut [CatalogBook]) {
    use book_model::Contributor;
    use metadata_contract::matching::author_match_keys;
    use std::collections::BTreeMap;

    let mut named = BTreeMap::<String, Contributor>::new();
    for credit in books.iter().flat_map(|book| &book.record.contributors).filter(|credit| !credit.name().contains(',')) {
        for key in author_match_keys(credit.name()) {
            named.entry(key).or_insert_with(|| credit.clone());
        }
    }
    for book in books {
        normalize_author_list(book, &named);
    }
}

#[cfg(test)]
fn normalize_author_list(book: &mut CatalogBook, named: &BTreeMap<String, Contributor>) {
    use book_model::{AUTHOR_MARC_RELATOR_CODE, Contributor};
    use metadata_contract::matching::author_match_keys;
    use std::collections::{BTreeMap, HashSet};
    if !book.record.authors().any(|credit| credit.name().contains(',')) {
        return;
    }
    let mut local = BTreeMap::<String, Contributor>::new();
    for credit in book.record.contributors.iter().filter(|credit| !credit.name().contains(',')) {
        for key in author_match_keys(credit.name()) {
            let existing = local.entry(key).or_insert_with(|| credit.clone());
            if credit.role_code() == AUTHOR_MARC_RELATOR_CODE {
                *existing = credit.clone();
            }
        }
    }
    let mut contributors = Vec::<Contributor>::new();
    let mut changed = false;
    for credit in &book.record.contributors {
        if credit.role_code() != AUTHOR_MARC_RELATOR_CODE || !credit.name().contains(',') {
            contributors.push(credit.clone());
            continue;
        }
        // An independently credited spelling of the whole name wins over
        // an interpretation of its comma as a list separator.
        if author_match_keys(credit.name()).iter().any(|key| named.contains_key(key)) {
            contributors.push(credit.clone());
            continue;
        }
        let resolved = credit
            .name()
            .split(',')
            .map(str::trim)
            .map(|name| {
                let keys = author_match_keys(name);
                if let Some(known) = keys.iter().find_map(|key| local.get(key)) {
                    // A separately recorded role on this book is authoritative.
                    Some(known.clone())
                } else {
                    let known = keys.iter().find_map(|key| named.get(key))?;
                    Contributor::with_id(known.agent_id(), name, AUTHOR_MARC_RELATOR_CODE).ok()
                }
            })
            .collect::<Option<Vec<_>>>();
        match resolved {
            Some(resolved) => {
                changed = true;
                contributors.extend(resolved.into_iter().filter(|credit| credit.role_code() == AUTHOR_MARC_RELATOR_CODE));
            }
            None => contributors.push(credit.clone()),
        }
    }
    if changed {
        let mut seen = HashSet::new();
        contributors.retain(|credit| {
            if credit.role_code() != AUTHOR_MARC_RELATOR_CODE {
                return true;
            }
            let keys = author_match_keys(credit.name());
            let fresh = keys.iter().all(|key| !seen.contains(key));
            seen.extend(keys);
            fresh
        });
        book.record.contributors = contributors;
        book.author = book.record.authors().map(|credit| credit.name()).collect::<Vec<_>>().join(", ");
        book.search_author = book_model::normalize_search_text(&book.author);
    }
}

/// Installs a verified snapshot and its browse tables atomically in the same
/// database. Preparation runs without the active catalog lock; publication
/// holds it only while SQLite copies the prepared pages into the active file.
pub fn store(image: &[u8], path: &Path, expected: (u64, u32)) -> Result<(), DatabaseError> {
    let mut source = Connection::open_in_memory()?;
    source.deserialize_read_exact("main", image, image.len(), true)?;
    if expected.1 != FORMAT_VERSION {
        return Err(DatabaseError::message("unsupported catalog snapshot format"));
    }
    if release(&source)? != expected {
        return Err(DatabaseError::message("catalog release differs from the signed manifest"));
    }
    storage::store_snapshot(&source, path)
}

fn release(db: &Connection) -> rusqlite::Result<(u64, u32)> {
    db.query_row("SELECT release_id, format_version FROM snapshot WHERE singleton=1", [], |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)))
}
