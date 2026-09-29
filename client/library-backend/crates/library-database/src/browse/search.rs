//! Global and membership-scoped title/author search.
use crate::browse::{SearchSql, options::file_type_query_value, read_book_card_row};
use crate::{Database, DatabaseError};
use library_model::{LibraryFileTypeFilter, LibrarySearchResult};
use sync_common::DirId;

enum SearchScope<'a> {
    Library,
    Folder(&'a DirId),
    Subject(&'a str),
}

impl Database {
    pub fn browse_library_search(&self, query: &str, downloaded_only: bool) -> Result<LibrarySearchResult, DatabaseError> {
        self.search_books(query, LibraryFileTypeFilter::All, downloaded_only, SearchScope::Library)
    }

    pub fn browse_library_search_folder(&self, directory_id: &DirId, query: &str, file_type: LibraryFileTypeFilter, downloaded_only: bool) -> Result<LibrarySearchResult, DatabaseError> {
        self.search_books(query, file_type, downloaded_only, SearchScope::Folder(directory_id))
    }

    pub fn browse_library_search_subject(&self, subject_path: Option<&str>, query: &str, file_type: LibraryFileTypeFilter, downloaded_only: bool) -> Result<LibrarySearchResult, DatabaseError> {
        self.search_books(query, file_type, downloaded_only, subject_path.map_or(SearchScope::Library, SearchScope::Subject))
    }

    fn search_books(&self, query: &str, file_type: LibraryFileTypeFilter, downloaded_only: bool, scope: SearchScope<'_>) -> Result<LibrarySearchResult, DatabaseError> {
        let query = book_model::normalize_search_text(query);
        if query.is_empty() {
            return Ok(LibrarySearchResult::default());
        }
        let file_type = file_type_query_value(file_type);
        let mut result = LibrarySearchResult::default();
        let mut append = |row: &rusqlite::Row<'_>| {
            let card = read_book_card_row(row)?;
            if row.get::<_, bool>("title_match")? {
                result.title_matches.push(card);
            } else {
                result.author_matches.push(card);
            }
            Ok(())
        };
        // One statement classifies both match types, so a concurrent title edit
        // cannot duplicate or lose a book between the two result lists.
        match scope {
            SearchScope::Library => self.connection.search_library_cards(&query, downloaded_only, file_type, &mut append)?,
            SearchScope::Folder(directory) => self.with_folder_cache(|| {
                self.connection.search_folder_cards(&directory.to_string(), &query, downloaded_only, file_type, &mut append)?;
                Ok(())
            })?,
            SearchScope::Subject(path) => self.with_subject_cache(|| {
                let route = self.subject_route_id(path)?.unwrap_or(-1);
                self.connection.search_subject_cards(route, &query, downloaded_only, file_type, &mut append)?;
                Ok(())
            })?,
        }
        Ok(result)
    }
}
