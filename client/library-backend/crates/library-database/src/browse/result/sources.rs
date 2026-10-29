//! Source adapters prepare identities and navigation without hydrating cards.
use super::*;
use crate::browse::{FoldersSql, parse_uuid_column};
use library_model::{BrowseBookSort, BrowseChipSort, HomeShelf};
use std::collections::HashSet;

impl Database {
    pub(super) fn prepare_browse_selection(&self, location: BrowseLocation, options: &BrowseOptions, catalog: Option<&Connection>) -> Result<Selection, DatabaseError> {
        match location {
            BrowseLocation::Folder(directory) => return self.prepare_folder_selection(directory, options),
            BrowseLocation::Home(shelf) => return self.prepare_home_selection(shelf, options),
            _ => {}
        }
        let source = if catalog.is_some() { Books::Catalog } else { Books::Library };
        let mut metadata = LibraryBrowseData::default();
        let mut sections = Vec::new();
        let searched = !options.search.is_empty();
        let (matching, direct) = match location {
            BrowseLocation::Author(author) | BrowseLocation::CatalogAuthor(author) => {
                let author_query = self.author_query(catalog, options)?;
                if author.is_none() {
                    let _stage = Stage::new("author heading aggregates");
                    metadata.contents.children = author_query.headings()?;
                } else if let Some(id) = author {
                    if let Some(name) = author_query.name(id)? {
                        let location = if catalog.is_some() { BrowseLocation::CatalogAuthor(Some(id)) } else { BrowseLocation::Author(Some(id)) };
                        metadata.path.push(BrowsePathSegment { id: location, name });
                    }
                }
                let direct_order = if author.is_some() || searched {
                    let _stage = Stage::new("author direct ordering");
                    author_query.ordered(author)?
                } else {
                    Vec::new()
                };
                (metadata.language_counts, metadata.format_counts) = match catalog {
                    Some(catalog) => self.catalog_facets(catalog, options)?,
                    None => self.library_facets(options)?,
                };
                return Ok(Selection { sections, metadata, books: SelectedBooks::Ordered { direct: direct_order.into_iter().map(Item::from).collect(), section_orders: Orders::new() } });
            }
            BrowseLocation::Subject(route) | BrowseLocation::Catalog(route) => {
                let nodes = match catalog {
                    Some(catalog) => self.catalog_subject_tree(catalog)?,
                    None => self.subject_tree_for(Books::Library)?,
                };
                let empty = BTreeSet::new();
                let matching = self.result_matching(catalog, nodes.get(&route).map_or(&empty, |node| &node.members), options)?;
                metadata.contents.children = Self::subject_children(source, route, &nodes, &matching, options);
                let direct = Self::subject_selected_books(route, &nodes, &matching, options);
                if !searched {
                    for parent in &metadata.contents.children {
                        let (BrowseLocation::Subject(child) | BrowseLocation::Catalog(child)) = parent.id else { continue };
                        let children = Self::subject_children(source, child, &nodes, &matching, options);
                        let books = Self::subject_selected_books(child, &nodes, &matching, options);
                        if !children.is_empty() || !books.is_empty() {
                            sections.push(SectionSelection { parent: parent.clone(), children, books });
                        }
                    }
                }
                metadata.path = crate::browse::subjects::subject_path(source, route, &nodes);
                if let Some((_, above)) = metadata.path.split_last() {
                    let unsearched = BrowseOptions { search: String::new(), ..options.clone() };
                    let root_matching = self.result_matching(catalog, nodes.get(&0).map_or(&empty, |node| &node.members), &unsearched)?;
                    for location in std::iter::once(source.location(0)).chain(above.iter().map(|row| row.id)) {
                        let (BrowseLocation::Subject(route) | BrowseLocation::Catalog(route)) = location else { continue };
                        metadata.ancestors.push((location, Self::subject_children(source, route, &nodes, &root_matching, &unsearched)));
                    }
                }
                (matching, direct)
            }
            BrowseLocation::Books => {
                let _stage = Stage::new("library book selection and ordering");
                let direct_order = self.author_query(None, options)?.all_ordered()?.into_iter().map(Item::from).collect();
                (metadata.language_counts, metadata.format_counts) = self.library_facets(options)?;
                return Ok(Selection { sections, metadata, books: SelectedBooks::Ordered { direct: direct_order, section_orders: Orders::new() } });
            }
            _ => return Err(DatabaseError::message("unsupported browse result scope")),
        };
        let _facets_stage = Stage::new("global facets");
        (metadata.language_counts, metadata.format_counts) = match catalog {
            Some(catalog) => self.catalog_facets(catalog, options)?,
            None => self.library_facets(options)?,
        };
        Ok(Selection { sections, metadata, books: SelectedBooks::Membership { eligible: matching.books, direct } })
    }

    fn prepare_home_selection(&self, shelf: Option<HomeShelf>, options: &BrowseOptions) -> Result<Selection, DatabaseError> {
        let prepared = self.home_shelves(options)?;
        let mut metadata = LibraryBrowseData::default();
        let mut sections = Vec::new();
        let mut section_orders = Orders::new();
        let mut direct = match shelf {
            Some(shelf) => prepared.shelves.iter().find(|found| found.shelf == shelf).map(|found| found.books.clone()).unwrap_or_default(),
            None if options.search.is_empty() => {
                for found in prepared.shelves.iter().filter(|found| !found.books.is_empty()) {
                    let parent = BrowseRow {
                        id: BrowseLocation::Home(Some(found.shelf)),
                        name: found.shelf.label().to_owned(),
                        path: None,
                        book_count: found.books.len(),
                        downloaded_book_count: found.books.iter().filter(|id| prepared.matching.downloaded.contains(id)).count(),
                    };
                    metadata.contents.children.push(parent.clone());
                    sections.push(SectionSelection { parent: parent.clone(), children: Vec::new(), books: found.books.iter().copied().collect() });
                    section_orders.insert(parent.id, Arc::new(found.books.iter().copied().map(Item::from).collect()));
                }
                Vec::new()
            }
            None => {
                let mut seen = HashSet::new();
                prepared.shelves.iter().flat_map(|found| found.books.iter().copied()).filter(|id| seen.insert(*id)).collect()
            }
        };
        if options.book_sort != BrowseBookSort::DefaultOrder {
            direct = self.ordered_books(&direct.iter().copied().collect(), options)?;
        }
        metadata.path = shelf.map(|shelf| BrowsePathSegment { id: BrowseLocation::Home(Some(shelf)), name: shelf.label().to_owned() }).into_iter().collect();
        (metadata.language_counts, metadata.format_counts) = self.library_facets(options)?;
        Ok(Selection { sections, metadata, books: SelectedBooks::Ordered { direct: direct.into_iter().map(Item::from).collect(), section_orders } })
    }

    fn prepare_folder_selection(&self, directory: DirId, options: &BrowseOptions) -> Result<Selection, DatabaseError> {
        let tree = self.folder_tree(&directory.to_string(), options)?;
        let scope = tree.scope(options);
        let mut metadata = LibraryBrowseData::default();
        metadata.contents.children = self.folder_children(scope, &tree.counts, options)?;
        let mut directories = scope.iter().cloned().collect::<BTreeSet<_>>();
        if options.search.is_empty() {
            directories.extend(metadata.contents.children.iter().filter_map(|row| row.id.folder()).map(|id| id.to_string()));
        }
        let mut placements = HashMap::<DirId, Vec<Item>>::new();
        self.connection.folder_placements(&serde_json::to_string(&directories).map_err(DatabaseError::operation)?, |row| {
            let directory = parse_uuid_column(row.get(1)?, 1)?;
            placements.entry(directory).or_default().push(Item { book: row.get(0)?, directory: Some(directory) });
            Ok(())
        })?;
        let with_books = placements.keys().copied().collect::<HashSet<_>>();
        let candidates = placements.values().flatten().map(|item| item.book).collect();
        let matching = self.matching_books(&candidates, options)?;
        let selected = if options.search.is_empty() { &matching.books } else { &matching.text_matches };
        let ordered = self.ordered_books(selected, options)?;
        let positions = ordered.iter().enumerate().map(|(position, &id)| (id, position)).collect::<HashMap<_, _>>();
        for items in placements.values_mut() {
            items.retain(|item| selected.contains(&item.book));
            items.sort_unstable_by_key(|item| (positions[&item.book], item.directory));
        }
        let scope = scope.iter().map(|id| id.parse::<DirId>().map_err(DatabaseError::operation)).collect::<Result<HashSet<_>, _>>()?;
        let mut direct = placements.iter().filter(|(directory, _)| scope.contains(directory)).flat_map(|(_, items)| items.iter().copied()).collect::<Vec<_>>();
        direct.sort_unstable_by_key(|item| (positions[&item.book], item.directory));
        let mut sections = Vec::new();
        let mut wrappers = Vec::new();
        let mut section_orders = Orders::new();
        if options.search.is_empty() {
            for parent in &metadata.contents.children {
                let Some(directory) = parent.id.folder() else { continue };
                let children = self.folder_children(&[directory.to_string()], &tree.counts, options)?;
                let items = placements.remove(&directory).unwrap_or_default();
                let books = items.iter().map(|item| item.book).collect();
                section_orders.insert(parent.id, Arc::new(items));
                let wrapper = !with_books.contains(&directory) && !children.is_empty();
                let section = SectionSelection { parent: parent.clone(), children, books };
                if wrapper {
                    wrappers.push(section);
                } else {
                    sections.push(section);
                }
            }
            sections.extend(wrappers);
            if options.chip_sort == BrowseChipSort::Popularity {
                let positions = metadata.contents.children.iter().enumerate().map(|(index, row)| (row.id, index)).collect::<HashMap<_, _>>();
                sections.sort_by_key(|section| positions[&section.parent.id]);
            }
        }
        metadata.path = self.browse_directory_path(&directory)?;
        metadata.ancestors = self.folder_ancestors(&metadata.path, options)?;
        (metadata.language_counts, metadata.format_counts) = self.library_facets(options)?;
        Ok(Selection { sections, metadata, books: SelectedBooks::Ordered { direct, section_orders } })
    }
}
