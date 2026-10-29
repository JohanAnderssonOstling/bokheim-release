//! Normalize transport options once, before opening a browse snapshot.
pub(crate) use book_model::BookFormat;
pub(crate) use library_model::{BrowseBookSort, BrowseChipSort, LibraryBrowseQuery, LibraryFileTypeFilter};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct BrowseOptions {
    pub search: String,
    pub file_types: i32,
    pub languages: String,
    pub chip_sort: BrowseChipSort,
    pub book_sort: BrowseBookSort,
    pub downloaded_only: bool,
    pub hide_finished: bool,
}

impl BrowseOptions {
    pub(super) fn needs_popularity(&self) -> bool {
        self.chip_sort == BrowseChipSort::Popularity || self.book_sort == BrowseBookSort::Popularity
    }
    pub fn from_query(query: &LibraryBrowseQuery, downloaded_only: bool) -> Self {
        Self {
            search: book_model::normalize_search_text(&query.search),
            file_types: if query.file_types.is_empty() || query.file_types.contains(&LibraryFileTypeFilter::All) { 0 } else { query.file_types.iter().fold(0, |mask, filter| mask | file_type_query_value(*filter)) },
            languages: {
                let mut languages = query.languages.iter().map(|language| base_language(language)).filter(|language| !language.is_empty()).collect::<Vec<_>>();
                languages.sort();
                languages.dedup();
                if languages.is_empty() { String::new() } else { format!(",{},", languages.join(",")) }
            },
            chip_sort: query.chip_sort,
            book_sort: query.book_sort,
            downloaded_only,
            hide_finished: query.hide_finished,
        }
    }
}

/// The mask of stored `BookFormat` codes a file-type filter matches.
pub(crate) fn file_type_query_value(filter: LibraryFileTypeFilter) -> i32 {
    match filter {
        LibraryFileTypeFilter::All => 0,
        LibraryFileTypeFilter::Book => BookFormat::Epub as i32 | BookFormat::Mobi as i32 | BookFormat::Fb2 as i32 | BookFormat::Cbz as i32,
        LibraryFileTypeFilter::Pdf => BookFormat::Pdf as i32,
        LibraryFileTypeFilter::Audiobook => BookFormat::M4b as i32 | BookFormat::Mp3Folder as i32,
    }
}

/// The file-type filter a stored `BookFormat` code belongs to. A book whose
/// format no device has observed yet shows as a book.
pub(crate) fn file_type_of(format: Option<i64>) -> LibraryFileTypeFilter {
    match format.and_then(BookFormat::from_code) {
        Some(BookFormat::Pdf) => LibraryFileTypeFilter::Pdf,
        Some(BookFormat::M4b | BookFormat::Mp3Folder) => LibraryFileTypeFilter::Audiobook,
        _ => LibraryFileTypeFilter::Book,
    }
}

/// The lowercase primary subtag browse filters on: `en-US` and `en_GB` are `en`.
pub(crate) fn base_language(tag: &str) -> String {
    tag.trim().split(['-', '_']).next().unwrap_or_default().to_lowercase()
}
