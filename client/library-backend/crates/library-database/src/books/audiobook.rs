//! Audiobook playback state and chapter navigation.

use crate::shared_sql::SharedSql;
use crate::{Database, DatabaseError};
use include_sqlite_sql::include_sql;
use library_model::BookTocEntry;

// ---- audiobook ----
// Cached audiobook playback metadata: chapters, duration, and recording narrator.
//
// Reading is one snapshot call; committing freshly inspected metadata is one
// write call with its own transaction. Callers never hold a connection or
// transaction across the inspection work that produces the metadata.

use library_model::AudiobookChapter;

/// Everything needed to open playback: whether the book is an audiobook, its
/// display title/author, and any already-cached chapter/duration/narrator data.
pub struct AudiobookPlaybackSnapshot {
    pub title: String,
    pub author: String,
    pub cached: Option<AudiobookPlaybackMetadata>,
}

pub struct AudiobookPlaybackMetadata {
    pub duration_ms: u64,
    pub chapters: Vec<AudiobookChapter>,
    pub narrator: Option<String>,
}

fn count(entries: &[BookTocEntry]) -> usize {
    entries.iter().map(|entry| 1 + count(&entry.children)).sum()
}

/// A chapter is an interval; a navigation entry is a point. The end is the next
/// chapter's start, and the last chapter runs to the end of the audio.
fn chapters_from_navigation(entries: &[BookTocEntry], duration_ms: u64) -> Vec<AudiobookChapter> {
    let starts = entries.iter().filter_map(|entry| Some((entry.title.as_str(), book_model::audiobook_toc_position(&entry.target)?))).collect::<Vec<_>>();
    starts
        .iter()
        .enumerate()
        .map(|(index, (title, start_ms))| AudiobookChapter { title: book_model::audiobook_chapter_display_title(title, index + 1), start_ms: *start_ms, end_ms: starts.get(index + 1).map_or(duration_ms, |(_, next)| *next) })
        .collect()
}

/// The file's chapters as the navigation every format shares: a title and the
/// offset it opens at, flat, because an M4B chapter list has no nesting.
pub(crate) fn navigation_from_chapters(chapters: &[book_metadata::AudiobookChapterMetadata]) -> Vec<BookTocEntry> {
    chapters.iter().map(|chapter| BookTocEntry { title: chapter.title.clone(), target: book_model::audiobook_toc_target(chapter.start_ms), children: Vec::new() }).collect()
}

impl Database {
    /// Everything a playback session needs to decide whether it must inspect
    /// the file: title/author for display, and cached chapters if present.
    pub fn audiobook_playback_snapshot(&self, hash: &sync_common::ContentHash) -> Result<AudiobookPlaybackSnapshot, DatabaseError> {
        let mut title_author = None;
        self.connection.audiobook_playback_title_author(hash.as_str(), |row| {
            title_author = Some((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        let (title, author) = title_author.unwrap_or_default();
        let mut cached_row = None;
        self.connection.audiobook_cached_metadata(hash.as_str(), |row| {
            cached_row = Some((row.get::<_, i64>(0)?, row.get::<_, String>(1)?));
            Ok(())
        })?;
        let cached = match cached_row {
            Some((duration_ms, toc_json)) => {
                let entries: Vec<BookTocEntry> = serde_json::from_str(&toc_json).map_err(DatabaseError::operation)?;
                let chapters = chapters_from_navigation(&entries, duration_ms.max(0) as u64);
                let mut evidence = None;
                self.connection.audiobook_recording_evidence(hash.as_str(), |row| {
                    evidence = row.get::<_, Option<String>>(0)?;
                    Ok(())
                })?;
                let narrator = evidence.and_then(|json| serde_json::from_str::<metadata_contract::audible::LookupRequest>(&json).ok()).and_then(|request| request.recording.narrators.into_iter().next());
                Some(AudiobookPlaybackMetadata { duration_ms: duration_ms as u64, chapters, narrator })
            }
            None => None,
        };
        Ok(AudiobookPlaybackSnapshot { title, author, cached })
    }

    /// Commits freshly inspected audiobook metadata and returns the resulting
    /// playback facts in one call. Opens and commits its own transaction; the
    /// inspection that produced `metadata` must already be finished.
    pub fn commit_audiobook_metadata(&self, hash: &sync_common::ContentHash, metadata: &book_metadata::AudiobookMetadata) -> Result<AudiobookPlaybackMetadata, DatabaseError> {
        let transaction = self.connection.unchecked_transaction()?;
        let duration_ms = i64::try_from(metadata.duration_ms).map_err(|_| DatabaseError::message("audiobook duration exceeds SQLite integer range"))?;
        transaction.audiobook_upsert_metadata(hash.as_str(), duration_ms)?;
        if !metadata.tracks.is_empty() {
            let tracks_json = serde_json::to_string(&metadata.tracks).map_err(DatabaseError::operation)?;
            transaction.audiobook_upsert_tracks(hash.as_str(), &tracks_json)?;
        }
        // Preserve a database-only chapter plan while the imported evidence is unchanged.
        let mut applied_request = None;
        transaction.audiobook_applied_request(hash.as_str(), |row| {
            applied_request = row.get::<_, Option<String>>(0)?;
            Ok(())
        })?;
        let request = metadata.audible_lookup_request();
        let preserve_chapters = applied_request.and_then(|json| serde_json::from_str::<metadata_contract::audible::LookupRequest>(&json).ok()).is_some_and(|applied| applied == request);
        let navigation = if preserve_chapters {
            None
        } else {
            let entries = navigation_from_chapters(&metadata.chapters);
            let entry_count = i64::try_from(count(&entries)).map_err(|_| DatabaseError::message("table of contents entry count exceeds SQLite integer range"))?;
            let toc_json = serde_json::to_string(&entries).map_err(DatabaseError::operation)?;
            transaction.shared_audiobook_upsert_toc(hash.as_str(), entry_count, &toc_json)?;
            Some(entries)
        };
        let request_json = serde_json::to_string(&request).map_err(DatabaseError::operation)?;
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_err(DatabaseError::operation)?.as_secs() as i64;
        transaction.audiobook_discover_job(hash.as_str(), metadata_contract::audible::POLICY_VERSION, &request_json, now)?;
        crate::sync::apply::commit(transaction)?;
        let entries = match navigation {
            Some(entries) => entries,
            None => navigation_from_chapters(&metadata.chapters),
        };
        Ok(AudiobookPlaybackMetadata { duration_ms: metadata.duration_ms, chapters: chapters_from_navigation(&entries, metadata.duration_ms), narrator: metadata.recording.narrators.first().cloned() })
    }

    pub fn audiobook_tracks(&self, hash: &sync_common::ContentHash) -> Result<Vec<book_model::AudiobookTrack>, DatabaseError> {
        let mut tracks = None;
        self.connection.audiobook_track_index_select(hash.as_str(), |row| {
            tracks = Some(serde_json::from_str::<Vec<book_model::AudiobookTrack>>(&row.get::<_, String>(0)?).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?);
            Ok(())
        })?;
        Ok(tracks.unwrap_or_default())
    }

    /// An audiobook's chapters projected into the shared navigation form. A
    /// book with no stored navigation comes back empty rather than wrong.
    pub fn audiobook_chapters(&self, hash: &sync_common::ContentHash) -> Result<Vec<AudiobookChapter>, DatabaseError> {
        let mut stored: Option<(String, i64)> = None;
        self.connection.audiobook_chapters_select(hash.as_str(), |row| {
            stored = Some((row.get(0)?, row.get(1)?));
            Ok(())
        })?;
        let Some((toc_json, duration_ms)) = stored else { return Ok(Vec::new()) };
        let entries: Vec<BookTocEntry> = serde_json::from_str(&toc_json).map_err(|error| DatabaseError::message(format!("stored table of contents is not readable: {error}")))?;
        Ok(chapters_from_navigation(&entries, duration_ms.max(0) as u64))
    }
}

include_sql!("src/books/sql/audiobook.sql");
