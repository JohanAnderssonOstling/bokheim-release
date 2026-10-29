//! Browse-driven downloads and eviction over shared database queries.

use super::events::LibraryEvent;
use crate::library::LibrarySession;
use crate::{BookId, DirId};
use library_model::{BookCardRow, BrowseLocation, BrowseRow, LibraryBrowseQuery};
use std::collections::HashSet;

impl LibrarySession {
    fn browse_hashes(&self, mut query: LibraryBrowseQuery, include_book: impl Fn(&BookCardRow) -> bool, descend: impl Fn(&BrowseRow) -> bool) -> Result<HashSet<BookId>, String> {
        let mut locations = vec![query.location];
        let mut visited = HashSet::new();
        let mut hashes = HashSet::new();
        while let Some(location) = locations.pop() {
            if !visited.insert(location) {
                continue;
            }
            query.location = location;
            let page = self.db.browse_contents(&query, false).map_err(|error| error.to_string())?;
            hashes.extend(page.books.into_iter().filter(&include_book).filter_map(|book| book.content_hash()));
            locations.extend(page.children.into_iter().filter(&descend).map(|child| child.id));
        }
        Ok(hashes)
    }

    pub(crate) fn queue_browse_downloads(&self, query: LibraryBrowseQuery) -> Result<(), String> {
        let hashes = self.browse_hashes(query, |book| !book.downloaded && !book.download_requested, |child| child.downloaded_book_count < child.book_count)?;
        self.request_downloads(&hashes.into_iter().collect::<Vec<_>>()).map_err(|error| error.to_string())
    }

    /// Evicts the downloaded copies under one folder without deleting any
    /// entry. Only books the server also holds are evicted, so a local-only
    /// book keeps its bytes. Returns how many copies were evicted.
    pub(crate) fn evict_folder_downloads(&self, directory_id: &DirId) -> Result<usize, String> {
        let query =
            LibraryBrowseQuery { location: BrowseLocation::Folder(*directory_id), search: String::new(), file_types: Vec::new(), chip_sort: Default::default(), book_sort: Default::default(), languages: Vec::new(), hide_finished: false };
        let hashes = self.browse_hashes(query, |book| book.downloaded, |child| child.downloaded_book_count > 0)?;
        let remote = self.db.remote_asset_hashes(library_model::BlobKind::Book).map_err(|error| error.to_string())?;
        let mut evicted = 0;
        for content_hash in hashes {
            if !remote.contains(&content_hash) {
                continue;
            }
            library_placement::evict_local_book(&self.assets, &self.db, &content_hash).map_err(|error| error.to_string())?;
            let state = self.persistent_download_state(content_hash).map_err(|error| error.to_string())?;
            self.emit(LibraryEvent::DownloadStatusChanged { content_hash, state });
            evicted += 1;
        }
        if evicted > 0 {
            self.events.notify_contents_changed();
        }
        Ok(evicted)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
