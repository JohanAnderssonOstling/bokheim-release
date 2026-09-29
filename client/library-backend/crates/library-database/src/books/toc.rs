//! Stored navigation document reads.

use include_sqlite_sql::include_sql;

// Stored navigation document reads.
//
// Writes live with the import pipeline that produces them; what is read here
// is one book's navigation in the form inspection already produced it.

use library_model::BookTocEntry;

include_sql!("src/books/sql/toc.sql");

/// The title of one entry in a stored tree. The target is the identity; the
/// wording comes from whatever navigation document is stored now.
pub(crate) fn entry_title(entries: &[BookTocEntry], target: &str) -> Option<String> {
    for entry in entries {
        if entry.target == target {
            return Some(entry.title.clone());
        }
        if let Some(found) = entry_title(&entry.children, target) {
            return Some(found);
        }
    }
    None
}
