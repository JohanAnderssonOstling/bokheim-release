//! Subject browsing: taxonomy.

use crate::browse::{BrowseRow, LibraryFormatCount, LibraryLanguageCount, LibrarySubjectEntry, LibrarySubjectView, SubjectCacheSql, SubjectsSql, book_sort_query_value, read_book_card_row, read_facet_rows, retain_selected_facet_choices};
use crate::{Database, DatabaseError};
use library_model::{BookCardRow, BrowseBookSort, BrowseChipSort, LibraryFileTypeFilter, SubjectPath};
use subject_projection::ROOT_SUBJECT_PATH;
use std::collections::{BTreeMap, BTreeSet};

mod cache;
mod navigation;

#[derive(Default)]
struct SubjectNode {
    label: String,
    parent: i64,
    children: BTreeSet<i64>,
    members: BTreeSet<i64>,
    direct: BTreeSet<i64>,
}

/// The books under a subject that pass the page's filters. The search is not
/// one of them: it picks subjects by label and books by `text_matches`, and a
/// subject's count is of every book it holds that passes the filters.
struct MatchingSubjectBooks {
    books: BTreeSet<i64>,
    downloaded: BTreeSet<i64>,
    /// Books whose own title or author contains the search. Empty when there
    /// is no search.
    text_matches: BTreeSet<i64>,
}

impl Database {
    pub fn browse_library_subjects(&self, subject_path: Option<&str>, file_type: LibraryFileTypeFilter, downloaded_only: bool) -> Result<LibrarySubjectView, DatabaseError> {
        let query = library_model::LibraryBrowseQuery {
            location: subject_path.unwrap_or(ROOT_SUBJECT_PATH).to_owned(),
            search: String::new(),
            file_types: vec![file_type],
            languages: vec![],
            chip_sort: BrowseChipSort::Alphabetical,
            book_sort: BrowseBookSort::Alphabetical,
            hide_finished: false,
            include_direct_child_books: false,
        };
        let contents = self.browse_subject_contents(&query, downloaded_only)?;
        let children = contents.children.into_iter().map(|row| LibrarySubjectEntry { id: SubjectPath::new(row.id), name: row.name, book_count: row.book_count, downloaded_book_count: row.downloaded_book_count }).collect();
        Ok(LibrarySubjectView { children, books: contents.books })
    }

    /// Complete subject page from one cache-validated snapshot.
    pub fn browse_subject(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<super::LibraryBrowseData, DatabaseError> {
        let options = super::BrowseOptions::from_query(query, downloaded_only);
        self.with_subject_cache(|| {
            let route = self.subject_route_id(&query.location)?.unwrap_or(-1);
            let contents = self.subject_contents(route, &options)?;
            let mut sections = Vec::new();
            if options.search.is_empty() {
                for parent in &contents.children {
                    let child_route = self.subject_route_id(&parent.id)?.unwrap_or(-1);
                    let child_contents = self.subject_contents(child_route, &options)?;
                    if child_contents.books.is_empty() && child_contents.children.is_empty() {
                        continue;
                    }
                    sections.push(library_model::BrowseSection { parent: parent.clone(), children: child_contents.children, books: child_contents.books });
                }
            }
            let (language_counts, format_counts) = self.subject_facets(route, &options)?;
            Ok(super::LibraryBrowseData { contents, chip_groups: Vec::new(), sections, path: Vec::new(), language_counts, format_counts })
        })
    }

    /// Show immediate subject routes and books assigned directly to this route.
    pub fn browse_subject_contents(&self, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<library_model::BrowseContents, DatabaseError> {
        let options = super::BrowseOptions::from_query(query, downloaded_only);
        self.with_subject_cache(|| {
            let route = self.subject_route_id(&query.location)?.unwrap_or(-1);
            self.subject_contents(route, &options)
        })
    }

    fn subject_contents(&self, location: i64, options: &super::BrowseOptions) -> Result<library_model::BrowseContents, DatabaseError> {
        let nodes = self.load_subject_tree(location)?;
        let matching = self.matching_subject_books(location, options)?;
        let Some(root) = nodes.get(&location) else {
            return Ok(library_model::BrowseContents { children: Vec::new(), books: Vec::new() });
        };
        if !options.search.is_empty() {
            return self.searched_subject_contents(location, &nodes, &matching, options);
        }

        let mut selected_books = root.direct.clone();
        if options.include_direct_child_books {
            for child in &root.children {
                selected_books.extend(&nodes[child].direct);
            }
        }
        selected_books.retain(|book| matching.books.contains(book));

        let mut children = Vec::new();
        for &route in &root.children {
            let node = &nodes[&route];
            let book_count = node.members.intersection(&matching.books).count();
            if book_count == 0 {
                continue;
            }
            let id = self.subject_route_path(route)?.ok_or_else(|| DatabaseError::message("missing subject route"))?;
            children.push(BrowseRow {
                id,
                name: node.label.clone(),
                path: None,
                book_count,
                downloaded_book_count: node.members.intersection(&matching.downloaded).count(),
            });
        }
        sort_subject_rows(&mut children, options.chip_sort);
        let books = self.subject_cards(&selected_books, options)?;
        Ok(library_model::BrowseContents { children, books })
    }

    /// A search from a subject reaches the subject itself and everything below
    /// it, at any depth: every subject whose label matches, with the path that
    /// places it under this one, and every book whose title or author matches.
    fn searched_subject_contents(&self, location: i64, nodes: &BTreeMap<i64, SubjectNode>, matching: &MatchingSubjectBooks, options: &super::BrowseOptions) -> Result<library_model::BrowseContents, DatabaseError> {
        let mut children = Vec::new();
        for (&route, node) in nodes {
            if route == location || !book_model::normalize_search_text(&node.label).contains(&options.search) {
                continue;
            }
            let book_count = node.members.intersection(&matching.books).count();
            if book_count == 0 {
                continue;
            }
            let id = self.subject_route_path(route)?.ok_or_else(|| DatabaseError::message("missing subject route"))?;
            children.push(BrowseRow {
                id,
                name: node.label.clone(),
                path: subject_path_below(location, route, nodes),
                book_count,
                downloaded_book_count: node.members.intersection(&matching.downloaded).count(),
            });
        }
        sort_subject_rows(&mut children, options.chip_sort);
        let root = &nodes[&location];
        let selected_books = root.members.iter().copied().filter(|book| matching.books.contains(book) && matching.text_matches.contains(book)).collect::<BTreeSet<_>>();
        let books = self.subject_cards(&selected_books, options)?;
        Ok(library_model::BrowseContents { children, books })
    }

    fn subject_cards(&self, selected_books: &BTreeSet<i64>, options: &super::BrowseOptions) -> Result<Vec<BookCardRow>, DatabaseError> {
        let mut books = Vec::new();
        if !selected_books.is_empty() {
            let ids = serde_json::to_string(selected_books).map_err(DatabaseError::operation)?;
            self.connection.subject_displayed_cards(&ids, book_sort_query_value(options.book_sort), |row| {
                books.push(read_book_card_row(row)?);
                Ok(())
            })?;
        }
        Ok(books)
    }

    fn load_subject_tree(&self, location: i64) -> Result<BTreeMap<i64, SubjectNode>, DatabaseError> {
        let mut nodes = BTreeMap::<i64, SubjectNode>::new();
        self.connection.subject_cached_members(location, |row| {
            let route = row.get(0)?;
            let node = nodes.entry(route).or_default();
            node.parent = row.get(1)?;
            node.label = row.get(2)?;
            let book = row.get(3)?;
            node.members.insert(book);
            if row.get::<_, bool>(4)? {
                node.direct.insert(book);
            }
            Ok(())
        })?;
        let edges = nodes.iter().map(|(route, node)| (*route, node.parent)).collect::<Vec<_>>();
        for (route, parent) in edges {
            if route != parent {
                if let Some(node) = nodes.get_mut(&parent) {
                    node.children.insert(route);
                }
            }
        }
        Ok(nodes)
    }

    fn matching_subject_books(&self, location: i64, options: &super::BrowseOptions) -> Result<MatchingSubjectBooks, DatabaseError> {
        let mut matching = BTreeSet::new();
        let mut downloaded = BTreeSet::new();
        let mut text_matches = BTreeSet::new();
        self.connection.subject_matching_books(location, i32::from(options.downloaded_only), options.file_types, &options.languages, &options.search, |row| {
            if options.hide_finished && row.get::<_, f32>(3)? >= library_model::FINISHED_PROGRESS_THRESHOLD {
                return Ok(());
            }
            let book = row.get(0)?;
            matching.insert(book);
            if row.get::<_, bool>(1)? {
                downloaded.insert(book);
            }
            if !options.search.is_empty() && row.get::<_, bool>(2)? {
                text_matches.insert(book);
            }
            Ok(())
        })?;
        Ok(MatchingSubjectBooks { books: matching, downloaded, text_matches })
    }

    pub fn browse_subject_facet_counts(&self, subject_path: &str, query: &str, file_types: &[LibraryFileTypeFilter], languages: &[&str], downloaded_only: bool) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), DatabaseError> {
        let options = super::BrowseOptions::new(query, file_types, languages, downloaded_only);
        self.with_subject_cache(|| {
            let route = self.subject_route_id(subject_path)?.unwrap_or(-1);
            self.subject_facets(route, &options)
        })
    }

    fn subject_facets(&self, route: i64, options: &super::BrowseOptions) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), DatabaseError> {
        let (mut language_counts, mut format_counts) = read_facet_rows(|rows| self.connection.get_subject_facets(route, &options.search, options.file_types, &options.languages, i32::from(options.downloaded_only), rows))?;
        retain_selected_facet_choices(&mut language_counts, &mut format_counts, &options.languages, options.file_types);
        Ok((language_counts, format_counts))
    }
}

fn sort_subject_rows(rows: &mut [BrowseRow], chip_sort: BrowseChipSort) {
    rows.sort_by(|left, right| {
        let counts = if chip_sort == BrowseChipSort::BookCount { right.book_count.cmp(&left.book_count) } else { std::cmp::Ordering::Equal };
        counts.then_with(|| book_model::normalize_search_text(&left.name).cmp(&book_model::normalize_search_text(&right.name))).then_with(|| left.name.cmp(&right.name)).then_with(|| left.id.cmp(&right.id))
    });
}

/// The labels between `location` and `route`, outermost first, as a searched
/// subject's chip states them — the way a searched folder carries the path to
/// its parent. `None` for a direct child, which needs no placing.
fn subject_path_below(location: i64, route: i64, nodes: &BTreeMap<i64, SubjectNode>) -> Option<String> {
    let mut labels = Vec::new();
    let mut current = nodes.get(&route)?.parent;
    while current != location {
        let Some(node) = nodes.get(&current) else { break };
        labels.push(node.label.as_str());
        if node.parent == current {
            break;
        }
        current = node.parent;
    }
    labels.reverse();
    (!labels.is_empty()).then(|| labels.join(" \u{203a} "))
}
