//! Normalize transport options once, before opening a browse snapshot.
use library_model::{BrowseBookSort, BrowseChipSort, LibraryBrowseQuery, LibraryFileTypeFilter};

pub(super) struct BrowseOptions {
    pub search: String,
    pub file_types: i32,
    pub languages: String,
    pub chip_sort: BrowseChipSort,
    pub book_sort: BrowseBookSort,
    pub downloaded_only: bool,
    pub hide_finished: bool,
    pub include_direct_child_books: bool,
}

impl BrowseOptions {
    pub fn new(search: &str, file_types: &[LibraryFileTypeFilter], languages: &[&str], downloaded_only: bool) -> Self {
        Self {
            search: book_model::normalize_search_text(search),
            file_types: file_types_query_value(file_types),
            languages: languages_query_value(languages),
            chip_sort: BrowseChipSort::Alphabetical,
            book_sort: BrowseBookSort::Alphabetical,
            downloaded_only,
            hide_finished: false,
            include_direct_child_books: false,
        }
    }

    pub fn from_query(query: &LibraryBrowseQuery, downloaded_only: bool) -> Self {
        let languages = query.languages.iter().map(String::as_str).collect::<Vec<_>>();
        Self {
            chip_sort: query.chip_sort,
            book_sort: query.book_sort,
            hide_finished: query.hide_finished,
            include_direct_child_books: query.include_direct_child_books,
            ..Self::new(&query.search, &query.file_types, &languages, downloaded_only)
        }
    }
}

pub(crate) fn file_type_query_value(filter: LibraryFileTypeFilter) -> i32 {
    match filter {
        LibraryFileTypeFilter::All => 0,
        LibraryFileTypeFilter::Book => 1,
        LibraryFileTypeFilter::Pdf => 2,
        LibraryFileTypeFilter::Audiobook => 4,
    }
}

pub(crate) fn file_types_query_value(filters: &[LibraryFileTypeFilter]) -> i32 {
    if filters.is_empty() || filters.contains(&LibraryFileTypeFilter::All) {
        return 0;
    }
    filters.iter().fold(0, |mask, filter| mask | file_type_query_value(*filter))
}

pub(crate) fn languages_query_value(languages: &[&str]) -> String {
    let mut languages = languages.iter().map(|language| language.trim().to_lowercase().replace('_', "-").split('-').next().unwrap_or_default().to_owned()).filter(|language| !language.is_empty()).collect::<Vec<_>>();
    languages.sort();
    languages.dedup();
    if languages.is_empty() { String::new() } else { format!(",{},", languages.join(",")) }
}

pub(crate) const fn chip_sort_query_value(sort: BrowseChipSort) -> i32 {
    match sort {
        BrowseChipSort::Alphabetical => 0,
        BrowseChipSort::BookCount => 1,
    }
}

pub(crate) const fn book_sort_query_value(sort: BrowseBookSort) -> i32 {
    match sort {
        BrowseBookSort::Alphabetical => 0,
        BrowseBookSort::RecentlyRead => 1,
    }
}
