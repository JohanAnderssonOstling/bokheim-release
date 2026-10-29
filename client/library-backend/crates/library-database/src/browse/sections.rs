//! Shared section selection, ordering, and bounded card loading.
#[cfg(test)]
use super::{BrowseOptions, subjects::Books};
#[cfg(test)]
use crate::{Database, DatabaseError};
use library_model::BrowseRow;
#[cfg(test)]
use library_model::{BookCardRow, BrowseBookPage, BrowsePageRequest, BrowsePageState, BrowseSection};
use std::collections::BTreeSet;
#[cfg(test)]
use std::collections::HashMap;

pub(super) struct SectionSelection {
    pub parent: BrowseRow,
    pub children: Vec<BrowseRow>,
    pub books: BTreeSet<i64>,
}

#[cfg(test)]
impl Database {
    pub(super) fn ordered_source_books(&self, source: Books, selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<i64>, DatabaseError> {
        let ordered = match source {
            Books::Library => self.ordered_books(selected, options),
            Books::Catalog => super::catalog::ordered_books(selected, options),
        }?;
        if ordered.len() != selected.len() {
            return Err(DatabaseError::message("browse data changed while ordering books"));
        }
        Ok(ordered)
    }

    fn source_cards(&self, source: Books, selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<HashMap<i64, BookCardRow>, DatabaseError> {
        let cards: HashMap<i64, BookCardRow> = match source {
            Books::Library => {
                let ids = selected.iter().copied().collect::<Vec<_>>();
                ids.iter().copied().zip(self.load_cards(&ids)?).collect()
            }
            Books::Catalog => super::catalog::card_rows(selected, options)?.into_iter().collect(),
        };
        if cards.len() != selected.len() {
            return Err(DatabaseError::message("browse data changed while loading book cards"));
        }
        Ok(cards)
    }

    /// Sort IDs once, then load only the union of the first section pages and
    /// the page's own direct books. Shared books need one database card read.
    pub(super) fn load_section_cards(
        &self, source: Books, direct: &BTreeSet<i64>, sections: Vec<SectionSelection>, options: &BrowseOptions, limit: Option<usize>, page: Option<BrowsePageRequest>,
    ) -> Result<(Vec<BookCardRow>, Vec<BrowseSection>, Option<BrowsePageState>), DatabaseError> {
        let state = page.map(|page| BrowsePageState {
            section_ids: sections.iter().map(|section| section.parent.id).collect(),
            section_offset: page.section_offset,
            book_offset: page.book_offset,
            book_total: direct.len(),
            headings: None,
            result_revision: None,
        });
        let sections = if let Some(page) = page { sections.into_iter().skip(page.section_offset).take(page.section_limit).collect() } else { sections };
        let mut selected = direct.clone();
        for section in &sections {
            selected.extend(&section.books);
        }
        let ordered = self.ordered_source_books(source, &selected, options)?;
        let positions: HashMap<_, _> = ordered.iter().enumerate().map(|(index, &id)| (id, index)).collect();
        let mut plans = Vec::with_capacity(sections.len());
        let direct_ids: Vec<_> = ordered.iter().copied().filter(|id| direct.contains(id)).collect();
        let direct_ids = if let Some(page) = page { direct_ids.into_iter().skip(page.book_offset).take(page.book_limit).collect::<Vec<_>>() } else { direct_ids };
        let mut loaded = direct_ids.iter().copied().collect::<BTreeSet<_>>();
        for section in sections {
            let total = section.books.len();
            let mut ids = section.books.into_iter().collect::<Vec<_>>();
            ids.sort_unstable_by_key(|id| positions[id]);
            if let Some(limit) = limit {
                ids.truncate(limit);
            }
            loaded.extend(&ids);
            plans.push((section.parent, section.children, ids, total));
        }
        let cards = self.source_cards(source, &loaded, options)?;
        let direct = direct_ids.into_iter().map(|id| cards[&id].clone()).collect();
        let sections = plans.into_iter().map(|(parent, children, ids, total)| BrowseSection { parent, children, books: ids.into_iter().map(|id| cards[&id].clone()).collect(), book_count: limit.map(|_| total) }).collect();
        Ok((direct, sections, state))
    }

    pub(super) fn load_source_book_page(&self, source: Books, selected: &BTreeSet<i64>, options: &BrowseOptions, offset: usize, limit: usize) -> Result<BrowseBookPage, DatabaseError> {
        let ordered = self.ordered_source_books(source, selected, options)?;
        let total = ordered.len();
        let ids = ordered.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        let cards = self.source_cards(source, &ids.iter().copied().collect(), options)?;
        Ok(BrowseBookPage { books: ids.into_iter().map(|id| cards[&id].clone()).collect(), offset, total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql_fixtures::FixturesSql;
    use library_model::{BrowseBookSort, BrowseChipSort, BrowseLocation, LibraryBrowseQuery};
    use rusqlite::params;

    #[test]
    fn section_pages_preserve_order_filters_counts_and_complete_recursive_contents() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.fixtures_detach_curated_concept_2().unwrap();
        let author = uuid::Uuid::from_u128(1).to_string();
        db.connection.execute("INSERT INTO author_identity(id,stable_id,name) VALUES(1,?1,'Writer')", [&author]).unwrap();
        for id in 1..=9 {
            db.connection.fixtures_insert_book_6(&id).unwrap();
            db.connection.fixtures_insert_book_dir_2(&sync_common::ROOT_DIR_ID.to_string(), &id, &format!("book{id}.epub"), &(id % 2)).unwrap();
            db.connection.fixtures_insert_book_language_2(&id, &(if id % 2 == 0 { "sv" } else { "en" })).unwrap();
            db.connection.fixtures_insert_book_unified_concept_2(&id, &(if id <= 6 { 4 } else { 3 })).unwrap();
            db.connection.execute("INSERT INTO book_contributor(book_row_id,position,author_identity_id,name,role) VALUES(?1,0,1,'Writer',?2)", params![id, b"aut".as_slice()]).unwrap();
            db.connection.execute("UPDATE book SET popularity_rating_count=?1,read_at=?1,read_progress=?2 WHERE row_id=?1", params![id, if id == 9 { 1.0 } else { 0.0 }]).unwrap();
        }
        for location in [BrowseLocation::Subject(0), BrowseLocation::Author(None)] {
            for sort in [BrowseBookSort::DefaultOrder, BrowseBookSort::Alphabetical, BrowseBookSort::Popularity, BrowseBookSort::RecentlyRead] {
                for languages in [vec![], vec!["en".into()], vec!["sv".into()]] {
                    for downloaded_only in [false, true] {
                        let mut query = LibraryBrowseQuery {
                            location,
                            search: String::new(),
                            file_types: vec![],
                            languages: languages.clone(),
                            chip_sort: BrowseChipSort::Popularity,
                            book_sort: sort,
                            hide_finished: true,
                        };
                        let full = db.browse(&query, downloaded_only).unwrap();
                        assert_windows(&db, &query, downloaded_only);
                        let first = db.browse_sections(&query, downloaded_only, 2).unwrap();
                        assert_eq!(first.contents, full.contents);
                        assert_eq!(first.sections.len(), full.sections.len());
                        for (page, complete) in first.sections.iter().zip(full.sections.iter()) {
                            assert_eq!(page.parent, complete.parent);
                            assert_eq!(page.children, complete.children);
                            assert_eq!(page.book_count, Some(complete.books.len()));
                            assert_eq!(page.books, complete.books[..complete.books.len().min(2)]);
                            query.location = page.parent.id;
                            assert_windows(&db, &query, downloaded_only);
                            let mut concatenated = Vec::new();
                            for offset in (0..complete.books.len()).step_by(2) {
                                let page = db.browse_section_books(&query, downloaded_only, offset, 2).unwrap();
                                assert_eq!(page.offset, offset);
                                assert_eq!(page.total, complete.books.len());
                                concatenated.extend(page.books);
                            }
                            assert_eq!(concatenated, complete.books);
                            assert_eq!(db.browse_contents(&query, downloaded_only).unwrap().books, complete.books);
                            let past = db.browse_section_books(&query, downloaded_only, usize::MAX, usize::MAX).unwrap();
                            assert!(past.books.is_empty());
                            assert_eq!(past.total, complete.books.len());
                        }
                    }
                }
            }
        }
    }

    fn assert_windows(db: &Database, query: &LibraryBrowseQuery, downloaded_only: bool) {
        let full = db.browse_sections(query, downloaded_only, library_model::BROWSE_SECTION_BOOK_LIMIT).unwrap();
        let first = db.browse_page(query, downloaded_only, BrowsePageRequest { section_limit: 1, book_limit: 0, ..Default::default() }).unwrap();
        let heading_state = first.page.unwrap().headings;
        let total = heading_state.as_ref().map_or(full.sections.len(), |headings| headings.total);
        let mut sections = Vec::new();
        let mut headings = Vec::new();
        for offset in 0..total {
            let page = db.browse_page(query, downloaded_only, BrowsePageRequest { section_offset: offset, section_limit: 1, book_offset: 0, book_limit: 0 }).unwrap();
            let state = page.page.unwrap();
            let expected_ids = if heading_state.is_some() { page.sections.iter().map(|section| section.parent.id).collect::<Vec<_>>() } else { full.sections.iter().map(|section| section.parent.id).collect::<Vec<_>>() };
            assert_eq!(state.section_ids, expected_ids);
            assert_eq!(state.headings, heading_state);
            assert_eq!(state.section_offset, offset);
            if heading_state.is_some() {
                assert_eq!(page.contents.children, full.contents.children[offset..offset + 1]);
                headings.extend(page.contents.children);
            } else {
                assert_eq!(page.contents.children, full.contents.children);
            }
            assert_eq!(page.language_counts, full.language_counts);
            assert_eq!(page.format_counts, full.format_counts);
            assert!(page.contents.books.is_empty());
            sections.extend(page.sections);
        }
        assert_eq!(sections, full.sections);
        if heading_state.is_some() {
            assert_eq!(headings, full.contents.children);
        }
        let mut books = Vec::new();
        for offset in (0..full.contents.books.len()).step_by(2) {
            let page = db.browse_page(query, downloaded_only, BrowsePageRequest { section_offset: 0, section_limit: 0, book_offset: offset, book_limit: 2 }).unwrap();
            assert!(page.sections.is_empty());
            assert_eq!(page.page.unwrap().book_total, full.contents.books.len());
            books.extend(page.contents.books);
        }
        assert_eq!(books, full.contents.books);
        let past = db.browse_page(query, downloaded_only, BrowsePageRequest { section_offset: usize::MAX, section_limit: usize::MAX, book_offset: usize::MAX, book_limit: usize::MAX }).unwrap();
        assert!(past.sections.is_empty());
        assert!(past.contents.books.is_empty());
        if heading_state.is_some() {
            assert!(past.contents.children.is_empty());
        }
    }

    #[test]
    fn library_author_headings_page_counts_filters_search_and_revision() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        for id in 1..=37 {
            let author = uuid::Uuid::from_u128(id as u128).to_string();
            db.connection.execute("INSERT INTO author_identity(id,stable_id,name) VALUES(?1,?2,?3)", params![id, author, format!("Writer {id:02}")]).unwrap();
            db.connection.fixtures_insert_book_6(&id).unwrap();
            db.connection.fixtures_insert_book_dir_2(&sync_common::ROOT_DIR_ID.to_string(), &id, &format!("book{id}.epub"), &(id % 2)).unwrap();
            db.connection.fixtures_insert_book_language_2(&id, &(if id % 2 == 0 { "sv" } else { "en" })).unwrap();
            db.connection.execute("INSERT INTO book_contributor(book_row_id,position,author_identity_id,name,role) VALUES(?1,0,?1,?2,?3)", params![id, format!("Writer {id:02}"), b"aut".as_slice()]).unwrap();
        }
        let mut query = LibraryBrowseQuery {
            location: BrowseLocation::Author(None),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::Alphabetical,
            book_sort: BrowseBookSort::Alphabetical,
            hide_finished: false,
        };
        let first = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
        assert_eq!(first.contents.children.len(), 24);
        assert_eq!(first.sections.len(), 24);
        assert_eq!(first.page.as_ref().unwrap().section_ids.len(), 24);
        assert_eq!(first.page.as_ref().unwrap().heading_count(), 37);
        let revision = first.page.unwrap().headings.unwrap().revision;
        for sort in [BrowseChipSort::Alphabetical, BrowseChipSort::BookCount, BrowseChipSort::Popularity] {
            query.chip_sort = sort;
            for search in ["", "writer", "writer 3", "missing"] {
                query.search = search.into();
                for languages in [vec![], vec!["en".into()], vec!["sv".into()]] {
                    query.languages = languages;
                    for downloaded in [false, true] {
                        assert_windows(&db, &query, downloaded);
                    }
                }
            }
        }
        query.search.clear();
        query.languages.clear();
        query.chip_sort = BrowseChipSort::Alphabetical;
        db.connection.execute("UPDATE book_contributor SET name='Changed author' WHERE author_identity_id=37", []).unwrap();
        let updated = db.browse_page(&query, false, BrowsePageRequest::default()).unwrap();
        assert_eq!(updated.page.as_ref().unwrap().heading_count(), 37);
        assert_ne!(updated.page.unwrap().headings.unwrap().revision, revision);
    }

    #[test]
    fn direct_grids_and_search_page_without_truncating_recursive_operations() {
        let db = Database::open(":memory:").unwrap();
        db.initialize_library().unwrap();
        db.connection.fixtures_detach_curated_concept_2().unwrap();
        for id in 1..=9 {
            db.connection.fixtures_insert_book_6(&id).unwrap();
            db.connection.fixtures_insert_book_dir_2(&sync_common::ROOT_DIR_ID.to_string(), &id, &format!("book{id}.epub"), &1).unwrap();
            db.connection.fixtures_insert_book_unified_concept_2(&id, &4).unwrap();
        }
        for location in [BrowseLocation::Books, BrowseLocation::Subject(0), BrowseLocation::Subject(4)] {
            for search in ["", "book"] {
                let query = LibraryBrowseQuery {
                    location,
                    search: search.into(),
                    file_types: vec![],
                    languages: vec![],
                    chip_sort: BrowseChipSort::Alphabetical,
                    book_sort: BrowseBookSort::Alphabetical,
                    hide_finished: false,
                };
                assert_windows(&db, &query, false);
                let first = db.browse_page(&query, false, BrowsePageRequest { book_limit: 2, ..Default::default() }).unwrap();
                assert!(first.contents.books.len() <= 2);
                if location == BrowseLocation::Books {
                    assert_eq!(db.browse_contents(&query, false).unwrap().books.len(), 9);
                    assert_eq!(first.page.unwrap().book_total, 9);
                }
            }
        }
    }
}
