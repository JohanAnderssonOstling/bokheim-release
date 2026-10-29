//! Reading-position register.

use crate::SqlParam;
use crate::books::toc::TocSql;
use crate::sync::apply::registers::{canonical_body, record_local};
use crate::{Database, DatabaseError};
use include_sqlite_sql::include_sql;
use library_replica::{LibraryStateEntry, ReadingPositionState};
use rusqlite::OptionalExtension;
use sync_common::BookId;
use sync_common::{StateKey, wire};

// ---- reading ----
// The one synchronized reading-position register, shared by every format.

use book_model::{BookTocEntry, ChapterReadStatus, ReadProgress, ReadingPosition};

impl Database {
    pub fn chapter_read_status(&self, hash: &BookId) -> Result<ChapterReadStatus, DatabaseError> {
        let Some((bytes, _)) = canonical_body(&self.connection, "chapter_read_status", &StateKey::Book(*hash), "")? else { return Ok(ChapterReadStatus::default()) };
        match wire::decode(&bytes, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
            LibraryStateEntry::ChapterReadStatus { value, .. } => Ok(value),
            _ => Err(DatabaseError::message("invalid chapter read-status register")),
        }
    }

    pub fn reset_chapter_read_status(&self, hash: &BookId) -> Result<bool, DatabaseError> {
        self.reset_chapter_read_status_at(hash, None)
    }

    fn reading_toc(&self, hash: &BookId) -> Result<Vec<BookTocEntry>, DatabaseError> {
        let mut toc = Vec::<u8>::new();
        self.connection.book_toc_select(hash, |row| {
            toc = row.get(0)?;
            Ok(())
        })?;
        if toc.is_empty() { Ok(Vec::new()) } else { crate::stored_document::decode(&toc) }
    }

    pub fn reset_chapter_read_status_at(&self, hash: &BookId, target: Option<&str>) -> Result<bool, DatabaseError> {
        self.write(|transaction| {
            let mut status = self.chapter_read_status(hash)?;
            let previous = status.clone();
            if let Some(target) = target {
                status.reset_chapter(&self.reading_toc(hash)?, target);
            } else {
                status.reset();
            }
            if status == previous {
                return Ok(false);
            }
            record_local(transaction, LibraryStateEntry::ChapterReadStatus { content_hash: *hash, value: status })?;
            Ok(true)
        })
    }

    /// Marks the whole book read — at 100%, every chapter read — or unread: at
    /// 0%, no chapter read. Where the reader left off stays where it is; only a
    /// book never opened is given a place, its start, to be read at. Returns
    /// whether anything changed, so its owner can schedule publication.
    pub fn set_book_read(&self, hash: &BookId, read: bool) -> Result<bool, DatabaseError> {
        let format = self.book_format(hash)?.ok_or_else(|| DatabaseError::message("book is not in the library"))?;
        self.write(|transaction| {
            let previous_chapters = self.chapter_read_status(hash)?;
            let previous_position = self.stored_reading_position(hash)?;
            let previous: f32 = transaction.reading_progress(&(&hash), |row| row.get(0)).optional()?.flatten().unwrap_or(0.0);
            let mut chapters = previous_chapters.clone();
            chapters.mark_all(&self.reading_toc(hash)?, read);
            let position = previous_position.clone().unwrap_or_else(|| ReadingPosition::start(format));
            let progress = if read { 100.0 } else { 0.0 };
            // An unopened book is unread already; it gets no position for it.
            let position_changed = (read || previous_position.is_some()) && (previous_position.is_none() || previous != progress);
            if previous_chapters != chapters {
                record_local(transaction, LibraryStateEntry::ChapterReadStatus { content_hash: *hash, value: chapters.clone() })?;
            }
            if position_changed {
                record_local(transaction, LibraryStateEntry::ReadingPosition { content_hash: *hash, value: ReadingPositionState { location: position, progress } })?;
            }
            Ok(position_changed || previous_chapters != chapters)
        })
    }

    fn stored_reading_position(&self, hash: &BookId) -> Result<Option<ReadingPosition>, DatabaseError> {
        let mut raw = None;
        self.connection.reading_get_position(hash, |row| {
            raw = row.get::<_, Option<String>>(0)?;
            Ok(())
        })?;
        let Some(raw) = raw.filter(|value| value != "0" && !value.is_empty()) else {
            return Ok(None);
        };
        ReadingPosition::parse(&raw).map(Some).map_err(DatabaseError::operation)
    }

    pub fn reading_position(&self, hash: &BookId) -> Result<Option<String>, DatabaseError> {
        Ok(self.stored_reading_position(hash)?.map(|position| position.as_str()))
    }

    pub fn pdf_reading_position(&self, hash: &BookId) -> Result<Option<(u32, f32)>, DatabaseError> {
        Ok(self.stored_reading_position(hash)?.and_then(|position| position.pdf_page_index().map(|page_index| (page_index, position.pdf_page_position().unwrap_or(0.0)))))
    }

    pub fn audiobook_reading_position(&self, hash: &BookId) -> Result<Option<u64>, DatabaseError> {
        Ok(self.stored_reading_position(hash)?.and_then(|position| position.audiobook_position_millis()))
    }

    /// Stores the canonical reading position and, when given, the navigation
    /// entry the reader resolved it to. Both writes happen inline in one
    /// transaction; `entry` travels with the position because that is when it
    /// changes, and is stored locally rather than synchronized. Returns whether
    /// the synchronized position changed, so its owner can schedule publication.
    pub fn update_reading_position(&self, hash: &BookId, position: &str, progress: Option<f32>, entry: Option<&str>) -> Result<bool, DatabaseError> {
        let position = ReadingPosition::parse(position).map_err(DatabaseError::operation)?;
        let progress = progress.map(ReadProgress::new).transpose().map_err(DatabaseError::operation)?;
        let changed = self.write(|transaction| {
            let previous_chapters = self.chapter_read_status(hash)?;
            let previous_position = self.stored_reading_position(hash)?;
            let previous: Option<f32> = transaction.reading_progress(&(&hash), |row| row.get(0)).optional()?.flatten();
            let progress = progress.map(ReadProgress::value).or(previous).unwrap_or(0.0);
            let mut chapters = previous_chapters.clone();
            let entries = self.reading_toc(hash)?;
            chapters.update_position(&entries, &position, entry, progress >= 100.0);
            let changed = previous_position.as_ref() != Some(&position) || previous.unwrap_or(0.0) != progress || previous_chapters != chapters;
            if previous_chapters != chapters {
                record_local(transaction, LibraryStateEntry::ChapterReadStatus { content_hash: *hash, value: chapters })?;
            }
            record_local(transaction, LibraryStateEntry::ReadingPosition { content_hash: *hash, value: ReadingPositionState { location: position, progress } })?;
            if let Some(entry) = entry.map(str::trim).filter(|entry| !entry.is_empty()) {
                transaction.reading_upsert_toc_entry(hash, entry)?;
            }
            Ok(changed)
        })?;
        Ok(changed)
    }
}

include_sql!("src/books/sql/reading.sql");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_fixtures::FixturesSql;

    #[test]
    fn resetting_one_chapter_keeps_siblings_and_clears_parent_completion() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        let hash = BookId::from_bytes([7; 32]);
        db.connection.fixtures_insert_book_7(&hash).unwrap();
        let leaf = |target: &str| BookTocEntry { title: target.into(), target: target.into(), children: vec![] };
        let toc = vec![BookTocEntry { title: "Part".into(), target: "part".into(), children: vec![leaf("one"), leaf("two")] }, leaf("other"), leaf("after")];
        db.connection.navigation_upsert(&hash, 5, &crate::stored_document::encode(&toc).unwrap(), None, None).unwrap();
        for (index, target) in ["one", "two", "other", "after"].into_iter().enumerate() {
            db.update_reading_position(&hash, &format!("epubcfi(/6/{})", (index + 1) * 2), Some(index as f32 * 20.0), Some(target)).unwrap();
        }
        assert!(db.chapter_read_status(&hash).unwrap().completed.contains("part"));
        let position = db.reading_position(&hash).unwrap();
        assert!(db.reset_chapter_read_status_at(&hash, Some("one")).unwrap());
        let status = db.chapter_read_status(&hash).unwrap();
        assert_eq!(status.completed, ["two".into(), "other".into()].into_iter().collect());
        assert_eq!(db.reading_position(&hash).unwrap(), position);
        db.update_reading_position(&hash, position.as_deref().unwrap(), None, Some("after")).unwrap();
        assert_eq!(db.chapter_read_status(&hash).unwrap(), status);
        assert!(db.reset_chapter_read_status_at(&hash, Some("part")).unwrap());
        assert_eq!(db.chapter_read_status(&hash).unwrap().completed, ["other".into()].into_iter().collect());
    }

    #[test]
    fn chapter_read_status_survives_reopen_and_reset_preserves_the_resume_position() {
        let path = std::env::temp_dir().join(format!("chapter-read-{}.sqlite", uuid::Uuid::new_v4()));
        let hash = BookId::from_bytes([7; 32]);
        let toc: Vec<_> = (0..5).map(|index| BookTocEntry { title: format!("Chapter {index}"), target: book_model::audiobook_toc_target(index * 1000), children: vec![] }).collect();
        {
            let db = Database::open(&path).unwrap();
            db.initialize_library().unwrap();
            db.connection.fixtures_insert_book_7(&hash).unwrap();
            db.connection.navigation_upsert(&hash, 5, &crate::stored_document::encode(&toc).unwrap(), Some(5000), None).unwrap();
            db.update_reading_position(&hash, &book_model::audiobook_reading_position(2500), Some(50.0), None).unwrap();
            assert!(db.chapter_read_status(&hash).unwrap().completed.is_empty());
            db.update_reading_position(&hash, &book_model::audiobook_reading_position(4000), Some(80.0), None).unwrap();
            assert_eq!(db.chapter_read_status(&hash).unwrap().completed.into_iter().collect::<Vec<_>>(), [toc[2].target.clone()]);
        }
        {
            let db = Database::open(&path).unwrap();
            assert!(db.chapter_read_status(&hash).unwrap().completed.contains(&toc[2].target));
            let position = db.reading_position(&hash).unwrap();
            assert!(db.reset_chapter_read_status(&hash).unwrap());
            assert_eq!(db.reading_position(&hash).unwrap(), position);
            db.update_reading_position(&hash, position.as_deref().unwrap(), Some(80.0), None).unwrap();
            assert!(db.chapter_read_status(&hash).unwrap().completed.is_empty());
            db.update_reading_position(&hash, &book_model::audiobook_reading_position(5000), Some(100.0), None).unwrap();
            assert_eq!(db.chapter_read_status(&hash).unwrap().completed.into_iter().collect::<Vec<_>>(), [toc[4].target.clone()]);
        }
        std::fs::remove_file(path).unwrap();
    }
}
