//! Native library-handle wrappers for shared browse queries.

use super::events::LibraryEvent;
use crate::asset_workflow::AssetPlacementWorkflow;
use crate::library::LibrarySession;
use crate::BackendError;
use crate::{ContentHash, DirId};
use library_database::LibraryBrowseData;
use library_model::{BrowseContents, LibraryHomeView};

impl LibrarySession {
    pub fn book_count(&self) -> Result<usize, BackendError> {
        self.db.browse_book_count().map_err(BackendError::operation)
    }

    /// Everything one book's page states, in one query rather than a request
    /// per panel: the card, its folder trail, its credits, its subjects, its
    /// navigation, and where the reader is inside it.
    pub fn book_detail(&self, content_hash: ContentHash) -> Result<library_model::BookDetail, BackendError> {
        self.db.browse_book_detail(content_hash).map_err(BackendError::operation)
    }

    pub fn home(&self, limit: i32, downloaded_only: bool) -> Result<LibraryHomeView, BackendError> {
        self.db.browse_home(limit, downloaded_only).map_err(BackendError::operation)
    }

    /// The author index: every credited identity with its shelf, already
    /// ordered. One query for the whole page rather than a shelf lookup per
    /// row, so the cost does not scale with the number of authors.
    pub fn authors(&self, sort: library_model::AuthorSort, downloaded_only: bool) -> Result<Vec<library_model::AuthorSummary>, BackendError> {
        self.db.browse_authors(sort, downloaded_only).map(|view| view.authors).map_err(BackendError::operation)
    }

    pub fn subject_contents(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<BrowseContents, BackendError> {
        self.db.browse_subject_contents(query, downloaded_only).map_err(BackendError::operation)
    }

    /// Global title/author search. Unlike the scoped `search` filter on
    /// folder and subject contents, this runs across the whole library and
    /// keeps title matches ahead of distinct author-only matches.
    pub fn library_search(&self, query: &str, downloaded_only: bool) -> Result<library_model::LibrarySearchResult, BackendError> {
        self.db.browse_library_search(query, downloaded_only).map_err(BackendError::operation)
    }

    pub fn directory_path(&self, directory_id: &DirId) -> Result<Vec<library_model::BrowsePathSegment>, BackendError> {
        self.db.browse_directory_path(directory_id).map_err(BackendError::operation)
    }
}

impl LibrarySession {
    pub(crate) fn folder_browse_data(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<LibraryBrowseData, String> {
        self.db.browse_folder(query, downloaded_only).map_err(|error| error.to_string())
    }

    pub(crate) fn subject_browse_data(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<LibraryBrowseData, String> {
        self.db.browse_subject(query, downloaded_only).map_err(|error| error.to_string())
    }

    pub(crate) fn queue_browse_downloads(&self, query: library_model::LibraryBrowseQuery, subject: bool) -> Result<(), String> {
        let hashes = collect_browse_downloads(query, |query| {
            let contents = if subject { self.db.browse_subject_contents(query, false) } else { self.db.browse_folder_contents(query, false) }.map_err(|error| error.to_string())?;
            Ok(contents)
        })?;
        self.request_downloads(&hashes).map_err(|error| error.to_string())
    }

    /// Evicts the downloaded copies under one folder without deleting any
    /// entry. Only books the server also holds are evicted, so a local-only
    /// book keeps its bytes. Returns how many copies were evicted.
    pub(crate) fn evict_folder_downloads(&self, directory_id: &DirId) -> Result<usize, String> {
        let mut query = library_model::LibraryBrowseQuery {
            location: directory_id.to_string(),
            search: String::new(),
            file_types: Vec::new(),
            chip_sort: Default::default(),
            book_sort: Default::default(),
            languages: Vec::new(),
            hide_finished: false,
            include_direct_child_books: false,
        };
        let mut locations = vec![query.location.clone()];
        let mut visited = std::collections::HashSet::new();
        let mut hashes = std::collections::HashSet::new();
        while let Some(location) = locations.pop() {
            if !visited.insert(location.clone()) {
                continue;
            }
            query.location = location;
            let page = self.db.browse_folder_contents(&query, false).map_err(|error| error.to_string())?;
            hashes.extend(page.books.into_iter().filter(|book| book.downloaded).map(|book| book.content_hash));
            locations.extend(page.children.into_iter().filter(|child| child.downloaded_book_count > 0).map(|child| child.id));
        }
        let remote = self.db.remote_asset_hashes(crate::BlobKind::Book).map_err(|error| error.to_string())?;
        let mut evicted = 0;
        for content_hash in hashes {
            if !remote.contains(&content_hash) {
                continue;
            }
            self.assets.evict_local_book(&self.db, &content_hash).map_err(|error| error.to_string())?;
            let state = self.persistent_download_state(content_hash).map_err(|error| error.to_string())?;
            self.emit(LibraryEvent::DownloadStatusChanged { content_hash, state });
            evicted += 1;
        }
        if evicted > 0 {
            self.notify_contents_changed();
        }
        Ok(evicted)
    }
}

fn collect_browse_downloads(mut query: library_model::LibraryBrowseQuery, mut contents: impl FnMut(&library_model::LibraryBrowseQuery) -> Result<library_model::BrowseContents, String>) -> Result<Vec<crate::ContentHash>, String> {
    let mut locations = vec![query.location.clone()];
    let mut visited = std::collections::HashSet::new();
    let mut hashes = std::collections::HashSet::new();
    while let Some(location) = locations.pop() {
        if !visited.insert(location.clone()) {
            continue;
        }
        query.location = location;
        let page = contents(&query)?;
        hashes.extend(page.books.into_iter().filter(|book| !book.downloaded && !book.download_requested).map(|book| book.content_hash));
        locations.extend(page.children.into_iter().filter(|child| child.downloaded_book_count < child.book_count).map(|child| child.id));
    }
    Ok(hashes.into_iter().collect())
}
