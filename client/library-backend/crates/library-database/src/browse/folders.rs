//! Folder browsing: hierarchy, contents, facets, paths.

use crate::browse::{
    BrowseRow, FoldersSql, LibraryFolderEntry, LibraryFolderView, LibraryFormatCount, LibraryLanguageCount, book_sort_query_value, chip_sort_query_value, invalid_integer_column, parse_uuid_column, read_book_card_row, read_facet_rows,
    retain_selected_facet_choices,
};
use crate::{Database, DatabaseError};
use library_model::{BookCardRow, LibraryFileTypeFilter};
use sync_common::DirId;

impl Database {
    /// Complete folder page, including facets and breadcrumbs, from one snapshot.
    pub fn browse_folder(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<super::LibraryBrowseData, DatabaseError> {
        let directory_id = DirId::parse_str(&query.location).map_err(DatabaseError::operation)?;
        let options = super::BrowseOptions::from_query(query, downloaded_only);
        self.with_folder_cache(|| {
            let (contents, chip_groups) = self.grouped_folder_contents(&directory_id, &options)?;
            let sections = if options.search.is_empty() {
                let mut panel_parents = Vec::new();
                let mut child_offset = 0;
                for group in &chip_groups {
                    panel_parents.extend(contents.children[child_offset..group.start].iter().cloned());
                    panel_parents.push(group.parent.clone());
                    child_offset = group.end;
                }
                panel_parents.extend(contents.children[child_offset..].iter().cloned());
                panel_parents
                    .iter()
                    .map(|parent| {
                        let child_id = DirId::parse_str(&parent.id).map_err(DatabaseError::operation)?;
                        let child_contents = self.folder_contents(&child_id, &options)?;
                        Ok(library_model::BrowseSection { parent: parent.clone(), children: child_contents.children, books: child_contents.books })
                    })
                    .collect::<Result<Vec<_>, DatabaseError>>()?
            } else {
                Vec::new()
            };
            let (language_counts, format_counts) = self.folder_facets(&directory_id, &options)?;
            let path = self.browse_directory_path(&directory_id)?;
            Ok(super::LibraryBrowseData { contents, chip_groups, sections, path, language_counts, format_counts })
        })
    }

    pub fn browse_library_folder(&self, directory_id: &DirId, downloaded_only: bool, file_type: LibraryFileTypeFilter) -> Result<LibraryFolderView, DatabaseError> {
        let options = super::BrowseOptions::new("", &[file_type], &[], downloaded_only);
        self.with_folder_cache(|| {
            let directories = self.folder_children(directory_id, &options)?;
            let books = self.folder_books(directory_id, &options)?;
            let children = directories
                .into_iter()
                .map(|row| Ok(LibraryFolderEntry { id: parse_uuid_column(row.id, 0)?, name: row.name, book_count: row.book_count, downloaded_book_count: row.downloaded_book_count }))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(LibraryFolderView { children, books })
        })
    }

    fn folder_contents(&self, folder_id: &DirId, options: &super::BrowseOptions) -> Result<library_model::BrowseContents, DatabaseError> {
        let mut children = self.folder_children(folder_id, options)?;
        let mut books = self.folder_books(folder_id, options)?;
        // Display the selected scope's count; the directory picker retains total holdings.
        if options.downloaded_only {
            for child in &mut children {
                child.book_count = child.downloaded_book_count;
            }
        }
        // Facets and folder counts intentionally describe full holdings.
        if options.hide_finished {
            books.retain(|book| book.progress < library_model::FINISHED_PROGRESS_THRESHOLD);
        }
        Ok(library_model::BrowseContents { children, books })
    }

    /// Ungrouped contents for recursive operations such as requesting downloads.
    pub fn browse_folder_contents(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<library_model::BrowseContents, DatabaseError> {
        let folder_id = DirId::parse_str(&query.location).map_err(DatabaseError::operation)?;
        let options = super::BrowseOptions::from_query(query, downloaded_only);
        self.with_folder_cache(|| self.folder_contents(&folder_id, &options))
    }

    fn grouped_folder_contents(&self, folder_id: &DirId, options: &super::BrowseOptions) -> Result<(library_model::BrowseContents, Vec<library_model::BrowseChipGroup>), DatabaseError> {
        let mut contents = self.folder_contents(folder_id, options)?;
        let groups = if options.search.is_empty() { self.group_folder_children(&mut contents.children, options)? } else { Vec::new() };
        Ok((contents, groups))
    }

    pub fn browse_directory_path(&self, directory_id: &DirId) -> Result<Vec<library_model::BrowsePathSegment>, DatabaseError> {
        Ok(self.dir_path_components(directory_id)?.into_iter().map(|component| library_model::BrowsePathSegment { id: component.id.to_string(), name: component.name }).collect())
    }

    fn folder_books(&self, dir_id: &DirId, options: &super::BrowseOptions) -> Result<Vec<BookCardRow>, DatabaseError> {
        let dir_id = dir_id.to_string();
        let normalized_query = &options.search;
        let downloaded_only = options.downloaded_only;
        let book_sort = options.book_sort;
        let mut cards = Vec::new();
        let languages = &options.languages;
        let mut append = |row: &rusqlite::Row<'_>| {
            cards.push(read_book_card_row(row)?);
            Ok(())
        };
        if normalized_query.is_empty() {
            self.connection.get_book_cards_in_dir(&dir_id, i32::from(downloaded_only), options.file_types, book_sort_query_value(book_sort), &languages, &mut append)?;
        } else {
            self.connection.search_book_cards_in_dir(&dir_id, i32::from(downloaded_only), normalized_query, options.file_types, book_sort_query_value(book_sort), &languages, &mut append)?;
        }
        Ok(cards)
    }
    fn folder_children(&self, parent_id: &DirId, options: &super::BrowseOptions) -> Result<Vec<BrowseRow>, DatabaseError> {
        let parent_id = parent_id.to_string();
        let normalized_query = &options.search;
        let chip_sort = options.chip_sort;
        let mut folders = Vec::new();
        let languages = &options.languages;
        let file_types = options.file_types;
        let mut append_row = |row: &rusqlite::Row<'_>| {
            let raw_id: String = row.get(0)?;
            let raw_book_count: i64 = row.get(2)?;
            let raw_downloaded_book_count: i64 = row.get(3)?;
            folders.push(BrowseRow {
                id: parse_uuid_column(raw_id, 0)?.to_string(),
                name: row.get(1)?,
                path: row.get::<_, Option<String>>(4)?.filter(|path| !path.is_empty()),
                book_count: usize::try_from(raw_book_count).map_err(|_| invalid_integer_column(2, "folder book count must not be negative"))?,
                downloaded_book_count: usize::try_from(raw_downloaded_book_count).map_err(|_| invalid_integer_column(3, "downloaded folder book count must not be negative"))?,
            });
            Ok(())
        };
        if normalized_query.is_empty() && file_types == 0 && languages.is_empty() {
            self.connection.get_immediate_sub_dirs(&parent_id, options.downloaded_only, chip_sort_query_value(chip_sort), &mut append_row)?;
        } else if normalized_query.is_empty() {
            self.connection.get_filtered_immediate_sub_dirs(&parent_id, options.downloaded_only, file_types, chip_sort_query_value(chip_sort), &languages, &mut append_row)?;
        } else {
            self.connection.get_sub_dirs(&parent_id, options.downloaded_only, normalized_query, file_types, chip_sort_query_value(chip_sort), &languages, &mut append_row)?;
        }
        Ok(folders)
    }

    pub fn browse_folder_facet_counts(&self, folder_id: &str, query: &str, file_types: &[LibraryFileTypeFilter], languages: &[&str], downloaded_only: bool) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), DatabaseError> {
        let folder_id = DirId::parse_str(folder_id).map_err(DatabaseError::operation)?;
        let options = super::BrowseOptions::new(query, file_types, languages, downloaded_only);
        self.with_folder_cache(|| self.folder_facets(&folder_id, &options))
    }

    fn folder_facets(&self, folder_id: &DirId, options: &super::BrowseOptions) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), DatabaseError> {
        let (mut language_counts, mut format_counts) = read_facet_rows(|rows| self.connection.get_folder_facets(&folder_id.to_string(), &options.search, options.file_types, &options.languages, i32::from(options.downloaded_only), rows))?;
        retain_selected_facet_choices(&mut language_counts, &mut format_counts, &options.languages, options.file_types);
        Ok((language_counts, format_counts))
    }

    fn group_folder_children(&self, children: &mut Vec<BrowseRow>, options: &super::BrowseOptions) -> rusqlite::Result<Vec<library_model::BrowseChipGroup>> {
        if children.is_empty() {
            return Ok(Vec::new());
        }
        let ids = serde_json::to_string(&children.iter().map(|row| &row.id).collect::<Vec<_>>()).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let mut replacements = std::collections::HashMap::<String, Vec<BrowseRow>>::new();
        self.connection.group_folder_children_with(&ids, options.file_types, &options.languages, chip_sort_query_value(options.chip_sort), options.downloaded_only, |row| {
            let total = usize::try_from(row.get::<_, i64>(3)?).map_err(|_| invalid_integer_column(3, "folder count must not be negative"))?;
            let downloaded = usize::try_from(row.get::<_, i64>(4)?).map_err(|_| invalid_integer_column(4, "downloaded count must not be negative"))?;
            replacements.entry(row.get(0)?).or_default().push(BrowseRow { id: row.get(1)?, name: row.get(2)?, path: None, book_count: if options.downloaded_only { downloaded } else { total }, downloaded_book_count: downloaded });
            Ok(())
        })?;
        Ok(expand_chip_groups(children, replacements))
    }
}

/// Flatten one explicitly supplied level without inspecting promoted children.
/// Keep ungrouped chips together before headed groups, retaining input order
/// within both sections and within each group's children.
fn expand_chip_groups(children: &mut Vec<BrowseRow>, mut replacements: std::collections::HashMap<String, Vec<BrowseRow>>) -> Vec<library_model::BrowseChipGroup> {
    let mut flat = Vec::new();
    let mut expanded = Vec::new();
    for parent in std::mem::take(children) {
        match replacements.remove(&parent.id).filter(|rows| !rows.is_empty()) {
            Some(rows) => expanded.push((parent, rows)),
            None => flat.push(parent),
        }
    }
    let mut groups = Vec::with_capacity(expanded.len());
    for (parent, rows) in expanded {
        let start = flat.len();
        flat.extend(rows);
        groups.push(library_model::BrowseChipGroup { parent, start, end: flat.len() });
    }
    *children = flat;
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_preserves_order_and_group_ranges_without_recursing() {
        let row = |id: &str| BrowseRow { id: id.into(), name: id.into(), path: None, book_count: 0, downloaded_book_count: 0 };
        let mut children = vec![row("group-a"), row("plain"), row("group-b"), row("empty")];
        let replacements = std::collections::HashMap::from([("group-a".into(), vec![row("a2"), row("a1")]), ("group-b".into(), vec![row("b")]), ("empty".into(), vec![]), ("a2".into(), vec![row("deeper")])]);
        let groups = expand_chip_groups(&mut children, replacements);
        assert_eq!(children.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["plain", "empty", "a2", "a1", "b"]);
        assert_eq!(groups.iter().map(|group| (group.parent.id.as_str(), group.start, group.end)).collect::<Vec<_>>(), [("group-a", 2, 4), ("group-b", 4, 5)]);
    }
}
