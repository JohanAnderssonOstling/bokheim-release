//! Library views: home, authors, detail, counts.

use crate::books::toc::TocSql;
use crate::books::toc::entry_title;
use crate::browse::{AuthorsSql, CountsSql, DetailSql, HomeSql, LibraryAuthorsView, invalid_integer_column, invalid_text_column, read_book_card_row};
use crate::{Database, DatabaseError};
use book_model::{AgentId, AuthorId};
use library_model::{AuthorSort, AuthorSummary, BookCardRow, BookDetail, BookDetailAuthor, LibraryHomeView};
use std::collections::HashMap;
use sync_common::ContentHash;

/// How far an author's shelf carousel can be paged. The shelf is one row of
/// book cards, so a wide window draws a dozen or so at once and this is a few
/// pages past that; the cap exists because the index loads every author's shelf
/// in one query, not because the row runs out of space.
const AUTHOR_SHELF_BOOK_COUNT: i32 = 40;
/// How many subject chips an author's row states. The chips share the heading
/// line with the name, so this is what fits beside it rather than everything an
/// author has been filed under.
const AUTHOR_SUBJECT_COUNT: i32 = 3;

struct SourceLocation {
    source_directory_id: Option<String>,
    path: Vec<library_model::BrowsePathSegment>,
}

impl Database {
    /// Number of visible books with at least one live library placement.
    ///
    /// Retained metadata rows and books that only remain in Trash do not make a
    /// library non-empty.
    pub fn browse_book_count(&self) -> Result<usize, DatabaseError> {
        let mut count = 0_i64;
        self.connection.count_all_books(|row| {
            count = row.get(0)?;
            Ok(())
        })?;
        Ok(usize::try_from(count).map_err(|_| DatabaseError::message("library book count must not be negative"))?)
    }

    /// Shared application projection for a library home screen. Platform hosts
    /// supply only the user's local-download preference.
    /// A negative limit is rejected; zero returns empty shelves.
    pub fn browse_home(&self, limit: i32, downloaded_only: bool) -> Result<LibraryHomeView, DatabaseError> {
        if limit < 0 {
            return Err(DatabaseError::message("browse home limit must not be negative"));
        }
        self.with_browse_snapshot(|| {
            Ok(LibraryHomeView {
                most_progress: self.get_most_progress_cards(limit, downloaded_only, f64::from(library_model::FINISHED_PROGRESS_THRESHOLD))?,
                recently_read: self.get_recently_read_cards(limit, downloaded_only, f64::from(library_model::FINISHED_PROGRESS_THRESHOLD))?,
                recently_added: self.get_recently_added_cards(limit, downloaded_only)?,
            })
        })
    }

    pub fn browse_authors(&self, sort: AuthorSort, downloaded_only: bool) -> Result<LibraryAuthorsView, DatabaseError> {
        self.with_browse_snapshot(|| {
            let mut shelves = self.get_author_book_cards_scoped(downloaded_only, AUTHOR_SHELF_BOOK_COUNT)?;
            let mut subjects = self.get_author_subjects_scoped(downloaded_only, AUTHOR_SUBJECT_COUNT)?;
            let mut authors = self
                .get_authors_scoped(downloaded_only)?
                .into_iter()
                .map(|(id, name, book_count)| {
                    let books = shelves.remove(&id).unwrap_or_default();
                    let subjects = subjects.remove(&id).unwrap_or_default();
                    AuthorSummary::try_new(id, name, book_count, books, subjects).map_err(|error| invalid_text_column(0, error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            sort.sort(&mut authors);
            Ok(LibraryAuthorsView { authors })
        })
    }

    pub fn browse_book_detail(&self, content_hash: ContentHash) -> Result<BookDetail, DatabaseError> {
        self.with_browse_snapshot(|| {
            let book = self.read_detail_card(&content_hash)?;
            let SourceLocation { source_directory_id, path } = self.read_detail_source(&content_hash)?;
            let authors = self.read_detail_authors(&content_hash)?;
            let credits = self.read_detail_credits(&content_hash)?;
            let subject_paths = self.read_detail_subject_paths(&content_hash)?;
            let related = self.read_detail_related(&content_hash, &authors)?;
            let toc = self.browse_book_navigation(&content_hash)?;
            let (current_entry, current_target) = self.browse_current_entry(&content_hash, book.format_category, &toc)?;
            Ok(BookDetail { book, source_directory_id, path, authors, subject_paths, credits, current_entry, current_target, toc, related })
        })
    }

    fn read_detail_card(&self, content_hash: &ContentHash) -> Result<BookCardRow, DatabaseError> {
        let mut book = None;
        self.connection.book_detail_card(content_hash.as_str(), |row| {
            book = Some(read_book_card_row(row)?);
            Ok(())
        })?;
        Ok(book.ok_or(rusqlite::Error::QueryReturnedNoRows)?)
    }

    fn read_detail_source(&self, content_hash: &ContentHash) -> Result<SourceLocation, DatabaseError> {
        let mut source_directory_id: Option<String> = None;
        self.connection.book_detail_directory(content_hash.as_str(), |row| {
            source_directory_id = row.get(0)?;
            Ok(())
        })?;

        let path: Vec<library_model::BrowsePathSegment> = source_directory_id
            .as_deref()
            .map(|directory| {
                let directory = sync_common::DirId::parse_str(directory).map_err(DatabaseError::operation)?;
                self.dir_path_components(&directory)
            })
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|component| library_model::BrowsePathSegment { id: component.id.to_string(), name: component.name.clone() })
            .collect();

        Ok(SourceLocation { source_directory_id, path })
    }

    fn read_detail_authors(&self, content_hash: &ContentHash) -> Result<Vec<BookDetailAuthor>, DatabaseError> {
        let mut authors = Vec::new();
        self.connection.book_detail_authors(content_hash.as_str(), |row| {
            let id: String = row.get(0)?;
            authors.push(BookDetailAuthor { id: AuthorId::parse_str(&id).map_err(|error| invalid_text_column(0, error.to_string()))?, name: row.get(1)? });
            Ok(())
        })?;

        Ok(authors)
    }

    fn read_detail_credits(&self, content_hash: &ContentHash) -> Result<Vec<library_model::BookDetailCredit>, DatabaseError> {
        // The same table the `author_credit` view reads, with the roles that view
        // filters out. Only narrator and translator: a reader picks an audiobook by
        // its narrator and an edition by its translator, and nothing else in the
        // contributor list answers a question a reader is asking.
        let mut credits = Vec::new();
        let narrator = book_model::NARRATOR_MARC_RELATOR_CODE.0.to_vec();
        let translator = book_model::TRANSLATOR_MARC_RELATOR_CODE.0.to_vec();
        self.connection.book_detail_credits(content_hash.as_str(), narrator, translator, |row| {
            let id: String = row.get(0)?;
            let role: Vec<u8> = row.get(2)?;
            credits.push(library_model::BookDetailCredit {
                id: AuthorId::parse_str(&id).map_err(|error| invalid_text_column(0, error.to_string()))?,
                name: row.get(1)?,
                role: String::from_utf8(role).map_err(|error| invalid_text_column(2, error.to_string()))?,
            });
            Ok(())
        })?;

        Ok(credits)
    }

    fn read_detail_subject_paths(&self, content_hash: &ContentHash) -> Result<Vec<String>, DatabaseError> {
        let mut subject_routes = Vec::new();
        self.connection.book_detail_subject_routes(content_hash.as_str(), |row| {
            subject_routes.push(row.get::<_, i64>(0)?);
            Ok(())
        })?;
        let mut subject_paths = Vec::new();
        for route in subject_routes {
            if let Some(path) = self.subject_route_path(route)? {
                subject_paths.push(path);
            }
        }
        subject_paths.sort_by(|left, right| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()).then_with(|| left.cmp(right)));
        subject_paths.dedup();

        Ok(subject_paths)
    }

    fn read_detail_related(&self, content_hash: &ContentHash, authors: &[BookDetailAuthor]) -> Result<Vec<BookCardRow>, DatabaseError> {
        let author_ids = authors.iter().map(|author| author.id.to_string()).collect::<Vec<_>>();
        let mut related = Vec::new();
        if !author_ids.is_empty() {
            let author_ids = serde_json::to_string(&author_ids).map_err(|error| invalid_text_column(0, error.to_string()))?;
            self.connection.related_books(&author_ids, content_hash.as_str(), |row| {
                related.push(read_book_card_row(row)?);
                Ok(())
            })?;
        }
        Ok(related)
    }

    fn get_authors_scoped(&self, downloaded_only: bool) -> Result<Vec<(AuthorId, String, usize)>, DatabaseError> {
        let mut authors = Vec::new();
        self.connection.get_authors(i32::from(downloaded_only), |row| {
            let raw_id: String = row.get(0)?;
            let id = AgentId::parse_str(&raw_id).map_err(|error| invalid_text_column(0, error.to_string()))?;
            let name: String = row.get(1)?;
            let count: i64 = row.get(2)?;
            let count = usize::try_from(count).map_err(|_| invalid_integer_column(2, "author book count must not be negative"))?;
            authors.push((id, name, count));
            Ok(())
        })?;
        Ok(authors)
    }
    /// The books each author's shelf draws, keyed by author, in shelf order.

    fn get_author_book_cards_scoped(&self, downloaded_only: bool, per_author: i32) -> Result<HashMap<AuthorId, Vec<BookCardRow>>, DatabaseError> {
        let mut shelves: HashMap<AuthorId, Vec<BookCardRow>> = HashMap::new();
        self.connection.get_author_book_cards(i32::from(downloaded_only), per_author, |row| {
            let card = read_book_card_row(row)?;
            // Author identity is separate from the shared card columns.
            let raw_id: String = row.get("author_id")?;
            let id = AgentId::parse_str(&raw_id).map_err(|error| invalid_text_column(12, error.to_string()))?;
            shelves.entry(id).or_default().push(card);
            Ok(())
        })?;
        Ok(shelves)
    }
    /// The subject chips each author's row carries, keyed by author, most-credited
    /// first.

    fn get_author_subjects_scoped(&self, downloaded_only: bool, per_author: i32) -> Result<HashMap<AuthorId, Vec<library_model::AuthorSubject>>, DatabaseError> {
        let mut counts: HashMap<AuthorId, Vec<SubjectCount>> = HashMap::new();
        self.connection.get_author_subject_counts(i32::from(downloaded_only), |row| {
            let raw_id: String = row.get(0)?;
            let id = AgentId::parse_str(&raw_id).map_err(|error| invalid_text_column(0, error.to_string()))?;
            counts.entry(id).or_default().push(SubjectCount { label: row.get(1)?, parent_route: row.get(2)?, parent_label: row.get(3)?, books: row.get(4)? });
            Ok(())
        })?;
        Ok(counts.into_iter().map(|(author, counts)| (author, select_author_subjects(counts, per_author))).collect())
    }

    /// The most recently read books: up to `limit` still in progress, then up to
    /// `limit` finished. The two groups arrive in one statement and stay in
    /// reading order within each, so a caller splits them by the same threshold
    /// the query ranked them with.
    fn get_recently_read_cards(&self, limit: i32, downloaded_only: bool, finished_threshold: f64) -> Result<Vec<BookCardRow>, DatabaseError> {
        let mut cards = Vec::new();
        self.connection.get_recently_read_cards(limit, i32::from(downloaded_only), finished_threshold, |row| {
            cards.push(read_book_card_row(row)?);
            Ok(())
        })?;
        Ok(cards)
    }

    fn get_most_progress_cards(&self, limit: i32, downloaded_only: bool, finished_threshold: f64) -> Result<Vec<BookCardRow>, DatabaseError> {
        let mut cards = Vec::new();
        self.connection.get_most_progress_cards(limit, i32::from(downloaded_only), finished_threshold, |row| {
            cards.push(read_book_card_row(row)?);
            Ok(())
        })?;
        Ok(cards)
    }

    fn get_recently_added_cards(&self, limit: i32, downloaded_only: bool) -> Result<Vec<BookCardRow>, DatabaseError> {
        let mut cards = Vec::new();
        self.connection.get_recently_added_cards(limit, i32::from(downloaded_only), |row| {
            cards.push(read_book_card_row(row)?);
            Ok(())
        })?;
        Ok(cards)
    }

    fn browse_current_entry(&self, content_hash: &ContentHash, format: library_model::LibraryFileTypeFilter, toc: &[library_model::BookTocEntry]) -> Result<(Option<String>, Option<String>), DatabaseError> {
        if format == library_model::LibraryFileTypeFilter::Audiobook {
            let Some(position) = self.audiobook_reading_position(content_hash)? else { return Ok((None, None)) };
            let chapter = self.audiobook_chapters(content_hash)?.into_iter().find(|chapter| position >= chapter.start_ms && position < chapter.end_ms);
            return Ok(match chapter {
                Some(chapter) => (Some(chapter.title), Some(book_model::audiobook_toc_target(chapter.start_ms))),
                None => (None, None),
            });
        }
        let mut target: Option<String> = None;
        self.connection.reading_entry_select(content_hash.as_str(), |row| {
            target = row.get(0)?;
            Ok(())
        })?;
        let Some(target) = target else { return Ok((None, None)) };
        Ok((entry_title(toc, &target), Some(target)))
    }

    fn browse_book_navigation(&self, content_hash: &ContentHash) -> Result<Vec<library_model::BookTocEntry>, DatabaseError> {
        let mut stored: Option<String> = None;
        self.connection.book_toc_select(content_hash.as_str(), |row| {
            stored = row.get(0)?;
            Ok(())
        })?;
        let mut entries =
            stored.map(|json| serde_json::from_str::<Vec<library_model::BookTocEntry>>(&json).map_err(|error| DatabaseError::message(format!("stored table of contents is not readable: {error}")))).transpose()?.unwrap_or_default();
        for (index, entry) in entries.iter_mut().enumerate() {
            if book_model::audiobook_toc_position(&entry.target).is_some() {
                entry.title = book_model::audiobook_chapter_display_title(&entry.title, index + 1);
            }
        }
        Ok(entries)
    }
}

/// One distinct-book count per author, label and parent route.
struct SubjectCount {
    label: String,
    parent_route: i64,
    parent_label: String,
    books: i64,
}

fn select_author_subjects(counts: Vec<SubjectCount>, limit: i32) -> Vec<library_model::AuthorSubject> {
    let mut labels: HashMap<String, (i64, SubjectCount)> = HashMap::new();
    for count in counts {
        let entry = labels.entry(count.label.clone()).or_insert_with(|| (0, SubjectCount { label: count.label.clone(), parent_route: count.parent_route, parent_label: count.parent_label.clone(), books: count.books }));
        // Preserve the existing ranking: sum the distinct counts of each parent
        // occurrence, even when the same book contributes to several parents.
        entry.0 += count.books;
        if count.books > entry.1.books || (count.books == entry.1.books && count.parent_route < entry.1.parent_route) {
            entry.1 = count;
        }
    }
    let mut ranked = labels.into_values().map(|(total, count)| (total, book_model::normalize_search_text(&count.label), count)).collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.label.cmp(&b.2.label)));
    ranked.into_iter().take(limit.max(0) as usize).map(|(_, _, count)| library_model::AuthorSubject::new(count.label, Some(count.parent_label))).collect()
}

#[cfg(test)]
mod subject_selection_tests {
    use super::*;

    #[test]
    fn chooses_parent_before_ranking_labels_and_limiting() {
        let count = |label: &str, parent, books| SubjectCount { label: label.into(), parent_route: parent, parent_label: format!("Parent {parent}"), books };
        let counts = vec![count("Zoology", 8, 3), count("Zoology", 2, 3), count("Biology", 1, 5), count("Écology", 3, 5), count("Ecology", 4, 5)];
        let subjects = select_author_subjects(counts, 3);
        assert_eq!(subjects.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["Zoology", "Biology", "Ecology"]);
        assert_eq!(subjects[0].parent.as_deref(), Some("Parent 2"));
        assert!(select_author_subjects(vec![count("A", 0, 1)], 0).is_empty());
    }
}
