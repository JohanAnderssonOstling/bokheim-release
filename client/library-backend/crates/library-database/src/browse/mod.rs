//! Library browse pages, hierarchy projections, home, authors, and search.

mod facets;
mod options;
mod rows;
mod snapshot;
use facets::{read_facet_rows, retain_selected_facet_choices};
use options::{BrowseOptions, book_sort_query_value, chip_sort_query_value};
use rows::{invalid_integer_column, invalid_text_column, parse_uuid_column, read_book_card_row};
pub(crate) mod folder_cache;
pub(crate) mod folders;
pub(crate) mod library;
pub(crate) mod progress;
pub(crate) mod search;
pub(crate) mod subjects;

use include_sqlite_sql::include_sql;

use library_model::BookCardRow;
use library_model::{AuthorSummary, BrowseRow, LibraryFormatCount, LibraryLanguageCount, SubjectPath};
use sync_common::DirId;

#[derive(Clone, serde::Serialize)]
pub struct LibraryAuthorsView {
    pub authors: Vec<AuthorSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LibraryHierarchyEntry<Id> {
    pub id: Id,
    pub name: String,
    pub book_count: usize,
    pub downloaded_book_count: usize,
}

#[derive(Clone, serde::Serialize)]
pub struct LibraryHierarchyView<Id> {
    pub children: Vec<LibraryHierarchyEntry<Id>>,
    pub books: Vec<BookCardRow>,
}

pub type LibraryFolderEntry = LibraryHierarchyEntry<DirId>;
pub type LibrarySubjectEntry = LibraryHierarchyEntry<SubjectPath>;
pub type LibraryFolderView = LibraryHierarchyView<DirId>;
pub type LibrarySubjectView = LibraryHierarchyView<SubjectPath>;

/// Complete library-owned browse projection returned to any UI transport.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct LibraryBrowseData {
    #[serde(default)]
    pub chip_groups: Vec<library_model::BrowseChipGroup>,
    #[serde(default)]
    pub sections: Vec<library_model::BrowseSection>,
    pub contents: library_model::BrowseContents,
    pub path: Vec<library_model::BrowsePathSegment>,
    pub language_counts: Vec<library_model::LibraryLanguageCount>,
    pub format_counts: Vec<library_model::LibraryFormatCount>,
}

include_sql!("src/browse/sql/schema.sql");
include_sql!("src/browse/sql/subject_schema.sql");
include_sql!("src/browse/sql/folder_schema.sql");
include_sql!("src/browse/sql/folders.sql");
include_sql!("src/browse/sql/subjects.sql");
include_sql!("src/browse/sql/subject_cache.sql");
include_sql!("src/browse/sql/home.sql");
include_sql!("src/browse/sql/authors.sql");
include_sql!("src/browse/sql/detail.sql");
include_sql!("src/browse/sql/search.sql");
include_sql!("src/browse/sql/counts.sql");

#[cfg(test)]
mod tests;
