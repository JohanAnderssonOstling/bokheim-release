//! Subject browsing: taxonomy. The same pages browse the library's books and
//! the public-domain catalog's; only where the books come from differs.

use crate::browse::books::MatchingBooks;
#[cfg(test)]
use crate::browse::sections::SectionSelection;
use crate::browse::{BrowseOptions, BrowseRow, SubjectsSql};
use crate::{Database, DatabaseError};
use library_model::BrowseLocation;
#[cfg(test)]
use library_model::BrowsePageRequest;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod tree;

/// Whose books a subject page browses: the library's, by book row, or the
/// catalog's, by row ID in the catalog browse projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Books {
    Library,
    Catalog,
}

impl Books {
    pub(super) fn location(self, route: i64) -> BrowseLocation {
        match self {
            Self::Library => BrowseLocation::Subject(route),
            Self::Catalog => BrowseLocation::Catalog(route),
        }
    }
}

/// The crumbs from the root down to `route`, root first. Empty when `route`
/// is not in the displayed tree, which is where compaction left it.
pub(in crate::browse) fn subject_path(books: Books, route: i64, nodes: &BTreeMap<i64, SubjectNode>) -> Vec<library_model::BrowsePathSegment> {
    let mut path = Vec::new();
    let mut current = route;
    while current != library_model::ROOT_SUBJECT_ROUTE_ID {
        let Some(node) = nodes.get(&current) else { return Vec::new() };
        path.push(library_model::BrowsePathSegment { id: books.location(current), name: node.label.clone() });
        current = node.parent;
    }
    path.reverse();
    path
}

fn route_of(location: BrowseLocation) -> Option<i64> {
    match location {
        BrowseLocation::Subject(route) | BrowseLocation::Catalog(route) => Some(route),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct SubjectNode {
    pub(super) label: String,
    pub(super) parent: i64,
    pub(super) children: BTreeSet<i64>,
    pub(super) members: BTreeSet<i64>,
    pub(super) direct: BTreeSet<i64>,
}

impl Database {
    /// The subjects a book is listed under, as Browse shows them: one path
    /// per displayed node that holds the book directly. Compaction can have
    /// lifted the book to an ancestor of its assigned concept, and a concept
    /// reached by several routes appears at each; both are what Browse lists.
    /// A book compacted all the way to the root has no subject.
    pub(crate) fn book_subjects(&self, books: Books, book: i64) -> Result<Vec<Vec<library_model::BrowsePathSegment>>, DatabaseError> {
        let nodes = self.subject_tree_for(books)?;
        Ok(nodes.iter().filter(|(route, node)| **route != library_model::ROOT_SUBJECT_ROUTE_ID && node.direct.contains(&book)).map(|(&route, _)| subject_path(books, route, &nodes)).filter(|path| !path.is_empty()).collect())
    }

    #[cfg(test)]
    pub(super) fn browse_subject(
        &self, books: Books, route: i64, query: &library_model::LibraryBrowseQuery, downloaded_only: bool, section_limit: Option<usize>, page: Option<BrowsePageRequest>,
    ) -> Result<super::LibraryBrowseData, DatabaseError> {
        self.with_subject_browse(books, route, query, downloaded_only, |route, nodes, matching, options| {
            let children = Self::subject_children(books, route, nodes, matching, options);
            let direct = Self::subject_selected_books(route, nodes, matching, options);
            let mut selections = Vec::new();
            if options.search.is_empty() {
                for parent in &children {
                    let Some(child_route) = route_of(parent.id) else { continue };
                    let children = Self::subject_children(books, child_route, nodes, matching, options);
                    let selected = Self::subject_selected_books(child_route, nodes, matching, options);
                    if selected.is_empty() && children.is_empty() {
                        continue;
                    }
                    selections.push(SectionSelection { parent: parent.clone(), children, books: selected });
                }
            }
            let (cards, sections, page) = self.load_section_cards(books, &direct, selections, options, section_limit, page)?;
            let contents = library_model::BrowseContents { children, books: cards };
            let (language_counts, format_counts) = match books {
                Books::Library => self.library_facets(options)?,
                Books::Catalog => super::catalog::facets(options)?,
            };
            let path = subject_path(books, route, nodes);
            let ancestors = self.subject_ancestors(books, &path, nodes, options)?;
            Ok(super::LibraryBrowseData { contents, sections, path, ancestors, language_counts, format_counts, feed: None, page })
        })
    }

    /// Show immediate subject routes and books assigned directly to this route.
    #[cfg(test)]
    pub(super) fn browse_subject_contents(&self, books: Books, route: i64, query: &library_model::LibraryBrowseQuery, downloaded_only: bool) -> Result<library_model::BrowseContents, DatabaseError> {
        self.with_subject_browse(books, route, query, downloaded_only, |route, nodes, matching, options| self.subject_contents(books, route, nodes, matching, options))
    }

    #[cfg(test)]
    pub(super) fn browse_subject_book_page(&self, books: Books, route: i64, query: &library_model::LibraryBrowseQuery, downloaded_only: bool, offset: usize, limit: usize) -> Result<library_model::BrowseBookPage, DatabaseError> {
        self.with_subject_browse(books, route, query, downloaded_only, |route, nodes, matching, options| {
            let selected = Self::subject_selected_books(route, nodes, matching, options);
            self.load_source_book_page(books, &selected, options, offset, limit)
        })
    }

    /// One tree and filtering result serve the selected node and its sections.
    #[cfg(test)]
    fn with_subject_browse<T>(
        &self, books: Books, route: i64, query: &library_model::LibraryBrowseQuery, downloaded_only: bool, read: impl FnOnce(i64, &BTreeMap<i64, SubjectNode>, &MatchingBooks, &BrowseOptions) -> Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        let options = BrowseOptions::from_query(query, downloaded_only);
        self.with_browse_snapshot(|| {
            let nodes = self.subject_tree_for(books)?;
            let candidates = nodes.get(&route).map(|node| &node.members);
            let matching = self.matching(books, candidates.unwrap_or(&BTreeSet::new()), &options)?;
            read(route, &nodes, &matching, &options)
        })
    }

    #[cfg(test)]
    fn matching(&self, books: Books, candidates: &BTreeSet<i64>, options: &BrowseOptions) -> Result<MatchingBooks, DatabaseError> {
        match books {
            Books::Library => self.matching_books(candidates, options),
            Books::Catalog => super::catalog::matching(candidates, &self.library_book_hashes()?, options),
        }
    }

    /// Node contents shared by downloads and card sections.
    #[cfg(test)]
    fn subject_contents(&self, books: Books, location: i64, nodes: &BTreeMap<i64, SubjectNode>, matching: &MatchingBooks, options: &BrowseOptions) -> Result<library_model::BrowseContents, DatabaseError> {
        let selected_books = Self::subject_selected_books(location, nodes, matching, options);
        let children = Self::subject_children(books, location, nodes, matching, options);
        let cards = match books {
            Books::Library => self.load_cards(&self.ordered_books(&selected_books, options)?)?,
            Books::Catalog => super::catalog::cards(&selected_books, options)?,
        };
        Ok(library_model::BrowseContents { children, books: cards })
    }

    pub(super) fn subject_selected_books(location: i64, nodes: &BTreeMap<i64, SubjectNode>, matching: &MatchingBooks, options: &BrowseOptions) -> BTreeSet<i64> {
        let Some(root) = nodes.get(&location) else { return BTreeSet::new() };
        let searched = !options.search.is_empty();
        let mut selected_books = if searched { root.members.intersection(&matching.text_matches).copied().collect() } else { root.direct.clone() };
        selected_books.retain(|book| matching.books.contains(book));

        selected_books
    }

    /// The routes a page at `location` lists: its children, or every match
    /// below it when searching, each with a count of the books that pass.
    pub(super) fn subject_children(books: Books, location: i64, nodes: &BTreeMap<i64, SubjectNode>, matching: &MatchingBooks, options: &BrowseOptions) -> Vec<BrowseRow> {
        let Some(root) = nodes.get(&location) else { return Vec::new() };
        let searched = !options.search.is_empty();
        let routes = if searched { tree::subtree_routes(nodes, location) } else { root.children.clone() };
        let mut children = Vec::new();
        for route in routes {
            let node = &nodes[&route];
            if searched && (route == location || !book_model::normalize_search_text(&node.label).contains(&options.search)) {
                continue;
            }
            if let Some(row) = Self::subject_child_row(books, route, node, matching, || {
                if searched {
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
                } else {
                    None
                }
            }) {
                children.push(row);
            }
        }
        super::popularity::sort_rows(&mut children, options.chip_sort, |row| route_of(row.id).and_then(|route| super::popularity::sum(&nodes[&route].members, matching)));
        if books == Books::Catalog {
            if options.chip_sort == library_model::BrowseChipSort::Popularity {
                children.sort_by_cached_key(|row| route_of(row.id).and_then(|route| super::popularity::source_priority(&nodes[&route].members, matching)));
            }
            children.sort_by_key(|row| route_of(row.id).is_none_or(|route| nodes[&route].members.is_disjoint(&matching.preferred)));
        }
        children
    }

    /// The children of the root and of each route on `path` above the last.
    /// The page's own matching covers only its subtree, so the root's is
    /// matched for these.
    #[cfg(test)]
    fn subject_ancestors(&self, books: Books, path: &[library_model::BrowsePathSegment], nodes: &BTreeMap<i64, SubjectNode>, options: &BrowseOptions) -> Result<Vec<(BrowseLocation, Vec<BrowseRow>)>, DatabaseError> {
        let Some((_, above)) = path.split_last() else { return Ok(Vec::new()) };
        let options = BrowseOptions { search: String::new(), ..options.clone() };
        let root = library_model::ROOT_SUBJECT_ROUTE_ID;
        let none = BTreeSet::new();
        let matching = self.matching(books, nodes.get(&root).map_or(&none, |node| &node.members), &options)?;
        let mut ancestors = Vec::with_capacity(above.len() + 1);
        for location in std::iter::once(books.location(root)).chain(above.iter().map(|segment| segment.id)) {
            let Some(route) = route_of(location) else { continue };
            ancestors.push((location, Self::subject_children(books, route, nodes, &matching, &options)));
        }
        Ok(ancestors)
    }

    fn subject_child_row(books: Books, route: i64, node: &SubjectNode, matching: &MatchingBooks, path: impl FnOnce() -> Option<String>) -> Option<BrowseRow> {
        let book_count = node.members.intersection(&matching.books).count();
        if book_count == 0 {
            return None;
        }
        Some(BrowseRow { id: books.location(route), name: node.label.clone(), path: path(), book_count, downloaded_book_count: node.members.intersection(&matching.downloaded).count() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_subject_popularity_uses_source_tiers_and_shared_observations_after_filters() {
        use crate::browse::popularity::sum;
        use library_model::{BrowseBookSort, BrowseChipSort, LibraryBrowseQuery};

        let nodes = BTreeMap::from([
            (0, SubjectNode { children: BTreeSet::from([1, 2, 3, 4, 5]), ..Default::default() }),
            (1, SubjectNode { label: "Gutenberg".into(), members: BTreeSet::from([10]), ..Default::default() }),
            (2, SubjectNode { label: "LoyalBooks".into(), members: BTreeSet::from([11, 12]), ..Default::default() }),
            (3, SubjectNode { label: "Unglue".into(), members: BTreeSet::from([13]), ..Default::default() }),
            (4, SubjectNode { label: "Litteraturbanken".into(), members: BTreeSet::from([14, 10]), ..Default::default() }),
            (5, SubjectNode { label: "PDL".into(), members: BTreeSet::from([15]), ..Default::default() }),
        ]);
        let mut matching = MatchingBooks {
            books: BTreeSet::from([10, 11, 12, 13, 14, 15]),
            downloaded: BTreeSet::new(),
            text_matches: BTreeSet::new(),
            popularity: BTreeMap::from([(10, 100000.0), (11, 1.5), (12, 1.5), (13, 1.0), (14, 0.0)]),
            preferred: BTreeSet::from([15]),
            source_priorities: BTreeMap::from([(10, 4), (11, 3), (12, 3), (13, 2), (14, 1), (15, 0)]),
            popularity_groups: BTreeMap::from([(11, "paired".into()), (12, "paired".into())]),
        };
        let query = LibraryBrowseQuery {
            location: BrowseLocation::Catalog(0),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::Popularity,
            book_sort: BrowseBookSort::Popularity,
            hide_finished: false,
        };
        let options = BrowseOptions::from_query(&query, false);
        let rows = Database::subject_children(Books::Catalog, 0, &nodes, &matching, &options);
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["PDL", "Litteraturbanken", "Unglue", "LoyalBooks", "Gutenberg"]);
        assert_eq!(sum(&nodes[&2].members, &matching), Some(1.5));
        assert_eq!(sum(&nodes[&4].members, &matching), Some(0.0));
        matching.books.remove(&14);
        matching.books.remove(&11);
        let rows = Database::subject_children(Books::Catalog, 0, &nodes, &matching, &options);
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["PDL", "Unglue", "LoyalBooks", "Gutenberg", "Litteraturbanken"]);
        assert_eq!(sum(&nodes[&2].members, &matching), Some(1.5));
    }

    #[test]
    fn catalog_subjects_with_preferred_books_lead_after_filters_in_every_sort() {
        use library_model::{BrowseBookSort, BrowseChipSort, LibraryBrowseQuery};

        let nodes = BTreeMap::from([
            (0, SubjectNode { children: BTreeSet::from([1, 2, 3]), ..Default::default() }),
            (1, SubjectNode { label: "A popular".into(), members: BTreeSet::from([10]), ..Default::default() }),
            (2, SubjectNode { label: "Z preferred".into(), members: BTreeSet::from([11, 13]), ..Default::default() }),
            (3, SubjectNode { label: "M other".into(), members: BTreeSet::from([12]), ..Default::default() }),
        ]);
        let mut matching = MatchingBooks {
            books: BTreeSet::from([10, 11, 12, 13]),
            downloaded: BTreeSet::new(),
            text_matches: BTreeSet::new(),
            popularity: BTreeMap::from([(10, 100.0), (12, 20.0), (13, 10.0)]),
            preferred: BTreeSet::from([11]),
            source_priorities: BTreeMap::new(),
            popularity_groups: BTreeMap::new(),
        };
        let mut query = LibraryBrowseQuery {
            location: BrowseLocation::Catalog(0),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::DefaultOrder,
            book_sort: BrowseBookSort::DefaultOrder,
            hide_finished: false,
        };
        for sort in [BrowseChipSort::DefaultOrder, BrowseChipSort::Alphabetical, BrowseChipSort::Popularity, BrowseChipSort::BookCount] {
            query.chip_sort = sort;
            let options = BrowseOptions::from_query(&query, false);
            let rows = Database::subject_children(Books::Catalog, 0, &nodes, &matching, &options);
            assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Z preferred", "A popular", "M other"]);
        }

        // The same taxonomy still honors ordinary popularity in the library.
        query.chip_sort = BrowseChipSort::Popularity;
        let options = BrowseOptions::from_query(&query, false);
        let rows = Database::subject_children(Books::Library, 0, &nodes, &matching, &options);
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A popular", "M other", "Z preferred"]);

        // A section loses source preference when its preferred book is filtered
        // out, even if other books keep the section visible.
        matching.books.remove(&11);
        matching.preferred.clear();
        let rows = Database::subject_children(Books::Catalog, 0, &nodes, &matching, &options);
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A popular", "M other", "Z preferred"]);
    }

    #[test]
    fn subject_cards_navigate_by_id_and_breadcrumbs_follow_compacted_parents() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.fixtures_detach_curated_concept().unwrap();
        for id in 1..=6 {
            db.connection.fixtures_insert_book_6(&id).unwrap();
            db.connection.fixtures_insert_book_dir(&(sync_common::ROOT_DIR_ID.to_string()), &id, &(format!("book{id}.epub"))).unwrap();
            db.connection.fixtures_insert_book_unified_concept_2(&id, &(if id <= 3 { 3 } else { 4 })).unwrap();
        }
        let mut query = library_model::LibraryBrowseQuery {
            location: BrowseLocation::Subject(library_model::ROOT_SUBJECT_ROUTE_ID),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        let root = db.browse(&query, false).unwrap();
        assert!(root.path.is_empty());
        assert_eq!(root.contents.children[0].id, BrowseLocation::Subject(1));
        let science = &root.sections[0];
        assert_eq!(science.parent.id, BrowseLocation::Subject(1));
        assert_eq!(science.children.iter().map(|child| (child.id, child.name.as_str())).collect::<Vec<_>>(), [(BrowseLocation::Subject(4), "Optics"), (BrowseLocation::Subject(3), "Quantum")]);
        query.location = science.children[1].id;
        let quantum = db.browse(&query, false).unwrap();
        assert_eq!(quantum.contents.books.len(), 3);
        assert_eq!(quantum.path.iter().map(|segment| (segment.id, segment.name.as_str())).collect::<Vec<_>>(), [(BrowseLocation::Subject(1), "Science"), (BrowseLocation::Subject(3), "Quantum")]);
        db.connection.fixtures_update_curated_concept_preferred_label_fields().unwrap();
        let renamed = db.browse(&query, false).unwrap();
        assert_eq!(renamed.contents.books.len(), 3);
        assert_eq!(renamed.path.iter().map(|segment| (segment.id, segment.name.as_str())).collect::<Vec<_>>(), [(BrowseLocation::Subject(1), "1 / science"), (BrowseLocation::Subject(3), "Quantum / fields")]);
        query.location = BrowseLocation::Subject(library_model::ROOT_SUBJECT_ROUTE_ID);
        query.search = "fields".into();
        assert_eq!(db.browse(&query, false).unwrap().contents.children[0].id, BrowseLocation::Subject(3));
        assert!(db.connection.is_autocommit());
    }
}

#[cfg(test)]
use crate::sql_fixtures::FixturesSql;
