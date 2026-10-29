//! Library views: book detail.

use crate::books::toc::{TocSql, entry_title};
use crate::browse::{DetailSql, invalid_integer_column, invalid_text_column};
use crate::{AudiobookSql, Database, DatabaseError, chapters_from_navigation};
use book_model::{AUTHOR_MARC_RELATOR_CODE, AuthorId, NARRATOR_MARC_RELATOR_CODE};
use library_model::{BookDetail, BookDetailAuthor, BookTocEntry};
use library_replica::LibraryStateEntry;
use sync_common::{BookId, wire};

impl Database {
    /// Everything one book's page states, in one snapshot rather than a request
    /// per panel: the card, its folder trail, its credits, its navigation, and
    /// where the reader is inside it.
    pub fn browse_book_detail(&self, content_hash: &BookId) -> Result<BookDetail, DatabaseError> {
        let hash = &content_hash;
        self.with_browse_snapshot(|| {
            let mut found = None;
            self.connection.book_detail_book(hash, |row| {
                let chapter_count: Option<i64> = row.get(1)?;
                let chapter_count = chapter_count.map(usize::try_from).transpose().map_err(|error| invalid_integer_column(1, error.to_string()))?;
                found = Some((row.get::<_, i64>(0)?, chapter_count, row.get::<_, Option<Vec<u8>>>(2)?));
                Ok(())
            })?;
            // A book the library does not hold may be in the catalog.
            let Some((id, audiobook_chapter_count, record)) = found else {
                let detail: Result<BookDetail, DatabaseError> = super::catalog::detail(content_hash)?.ok_or_else(|| rusqlite::Error::QueryReturnedNoRows.into());
                return self.with_catalog_subjects(detail?);
            };
            let year = year_from_record(record)?;
            let book = self.load_cards(&[id])?.pop().ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            let subjects = self.book_subjects(super::subjects::Books::Library, id)?;

            // Empty when the book has no stored blurb.
            let mut description = String::new();
            self.connection.book_detail_description(hash, |row| {
                description = row.get(0)?;
                Ok(())
            })?;

            let mut source_directory_id: Option<String> = None;
            self.connection.book_detail_directory(hash, |row| {
                source_directory_id = row.get(0)?;
                Ok(())
            })?;
            let path = match source_directory_id.as_deref() {
                Some(directory) => self.dir_path_components(&sync_common::DirId::parse_str(directory).map_err(DatabaseError::operation)?)?,
                None => Vec::new(),
            };
            let path = path.into_iter().map(|component| library_model::BrowsePathSegment { id: library_model::BrowseLocation::Folder(component.id), name: component.name }).collect();

            let mut authors = Vec::new();
            self.connection.book_detail_authors(hash, AUTHOR_MARC_RELATOR_CODE.0.to_vec(), |row| {
                let id: String = row.get(0)?;
                authors.push(BookDetailAuthor { id: AuthorId::parse_str(&id).map_err(|error| invalid_text_column(0, error.to_string()))?, name: row.get(1)? });
                Ok(())
            })?;

            // Narrators are the only non-author credits shown in book details.
            let mut credits = Vec::new();
            let narrator = NARRATOR_MARC_RELATOR_CODE.0.to_vec();
            self.connection.book_detail_credits(hash, narrator, |row| {
                let id: String = row.get(0)?;
                let role: Vec<u8> = row.get(2)?;
                credits.push(library_model::BookDetailCredit {
                    id: AuthorId::parse_str(&id).map_err(|error| invalid_text_column(0, error.to_string()))?,
                    name: row.get(1)?,
                    role: String::from_utf8(role).map_err(|error| invalid_text_column(2, error.to_string()))?,
                });
                Ok(())
            })?;

            let mut stored_toc: Option<Vec<u8>> = None;
            self.connection.book_toc_select(hash, |row| {
                stored_toc = row.get(0)?;
                Ok(())
            })?;
            let mut toc = stored_toc.map(|bytes| crate::stored_document::decode::<Vec<BookTocEntry>>(&bytes)).transpose()?.unwrap_or_default();
            for (index, entry) in toc.iter_mut().enumerate() {
                if book_model::audiobook_toc_position(&entry.target).is_some() {
                    entry.title = book_model::audiobook_chapter_display_title(&entry.title, index + 1);
                }
            }

            // An audiobook's position is a time; the current entry is the chapter
            // that contains it. Other formats store the entry the reader resolved.
            let (current_entry, current_target) = if book.format_category == library_model::LibraryFileTypeFilter::Audiobook {
                let mut chapters: Option<(Vec<u8>, i64)> = None;
                self.connection.audiobook_chapters_select(hash, |row| {
                    chapters = Some((row.get(0)?, row.get(1)?));
                    Ok(())
                })?;
                let chapter = match (self.audiobook_reading_position(content_hash)?, chapters) {
                    (Some(position), Some((toc, duration_ms))) => {
                        let entries: Vec<BookTocEntry> = crate::stored_document::decode(&toc)?;
                        chapters_from_navigation(&entries, duration_ms.max(0) as u64).into_iter().find(|chapter| position >= chapter.start_ms && position < chapter.end_ms)
                    }
                    _ => None,
                };
                match chapter {
                    Some(chapter) => (Some(chapter.title), Some(book_model::audiobook_toc_target(chapter.start_ms))),
                    None => (None, None),
                }
            } else {
                let mut target: Option<String> = None;
                self.connection.reading_entry_select(hash, |row| {
                    target = row.get(0)?;
                    Ok(())
                })?;
                match target {
                    Some(target) => (entry_title(&toc, &target), Some(target)),
                    None => (None, None),
                }
            };

            let mut chapter_read_status = self.chapter_read_status(content_hash)?;
            chapter_read_status.update_parents(&toc);
            Ok(BookDetail { book, external_url: None, audiobook_chapter_count, description, subjects, year, source_directory_id, path, authors, credits, current_entry, current_target, toc, chapter_read_status })
        })
    }
}

/// The publication year in a book's addition record: the one the file states
/// wins, and the metadata server's answer stands in where the file has none.
/// None for a book added offline, which has no record until it syncs.
fn year_from_record(record: Option<Vec<u8>>) -> Result<Option<i32>, DatabaseError> {
    let Some(record) = record else { return Ok(None) };
    match wire::decode::<LibraryStateEntry>(&record, wire::MAX_DECODED_REQUEST_BYTES).map_err(DatabaseError::operation)? {
        LibraryStateEntry::BookAdd { value, .. } => Ok(value.record.book.year),
        _ => Err(DatabaseError::message("a book's year requires an immutable book addition")),
    }
}
