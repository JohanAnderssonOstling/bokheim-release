//! The catalog's side of subject and author browsing: filters, order, cards
//! and facets queried from the catalog SQLite projection, by book row ID.
//! Catalog books have no reading state; external entries open their source page.
#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[cfg(test)]
use book_model::AuthorId;
use content_address::BookId;
use library_model::{BookCardRow, BookDetail, BookSource, CardKey, LibraryFormatCount, LibraryLanguageCount};
#[cfg(test)]
use library_model::{BrowseContents, BrowseLocation, BrowsePageRequest, BrowsePathSegment, LibraryBrowseQuery};

use super::BooksSql;
#[cfg(test)]
use super::LibraryBrowseData;
#[cfg(test)]
use super::authors::{Author, index_rows, page_author_rows};
use super::books::MatchingBooks;
use super::facets::{add_format_count, include_selected_facets};
use super::options::{BrowseOptions, file_type_of};
use super::rows::invalid_integer_column;
#[cfg(test)]
use super::sections::SectionSelection;
#[cfg(test)]
use super::subjects::Books;
use crate::catalog::CatalogBook;
use crate::{Database, DatabaseError};
use rusqlite::Connection;

impl Database {
    /// The hashes of the library's visible books.
    pub(super) fn library_book_hashes(&self) -> Result<HashSet<BookId>, DatabaseError> {
        let mut hashes = HashSet::new();
        self.connection.library_book_hashes(|row| {
            hashes.insert(row.get(0)?);
            Ok(())
        })?;
        Ok(hashes)
    }

    /// The catalog by author, as the library's Authors page: the index holds
    /// a card per author with their books, an author their books alone.
    #[cfg(test)]
    pub(super) fn browse_catalog_author(&self, author: Option<AuthorId>, query: &LibraryBrowseQuery, downloaded_only: bool, section_limit: Option<usize>, page: Option<BrowsePageRequest>) -> Result<LibraryBrowseData, DatabaseError> {
        let options = BrowseOptions::from_query(query, downloaded_only);
        let authors = load_authors()?;
        let matching = matching(&candidates(author, &authors), &self.library_book_hashes()?, &options)?;
        let searched = !options.search.is_empty();
        let mut children = if author.is_none() { index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id))) } else { Vec::new() };
        let headings = if author.is_none() { page_author_rows(&mut children, page, !searched) } else { None };
        let direct = if author.is_some() || searched { if searched { matching.text_matches.clone() } else { matching.books.clone() } } else { BTreeSet::new() };
        let selections = if author.is_none() && !searched {
            children
                .iter()
                .filter_map(|parent| {
                    let BrowseLocation::CatalogAuthor(Some(id)) = parent.id else { return None };
                    Some(SectionSelection { parent: parent.clone(), children: Vec::new(), books: authors[&id].books.intersection(&matching.books).copied().collect() })
                })
                .collect()
        } else {
            Vec::new()
        };
        let card_page = if headings.is_some() { page.map(|page| BrowsePageRequest { section_offset: 0, ..page }) } else { page };
        let (books, sections, mut state) = self.load_section_cards(Books::Catalog, &direct, selections, &options, section_limit, card_page)?;
        if let (Some(state), Some(request)) = (&mut state, page) {
            state.section_offset = request.section_offset;
            state.headings = headings;
        }
        let page = state;
        let contents = BrowseContents { children, books };
        let (language_counts, format_counts) = facets(&options)?;
        let path = author.and_then(|id| authors.get(&id).map(|found| BrowsePathSegment { id: BrowseLocation::CatalogAuthor(Some(id)), name: found.name.clone() })).into_iter().collect();
        Ok(LibraryBrowseData { contents, sections, path, ancestors: Vec::new(), language_counts, format_counts, feed: None, page })
    }

    #[cfg(test)]
    pub(super) fn browse_catalog_author_contents(&self, author: Option<AuthorId>, query: &LibraryBrowseQuery, downloaded_only: bool) -> Result<BrowseContents, DatabaseError> {
        let options = BrowseOptions::from_query(query, downloaded_only);
        let authors = load_authors()?;
        let matching = matching(&candidates(author, &authors), &self.library_book_hashes()?, &options)?;
        author_contents(author, &authors, &matching, &options)
    }

    #[cfg(test)]
    pub(super) fn browse_catalog_author_book_page(&self, author: Option<AuthorId>, query: &LibraryBrowseQuery, downloaded_only: bool, offset: usize, limit: usize) -> Result<library_model::BrowseBookPage, DatabaseError> {
        self.with_browse_snapshot(|| {
            let options = BrowseOptions::from_query(query, downloaded_only);
            let authors = load_authors()?;
            let matching = matching(&candidates(author, &authors), &self.library_book_hashes()?, &options)?;
            let selected = if options.search.is_empty() { &matching.books } else { &matching.text_matches };
            self.load_source_book_page(Books::Catalog, selected, &options, offset, limit)
        })
    }
}

/// Every author credited on the catalog's books. A credit joins the author
/// that shares its identity or its name: sources such as Litteraturbanken
/// give each book's authors identities of their own, so the identity alone
/// would list one writer once per book. Names are compared as import compares
/// provisional authors, setting aside accents, honorifics and surname-first
/// order. An author files under the smallest spelling credited to them, as in
/// the library, and under the first identity met. A book with no credited
/// author is under none.
#[cfg(test)]
fn catalog_authors(books: &[CatalogBook]) -> BTreeMap<AuthorId, Author> {
    let mut authors = BTreeMap::<AuthorId, Author>::new();
    let mut by_identity = HashMap::<AuthorId, AuthorId>::new();
    let mut by_name = HashMap::<String, AuthorId>::new();
    for (index, book) in books.iter().enumerate() {
        for credit in book.record.authors() {
            let keys = metadata_contract::matching::author_match_keys(credit.name());
            let id = by_identity.get(&credit.agent_id()).or_else(|| keys.iter().find_map(|key| by_name.get(key))).copied().unwrap_or(credit.agent_id());
            by_identity.insert(credit.agent_id(), id);
            for key in keys {
                by_name.entry(key).or_insert(id);
            }
            let author = authors.entry(id).or_insert_with(|| Author { name: credit.name().to_owned(), books: BTreeSet::new() });
            if credit.name() < author.name.as_str() {
                author.name = credit.name().to_owned();
            }
            author.books.insert(index as i64);
        }
    }
    authors
}

/// The books an author page considers: one author's, or every credited book.
#[cfg(test)]
fn candidates(author: Option<AuthorId>, authors: &BTreeMap<AuthorId, Author>) -> BTreeSet<i64> {
    match author {
        Some(id) => authors.get(&id).map(|found| found.books.clone()).unwrap_or_default(),
        None => authors.values().flat_map(|found| found.books.iter().copied()).collect(),
    }
}

/// The index holds authors and, under a search, the books that match it; an
/// author holds their books and nothing below them.
#[cfg(test)]
fn author_contents(author: Option<AuthorId>, authors: &BTreeMap<AuthorId, Author>, matching: &MatchingBooks, options: &BrowseOptions) -> Result<BrowseContents, DatabaseError> {
    let searched = !options.search.is_empty();
    let none = BTreeSet::new();
    let (children, books) = match author {
        Some(_) => (Vec::new(), if searched { &matching.text_matches } else { &matching.books }),
        None => (index_rows(authors, matching, options, |id| BrowseLocation::CatalogAuthor(Some(id))), if searched { &matching.text_matches } else { &none }),
    };
    Ok(BrowseContents { children, books: cards(books, options)? })
}

/// The catalog lists only what the library does not hold: a book in `held`
/// matches nothing, so it leaves the cards, the counts and the sections alike.
#[cfg(test)]
pub(super) fn matching(candidates: &BTreeSet<i64>, held: &HashSet<BookId>, options: &BrowseOptions) -> Result<MatchingBooks, DatabaseError> {
    crate::catalog::read(|db| matching_in(db, candidates, held, options))
}

pub(super) fn matching_in(db: &Connection, candidates: &BTreeSet<i64>, held: &HashSet<BookId>, options: &BrowseOptions) -> Result<MatchingBooks, DatabaseError> {
    let mut result =
        MatchingBooks { books: BTreeSet::new(), downloaded: BTreeSet::new(), text_matches: BTreeSet::new(), popularity: BTreeMap::new(), preferred: BTreeSet::new(), source_priorities: BTreeMap::new(), popularity_groups: BTreeMap::new() };
    let ids = serde_json::to_string(candidates).map_err(DatabaseError::operation)?;
    let held = serde_json::to_string(&held.iter().map(ToString::to_string).collect::<Vec<_>>()).map_err(DatabaseError::operation)?;
    db.catalog_browse_matching(&ids, &held, options.file_types, &options.languages, &options.search, |row| {
        let id = row.get(0)?;
        result.books.insert(id);
        if !options.search.is_empty() && row.get::<_, bool>(1)? {
            result.text_matches.insert(id);
        }
        if row.get::<_, bool>(2)? {
            result.preferred.insert(id);
        }
        if options.needs_popularity() {
            result.source_priorities.insert(id, row.get(3)?);
            if let Some(score) = row.get::<_, Option<f64>>(4)? {
                result.popularity.insert(id, score);
                if let Some(group) = row.get::<_, Option<String>>(5)? {
                    result.popularity_groups.insert(id, group);
                }
            }
        }
        Ok(())
    })?;
    Ok(result)
}

#[cfg(test)]
fn load_authors() -> Result<BTreeMap<AuthorId, Author>, DatabaseError> {
    crate::catalog::read(load_authors_in)
}

#[cfg(test)]
pub(super) fn load_authors_in(db: &Connection) -> Result<BTreeMap<AuthorId, Author>, DatabaseError> {
    let mut authors = BTreeMap::<AuthorId, Author>::new();
    db.catalog_browse_authors(|row| {
        let identity: Vec<u8> = row.get(0)?;
        let id = AuthorId::from_slice(&identity).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(error)))?;
        authors.entry(id).or_insert(Author { name: row.get(1)?, books: BTreeSet::new() }).books.insert(row.get(2)?);
        Ok(())
    })?;
    Ok(authors)
}

#[cfg(test)]

fn matching_books(books: &[CatalogBook], candidates: &BTreeSet<i64>, held: &HashSet<BookId>, options: &BrowseOptions) -> MatchingBooks {
    let mut matching = BTreeSet::new();
    let mut text_matches = BTreeSet::new();
    let mut popularity = BTreeMap::new();
    let mut preferred = BTreeSet::new();
    let mut source_priorities = BTreeMap::new();
    for &index in candidates {
        let Some(book) = usize::try_from(index).ok().and_then(|index| books.get(index)) else { continue };
        let format = options.file_types == 0 || options.file_types & book.format() as i32 != 0;
        let language = options.languages.is_empty() || book.languages.iter().any(|language| options.languages.contains(&format!(",{language},")));
        // Signing out limits library browsing to downloaded books. Catalog
        // downloads are public and remain available without an account.
        if !format || !language || book.hash.is_some_and(|hash| held.contains(&hash)) {
            continue;
        }
        matching.insert(index);
        if book.is_preferred_source() {
            preferred.insert(index);
        }
        if options.needs_popularity() {
            source_priorities.insert(index, book.source_priority());
            if let Some(score) = popularity_score(book) {
                popularity.insert(index, score);
            }
        }
        if !options.search.is_empty() && (book.search_title.contains(&options.search) || book.search_author.contains(&options.search)) {
            text_matches.insert(index);
        }
    }
    MatchingBooks { books: matching, downloaded: BTreeSet::new(), text_matches, popularity, preferred, source_priorities, popularity_groups: BTreeMap::new() }
}

#[cfg(test)]
fn popularity_score(book: &CatalogBook) -> Option<f64> {
    book.popularity.as_ref()?.score()
}

/// Preferred-source books first, then the requested order within each group.
#[cfg(test)]
pub(super) fn cards(selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<BookCardRow>, DatabaseError> {
    crate::catalog::read(|db| cards_in(db, selected, options))
}

#[cfg(test)]
pub(super) fn card_rows(selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<(i64, BookCardRow)>, DatabaseError> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    crate::catalog::read(|db| card_rows_in(db, selected, options))
}

#[cfg(test)]
pub(super) fn ordered_books(selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<i64>, DatabaseError> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    crate::catalog::read(|db| ordered_books_in(db, selected, options))
}

pub(super) fn ordered_books_in(db: &Connection, selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<i64>, DatabaseError> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(selected).map_err(DatabaseError::operation)?;
    let mut ordered = Vec::with_capacity(selected.len());
    db.catalog_browse_ordered(&ids, i32::from(options.book_sort == library_model::BrowseBookSort::Popularity), |row| {
        ordered.push(row.get(0)?);
        Ok(())
    })?;
    Ok(ordered)
}

#[cfg(test)]
fn cards_in(db: &Connection, selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<BookCardRow>, DatabaseError> {
    Ok(card_rows_in(db, selected, options)?.into_iter().map(|(_, card)| card).collect())
}

pub(super) fn card_rows_in(db: &Connection, selected: &BTreeSet<i64>, options: &BrowseOptions) -> Result<Vec<(i64, BookCardRow)>, DatabaseError> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let mut cards = Vec::new();
    let ids = serde_json::to_string(selected).map_err(DatabaseError::operation)?;
    db.catalog_browse_cards(&ids, i32::from(options.book_sort == library_model::BrowseBookSort::Popularity), |row| {
        let hash: Option<BookId> = row.get(2)?;
        let key: BookId = row.get(1)?;
        cards.push((
            row.get(0)?,
            BookCardRow {
                source: hash.map_or(BookSource::ExternalCatalog { entry: CardKey::from(key), downloadable: row.get(7)? }, |content_hash| BookSource::Catalog { content_hash }),
                title: row.get(3)?,
                subtitle: row.get(4)?,
                author: row.get(5)?,
                progress: 0.0,
                downloaded: false,
                download_requested: false,
                format_category: file_type_of(row.get(6)?),
                audiobook_duration_ms: row.get::<_, Option<i64>>(8)?.map(u64::try_from).transpose().map_err(|error| invalid_integer_column(8, error.to_string()))?,
                added_at: 0,
                source_directory: None,
            },
        ));
        Ok(())
    })?;
    Ok(cards)
}

#[cfg(test)]

fn ordered_cards(books: &[CatalogBook], selected: &BTreeSet<i64>, options: &BrowseOptions) -> Vec<BookCardRow> {
    let mut chosen = selected.iter().filter_map(|&index| usize::try_from(index).ok().and_then(|index| books.get(index))).collect::<Vec<_>>();
    chosen.sort_by(|left, right| {
        let preferred = right.is_preferred_source().cmp(&left.is_preferred_source());
        let popularity = if options.book_sort == library_model::BrowseBookSort::Popularity {
            left.source_priority().cmp(&right.source_priority()).then_with(|| super::popularity::compare(popularity_score(left), popularity_score(right)))
        } else {
            std::cmp::Ordering::Equal
        };
        preferred.then(popularity).then_with(|| left.search_title.cmp(&right.search_title)).then_with(|| left.title.cmp(&right.title)).then_with(|| left.key.cmp(&right.key))
    });
    chosen.into_iter().map(card).collect()
}

fn card(book: &CatalogBook) -> BookCardRow {
    BookCardRow {
        source: book.hash.map_or(BookSource::ExternalCatalog { entry: book.key, downloadable: book.audiobook_download_url().is_some() }, |content_hash| BookSource::Catalog { content_hash }),
        title: book.title.clone(),
        subtitle: book.subtitle.clone(),
        author: book.author.clone(),
        progress: 0.0,
        downloaded: false,
        download_requested: false,
        format_category: file_type_of(Some(book.format() as i64)),
        audiobook_duration_ms: book.audiobook_duration_ms,
        added_at: 0,
        source_directory: None,
    }
}

/// The page of a catalog book the library does not hold: its record, with
/// nothing about reading or placement.
pub(super) fn detail(hash: &BookId) -> Result<Option<BookDetail>, DatabaseError> {
    crate::catalog::entry(CardKey::from(hash)).map(|book| book.filter(|book| book.hash.is_some()).map(|book| book_detail(&book)))
}

impl Database {
    pub fn external_catalog_detail(&self, entry: CardKey) -> Result<BookDetail, DatabaseError> {
        let book = crate::catalog::entry(entry)?.filter(|book| book.hash.is_none()).ok_or_else(|| DatabaseError::message("the entry is not in the catalog"))?;
        self.with_catalog_subjects(book_detail(&book))
    }

    /// A catalog book's detail with the subjects the catalog's subject pages
    /// list it under.
    pub(super) fn with_catalog_subjects(&self, mut detail: BookDetail) -> Result<BookDetail, DatabaseError> {
        if let Some(row) = crate::catalog::browse_row(detail.book.key())? {
            detail.subjects = self.book_subjects(super::subjects::Books::Catalog, row)?;
        }
        Ok(detail)
    }
}

fn book_detail(book: &CatalogBook) -> BookDetail {
    BookDetail {
        book: card(book),
        external_url: book.external_url.clone(),
        audiobook_chapter_count: None,
        description: book.record.description.clone(),
        subjects: Vec::new(),
        year: book.record.book.year,
        source_directory_id: None,
        path: Vec::new(),
        authors: Vec::new(),
        credits: Vec::new(),
        current_entry: None,
        current_target: None,
        chapter_read_status: Default::default(),
        toc: book.toc.clone(),
    }
}

/// Language and format counts over the whole catalog, as the library's are
/// over the whole library.
#[cfg(test)]
pub(super) fn facets(options: &BrowseOptions) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), DatabaseError> {
    crate::catalog::read(|db| facets_in(db, options))
}

pub(super) fn facets_in(db: &Connection, options: &BrowseOptions) -> Result<(Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>), DatabaseError> {
    let mut languages = Vec::new();
    let mut formats = Vec::new();
    db.catalog_browse_languages(|row| {
        languages.push(LibraryLanguageCount { language: row.get(0)?, book_count: row.get(1)? });
        Ok(())
    })?;
    db.catalog_browse_formats(|row| {
        add_format_count(&mut formats, row.get(0)?, row.get(1)?);
        Ok(())
    })?;
    include_selected_facets(&mut languages, &mut formats, &options.languages, options.file_types);
    Ok((languages, formats))
}

include_sqlite_sql::include_sql!("src/browse/sql/catalog.sql");

#[cfg(test)]

fn book_facets(books: &[CatalogBook], options: &BrowseOptions) -> (Vec<LibraryLanguageCount>, Vec<LibraryFormatCount>) {
    let mut languages = BTreeMap::<&str, u32>::new();
    let mut format_counts = Vec::new();
    for book in books {
        add_format_count(&mut format_counts, Some(book.format() as i64), 1);
        for language in &book.languages {
            *languages.entry(language).or_default() += 1;
        }
    }
    let mut language_counts = languages.into_iter().map(|(language, book_count)| LibraryLanguageCount { language: language.to_owned(), book_count }).collect();
    include_selected_facets(&mut language_counts, &mut format_counts, &options.languages, options.file_types);
    (language_counts, format_counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{BookDetails, BookFormat, BookRecord, Contributor, MarcRelatorCode};
    use library_model::{BrowseLocation, LibraryBrowseQuery, LibraryFileTypeFilter, ROOT_SUBJECT_ROUTE_ID};
    use rusqlite::params;

    #[test]
    fn sql_matching_agrees_with_book_filters_and_keeps_external_entries_when_the_library_holds_their_key() {
        let mut books = (1..=4).map(|id| authored_book(id, Vec::new())).collect::<Vec<_>>();
        books[0].hash = Some(BookId::from_bytes(books[0].key.0));
        books[1].hash = Some(BookId::from_bytes(books[1].key.0));
        books[2].file_format = BookFormat::Mp3Folder;
        for (index, book) in books.iter_mut().enumerate() {
            book.languages = vec![if index % 2 == 0 { "en".into() } else { "sv".into() }];
        }
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("../catalog/schema.sql")).unwrap();
        for (index, book) in books.iter().enumerate() {
            db.execute(
                "INSERT INTO catalog_browse_book(row_id,key,external,format,search_title,search_author,source_priority,popularity_score) VALUES(?1,?2,?3,?4,?5,?6,?7,NULL)",
                params![index as i64, &book.key.0[..], book.hash.is_none(), book.format() as i32, book.search_title, book.search_author, book.source_priority()],
            )
            .unwrap();
            for language in &book.languages {
                db.execute("INSERT INTO catalog_browse_language VALUES(?1,?2)", params![index as i64, language]).unwrap();
            }
        }
        let candidates = BTreeSet::from([0, 1, 2, 3]);
        for held in [HashSet::new(), HashSet::from([books[0].hash.unwrap(), BookId::from_bytes(books[2].key.0)])] {
            for languages in [vec![], vec!["en".into()], vec!["sv".into()]] {
                for file_types in [vec![], vec![LibraryFileTypeFilter::Book], vec![LibraryFileTypeFilter::Audiobook]] {
                    for search in ["", "book 2", "missing"] {
                        let query = LibraryBrowseQuery {
                            location: BrowseLocation::Catalog(0),
                            search: search.into(),
                            file_types: file_types.clone(),
                            languages: languages.clone(),
                            chip_sort: Default::default(),
                            book_sort: Default::default(),
                            hide_finished: false,
                        };
                        let options = BrowseOptions::from_query(&query, false);
                        let actual = matching_in(&db, &candidates, &held, &options).unwrap();
                        let expected = matching_books(&books, &candidates, &held, &options);
                        assert_eq!(actual.books, expected.books);
                        assert_eq!(actual.text_matches, expected.text_matches);
                        assert_eq!(actual.preferred, expected.preferred);
                    }
                }
            }
        }
        let options = BrowseOptions::from_query(
            &LibraryBrowseQuery {
                location: BrowseLocation::Catalog(0),
                search: String::new(),
                file_types: vec![],
                languages: vec![],
                chip_sort: Default::default(),
                book_sort: Default::default(),
                hide_finished: false,
            },
            false,
        );
        assert!(matching_in(&db, &BTreeSet::new(), &HashSet::new(), &options).unwrap().books.is_empty());
    }

    fn authored_book(number: u8, contributors: Vec<Contributor>) -> CatalogBook {
        let record = BookRecord { metadata: BookDetails { title: format!("Book {number}"), contributors, ..Default::default() }, description: String::new() };
        let author = record.authors().map(|credit| credit.name()).collect::<Vec<_>>().join(", ");
        CatalogBook {
            hash: None,
            key: CardKey([number; 32]),
            external_url: None,
            download_url: None,
            file_format: BookFormat::Epub,
            audiobook_duration_ms: None,
            sources: Vec::new(),
            title: record.title.clone(),
            subtitle: None,
            search_title: format!("book {number}"),
            search_author: book_model::normalize_search_text(&author),
            author,
            languages: Vec::new(),
            concepts: Vec::new(),
            record,
            toc: Vec::new(),
            popularity: None,
        }
    }

    fn credit(number: u128, name: &str, role: &[u8; 3]) -> Contributor {
        Contributor::with_id(AuthorId::from_u128(number), name, MarcRelatorCode(*role)).unwrap()
    }

    #[test]
    fn composite_catalog_authors_use_individual_credits_and_exclude_non_authors() {
        let mut books =
            vec![authored_book(1, vec![credit(1, "Alex Smith, Jamie Jones, Terry White", b"aut"), credit(2, "Alex Smith", b"aut"), credit(3, "Jamie Jones", b"aut"), credit(4, "Terry White", b"edt"), credit(5, "Untyped Credit", b"ctb")])];
        crate::catalog::normalize_author_lists(&mut books);
        assert_eq!(books[0].record.authors().map(|author| author.name()).collect::<Vec<_>>(), ["Alex Smith", "Jamie Jones"]);
        assert_eq!(books[0].author, "Alex Smith, Jamie Jones");
        assert!(books[0].record.contributors.contains(&credit(4, "Terry White", b"edt")));
        let authors = catalog_authors(&books);
        assert_eq!(authors.len(), 2);
        for id in [AuthorId::from_u128(2), AuthorId::from_u128(3)] {
            assert_eq!(candidates(Some(id), &authors), BTreeSet::from([0]));
        }
        for id in [AuthorId::from_u128(4), AuthorId::from_u128(5)] {
            assert!(candidates(Some(id), &authors).is_empty());
        }
        assert_eq!(candidates(None, &authors), BTreeSet::from([0]));
        let query = LibraryBrowseQuery {
            location: BrowseLocation::CatalogAuthor(None),
            search: "Jamie".into(),
            file_types: Vec::new(),
            languages: Vec::new(),
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &candidates(None, &authors), &HashSet::new(), &options);
        assert_eq!(matching.text_matches, BTreeSet::from([0]));
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Jamie Jones");
        assert_eq!(rows[0].book_count, 1);
    }

    #[test]
    fn independently_named_coauthors_resolve_legacy_lists_and_share_the_book() {
        let mut books = vec![authored_book(1, vec![credit(1, "Jacob Grimm, Wilhelm Grimm", b"aut")]), authored_book(2, vec![credit(2, "Jacob Grimm", b"aut")]), authored_book(3, vec![credit(3, "Wilhelm Grimm", b"aut")])];
        crate::catalog::normalize_author_lists(&mut books);
        let authors = catalog_authors(&books);
        assert_eq!(authors.len(), 2);
        assert_eq!(candidates(Some(AuthorId::from_u128(2)), &authors), BTreeSet::from([0, 1]));
        assert_eq!(candidates(Some(AuthorId::from_u128(3)), &authors), BTreeSet::from([0, 2]));
    }

    #[test]
    fn surname_first_names_and_unresolved_lists_remain_intact() {
        let mut books = vec![
            authored_book(1, vec![credit(1, "Berger, Henning", b"aut")]),
            authored_book(2, vec![credit(2, "Henning Berger", b"aut")]),
            authored_book(3, vec![credit(3, "Alex Smith, Unknown Writer", b"aut"), credit(4, "Alex Smith", b"aut")]),
        ];
        let before = books.iter().map(|book| book.record.clone()).collect::<Vec<_>>();
        crate::catalog::normalize_author_lists(&mut books);
        assert_eq!(books.iter().map(|book| book.record.clone()).collect::<Vec<_>>(), before);
        let authors = catalog_authors(&books);
        assert_eq!(authors[&AuthorId::from_u128(1)].books, BTreeSet::from([0, 1]));
    }

    #[test]
    fn source_popularity_orders_cards_and_authors_and_counts_each_format() {
        use crate::browse::popularity::sum;
        use crate::catalog::CatalogPopularity;
        use library_model::{BrowseBookSort, BrowseChipSort};

        let mut books = (1..=9).map(|id| authored_book(id, vec![credit(id as u128, &format!("Writer {id}"), b"aut")])).collect::<Vec<_>>();
        let sources = ["gutenberg.org", "loyalbooks.com", "unglue.it", "litteraturbanken.se", "publicdomainlibrary.org", "gutenberg.org", "loyalbooks.com", "gutenberg.org", "standardebooks.org"];
        for (book, (source, score)) in books.iter_mut().zip(sources.into_iter().zip([Some(10000.0), Some(1.1), Some(1.0), Some(0.0), None, Some(0.0), Some(1.1), None, None])) {
            book.sources = vec![source.into()];
            book.popularity = score.map(|source_score| CatalogPopularity { source_score: Some(source_score), source: Some(source.into()), ..Default::default() });
            book.languages = vec!["en".into()];
        }
        // One LoyalBooks page offers two formats to the same author.
        books[6].record.contributors = books[1].record.contributors.clone();
        books[6].file_format = BookFormat::Mp3Folder;
        // A replacement also retains its older provenance. Its best source
        // still places it above lower-tier books with known popularity.
        books[8].sources.push("gutenberg.org".into());
        let query = LibraryBrowseQuery {
            location: BrowseLocation::CatalogAuthor(None),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::Popularity,
            book_sort: BrowseBookSort::Popularity,
            hide_finished: false,
        };
        let authors = catalog_authors(&books);
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &candidates(None, &authors), &HashSet::new(), &options);
        assert_eq!(ordered_cards(&books, &matching.books, &options).iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Book 5", "Book 9", "Book 4", "Book 3", "Book 2", "Book 7", "Book 1", "Book 6", "Book 8"]);
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Writer 5", "Writer 9", "Writer 4", "Writer 3", "Writer 2", "Writer 1", "Writer 6", "Writer 8"]);
        assert_eq!(sum(&BTreeSet::from([1, 6]), &matching), Some(2.2));

        // Filtering to audio counts only the audio edition.
        let audio_options = BrowseOptions::from_query(&LibraryBrowseQuery { file_types: vec![library_model::LibraryFileTypeFilter::Audiobook], ..query }, false);
        let audio = matching_books(&books, &candidates(None, &authors), &HashSet::new(), &audio_options);
        assert_eq!(sum(&BTreeSet::from([1, 6]), &audio), Some(1.1));
        assert_eq!(audio.books, BTreeSet::from([6]));
    }

    #[test]
    fn catalog_popularity_sorts_known_scores_and_filtered_author_totals_with_unknowns() {
        use crate::catalog::{CatalogPopularity, CombinedPopularity};
        use library_model::{BrowseBookSort, BrowseChipSort};
        let mut books = vec![
            authored_book(1, vec![credit(1, "A writer", b"aut")]),
            authored_book(2, vec![credit(2, "Z writer", b"aut")]),
            authored_book(3, vec![credit(2, "Z writer", b"aut"), credit(2, "Z writer", b"aut")]),
            authored_book(4, vec![credit(4, "Unknown", b"aut")]),
            authored_book(5, vec![credit(5, "Zero", b"aut")]),
        ];
        for (book, score) in books.iter_mut().zip([Some(80.0), Some(60.0), Some(30.0), None, Some(0.0)]) {
            book.popularity = score.map(|score| CatalogPopularity {
                popularity: Some(1),
                epub_popularity: Some(1),
                combined_popularity: Some(CombinedPopularity { score: Some(score), reading_percentile: Some(score), epub_percentile: Some(score) }),
                ..Default::default()
            });
            book.languages = vec!["en".into()];
        }
        books[2].languages = vec!["sv".into()];
        let authors = catalog_authors(&books);
        let mut query = LibraryBrowseQuery {
            location: BrowseLocation::CatalogAuthor(None),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::Popularity,
            book_sort: BrowseBookSort::Popularity,
            hide_finished: false,
        };
        let candidates = candidates(None, &authors);
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &candidates, &HashSet::new(), &options);
        assert_eq!(ordered_cards(&books, &matching.books, &options).iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Book 1", "Book 2", "Book 3", "Book 5", "Book 4"]);
        assert!(!matching.popularity.contains_key(&3));
        assert_eq!(matching.popularity[&4], 0.0);
        assert!(books[3].popularity.is_none());
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Z writer", "A writer", "Zero", "Unknown"]);
        assert_eq!(rows[0].book_count, 2);
        query.languages = vec!["en".into()];
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &candidates, &HashSet::new(), &options);
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A writer", "Z writer", "Zero", "Unknown"]);

        // Held books contribute no score to totals.
        let hash = BookId::from_bytes([2; 32]);
        books[1].hash = Some(hash);
        let matching = matching_books(&books, &candidates, &HashSet::from([hash]), &options);
        assert!(!matching.popularity.contains_key(&1));
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A writer", "Zero", "Unknown"]);

        // Alphabetical order still ignores popularity.
        query.book_sort = BrowseBookSort::Alphabetical;
        let options = BrowseOptions::from_query(&query, false);
        assert_eq!(ordered_cards(&books, &matching.books, &options).iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Book 1", "Book 4", "Book 5"]);
    }

    #[test]
    fn catalog_popularity_partial_evidence_stays_unknown_with_alphabetical_ties() {
        use crate::catalog::{CatalogPopularity, CombinedPopularity};
        use library_model::{BrowseBookSort, BrowseChipSort};

        let mut books = vec![authored_book(3, vec![]), authored_book(1, vec![]), authored_book(2, vec![])];
        books[0].popularity = Some(CatalogPopularity { popularity: Some(0), epub_popularity: None, combined_popularity: None, ..Default::default() });
        books[2].popularity =
            Some(CatalogPopularity { popularity: Some(1), epub_popularity: Some(1), combined_popularity: Some(CombinedPopularity { score: Some(50.0), reading_percentile: Some(50.0), epub_percentile: Some(50.0) }), ..Default::default() });
        let evidence = books.iter().map(|book| book.popularity.clone()).collect::<Vec<_>>();
        let query = LibraryBrowseQuery {
            location: BrowseLocation::Catalog(ROOT_SUBJECT_ROUTE_ID),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::Popularity,
            book_sort: BrowseBookSort::Popularity,
            hide_finished: false,
        };
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &BTreeSet::from([0, 1, 2]), &HashSet::new(), &options);
        assert_eq!(ordered_cards(&books, &matching.books, &options).iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Book 2", "Book 1", "Book 3"]);
        assert_eq!(books.iter().map(|book| book.popularity.clone()).collect::<Vec<_>>(), evidence);
    }

    #[test]
    fn preferred_source_precedes_every_sort_and_author_sections_respect_filters() {
        use crate::catalog::{CatalogPopularity, CombinedPopularity};
        use library_model::{BrowseBookSort, BrowseChipSort};

        let mut books = vec![
            authored_book(1, vec![credit(1, "A other", b"aut")]),
            authored_book(2, vec![credit(2, "Z preferred", b"aut")]),
            authored_book(3, vec![credit(3, "M unknown", b"aut")]),
            authored_book(4, vec![credit(2, "Z preferred", b"aut")]),
        ];
        for book in &mut books {
            book.languages = vec!["en".into()];
        }
        books[3].languages = vec!["sv".into()];
        books[0].sources = vec!["litteraturbanken.se".into()];
        books[1].sources = vec!["publicdomainlibrary.org".into()];
        books[2].sources = vec!["loyalbooks.com".into()];
        books[3].sources = vec!["loyalbooks.com".into(), "publicdomainlibrary.org".into()];
        for (index, score) in [(0, 100.0), (3, 0.0)] {
            books[index].popularity = Some(CatalogPopularity {
                popularity: Some(1),
                epub_popularity: Some(1),
                combined_popularity: Some(CombinedPopularity { score: Some(score), reading_percentile: Some(score), epub_percentile: Some(score) }),
                ..Default::default()
            });
        }
        let authors = catalog_authors(&books);
        let candidates = candidates(None, &authors);
        let mut query = LibraryBrowseQuery {
            location: BrowseLocation::CatalogAuthor(None),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: BrowseChipSort::DefaultOrder,
            book_sort: BrowseBookSort::DefaultOrder,
            hide_finished: false,
        };
        for (book_sort, chip_sort, expected) in [
            (BrowseBookSort::DefaultOrder, BrowseChipSort::DefaultOrder, ["Book 2", "Book 4", "Book 1", "Book 3"]),
            (BrowseBookSort::Alphabetical, BrowseChipSort::Alphabetical, ["Book 2", "Book 4", "Book 1", "Book 3"]),
            (BrowseBookSort::Popularity, BrowseChipSort::Popularity, ["Book 4", "Book 2", "Book 1", "Book 3"]),
        ] {
            query.book_sort = book_sort;
            query.chip_sort = chip_sort;
            let options = BrowseOptions::from_query(&query, false);
            let matching = matching_books(&books, &candidates, &HashSet::new(), &options);
            assert_eq!(matching.preferred, BTreeSet::from([1, 3]));
            assert_eq!(ordered_cards(&books, &matching.books, &options).iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), expected);
            let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
            assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["Z preferred", "A other", "M unknown"]);
        }
        assert!(books[1].popularity.is_none());

        query.languages = vec!["en".into()];
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &candidates, &HashSet::new(), &options);
        assert_eq!(matching.preferred, BTreeSet::from([1]));
        assert_eq!(ordered_cards(&books, &matching.books, &options).iter().map(|book| book.title.as_str()).collect::<Vec<_>>(), ["Book 2", "Book 1", "Book 3"]);

        let hash = BookId::from_bytes([2; 32]);
        books[1].hash = Some(hash);
        let matching = matching_books(&books, &candidates, &HashSet::from([hash]), &options);
        assert!(matching.preferred.is_empty());
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(), ["A other", "M unknown"]);
    }

    #[test]
    fn catalog_format_filters_separate_hosted_books_source_epubs_and_audio() {
        let hash = BookId::new(&"2".repeat(64));
        let mut books = vec![
            authored_book(1, vec![credit(1, "Mixed writer", b"aut")]),
            authored_book(2, vec![credit(2, "Book writer", b"aut")]),
            authored_book(3, vec![credit(1, "Mixed writer", b"aut")]),
            authored_book(4, vec![credit(3, "Audio writer", b"aut")]),
        ];
        books[0].hash = Some(hash);
        for (book, language) in books.iter_mut().zip(["en", "sv", "en", "sv"]) {
            book.languages = vec![language.into()];
        }
        for book in &mut books[2..] {
            book.file_format = BookFormat::Mp3Folder;
        }
        books[1].download_url = Some("https://source.example/book.epub".into());
        books[2].download_url = Some("https://archive.org/download/recording/audio.zip".into());
        assert!(matches!(card(&books[1]).source, BookSource::ExternalCatalog { downloadable: false, .. }));
        assert!(matches!(card(&books[2]).source, BookSource::ExternalCatalog { downloadable: true, .. }));
        assert!(matches!(card(&books[3]).source, BookSource::ExternalCatalog { downloadable: false, .. }));
        let authors = catalog_authors(&books);
        let candidates = candidates(None, &authors);
        let mut query = LibraryBrowseQuery {
            location: BrowseLocation::CatalogAuthor(None),
            search: String::new(),
            file_types: vec![],
            languages: vec![],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        for downloaded_only in [false, true] {
            for (filters, expected) in [
                (vec![], BTreeSet::from([0, 1, 2, 3])),
                (vec![LibraryFileTypeFilter::Book], BTreeSet::from([0, 1])),
                (vec![LibraryFileTypeFilter::Audiobook], BTreeSet::from([2, 3])),
                (vec![LibraryFileTypeFilter::Book, LibraryFileTypeFilter::Audiobook], BTreeSet::from([0, 1, 2, 3])),
                (vec![LibraryFileTypeFilter::Pdf], BTreeSet::new()),
                (vec![LibraryFileTypeFilter::All, LibraryFileTypeFilter::Pdf], BTreeSet::from([0, 1, 2, 3])),
            ] {
                query.file_types = filters;
                let options = BrowseOptions::from_query(&query, downloaded_only);
                let matching = matching_books(&books, &candidates, &HashSet::new(), &options);
                assert_eq!(matching.books, expected);
                let cards = ordered_cards(&books, &matching.books, &options);
                assert_eq!(cards.iter().filter(|card| card.format_category == LibraryFileTypeFilter::Audiobook).count(), expected.intersection(&BTreeSet::from([2, 3])).count());
            }
        }
        query.file_types = vec![LibraryFileTypeFilter::Audiobook];
        let options = BrowseOptions::from_query(&query, false);
        let matching = matching_books(&books, &candidates, &HashSet::new(), &options);
        let rows = index_rows(&authors, &matching, &options, |id| BrowseLocation::CatalogAuthor(Some(id)));
        assert_eq!(rows.iter().map(|row| (row.name.as_str(), row.book_count)).collect::<Vec<_>>(), [("Audio writer", 1), ("Mixed writer", 1)]);

        query.languages = vec!["en-US".into()];
        query.search = "book 3".into();
        let options = BrowseOptions::from_query(&query, true);
        let matching = matching_books(&books, &candidates, &HashSet::from([hash]), &options);
        assert_eq!(matching.books, BTreeSet::from([2]));
        assert_eq!(matching.text_matches, BTreeSet::from([2]));

        query.file_types = vec![LibraryFileTypeFilter::Book];
        assert!(matching_books(&books, &candidates, &HashSet::from([hash]), &BrowseOptions::from_query(&query, true)).books.is_empty());
    }

    #[test]
    fn catalog_format_facets_count_each_category_and_keep_selected_empty_filters() {
        let mut books = vec![authored_book(1, vec![]), authored_book(2, vec![]), authored_book(3, vec![])];
        books[0].hash = Some(BookId::new(&"3".repeat(64)));
        books[2].download_url = Some("https://archive.org/download/recording/audio.zip".into());
        books[2].file_format = BookFormat::Mp3Folder;
        let query = LibraryBrowseQuery {
            location: BrowseLocation::Catalog(ROOT_SUBJECT_ROUTE_ID),
            search: String::new(),
            file_types: vec![LibraryFileTypeFilter::Pdf],
            languages: vec![],
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        let options = BrowseOptions::from_query(&query, false);
        let counts = book_facets(&books, &options).1;
        assert_eq!(counts.iter().map(|count| (count.format, count.book_count)).collect::<Vec<_>>(), [(LibraryFileTypeFilter::Book, 2), (LibraryFileTypeFilter::Pdf, 0), (LibraryFileTypeFilter::Audiobook, 1)]);
        let counts = book_facets(&[], &options).1;
        assert_eq!(counts.iter().map(|count| (count.format, count.book_count)).collect::<Vec<_>>(), [(LibraryFileTypeFilter::Pdf, 0)]);
    }

    #[test]
    fn signed_out_catalog_browsing_keeps_public_books_and_respects_filters() {
        let hash = BookId::new(&"1".repeat(64));
        let mut books = [CatalogBook {
            hash: Some(hash),
            key: CardKey::from(hash),
            external_url: None,
            download_url: None,
            file_format: BookFormat::Epub,
            audiobook_duration_ms: None,
            sources: Vec::new(),
            title: "Public book".into(),
            subtitle: None,
            author: "Author".into(),
            search_title: "public book".into(),
            search_author: "author".into(),
            languages: vec!["en".into()],
            concepts: Vec::new(),
            record: BookRecord { metadata: BookDetails::default(), description: String::new() },
            toc: Vec::new(),
            popularity: None,
        }];
        let mut query = LibraryBrowseQuery {
            location: BrowseLocation::Catalog(ROOT_SUBJECT_ROUTE_ID),
            search: "public".into(),
            file_types: Vec::new(),
            languages: Vec::new(),
            chip_sort: Default::default(),
            book_sort: Default::default(),
            hide_finished: false,
        };
        let candidates = BTreeSet::from([0]);
        let held = HashSet::new();
        for downloaded_only in [false, true] {
            let matches = matching_books(&books, &candidates, &held, &BrowseOptions::from_query(&query, downloaded_only));
            assert_eq!(matches.books, candidates);
            assert_eq!(matches.text_matches, candidates);
            assert!(matches.downloaded.is_empty());
        }

        let options = BrowseOptions::from_query(&query, true);
        assert!(matching_books(&books, &candidates, &HashSet::from([hash]), &options).books.is_empty());
        query.languages = vec!["sv".into()];
        assert!(matching_books(&books, &candidates, &held, &BrowseOptions::from_query(&query, true)).books.is_empty());
        query.languages = vec!["en-US".into()];
        assert_eq!(matching_books(&books, &candidates, &held, &BrowseOptions::from_query(&query, true)).books, candidates);
        books[0].hash = None;
        books[0].external_url = Some("https://litteraturbanken.se/book/info".into());
        query.languages = vec!["en".into()];
        let external = card(&books[0]);
        assert!(matches!(external.source, BookSource::ExternalCatalog { .. }));
        assert!(external.content_hash().is_none());
        assert_eq!(matching_books(&books, &candidates, &HashSet::from([hash]), &BrowseOptions::from_query(&query, true)).books, candidates);
        query.file_types = vec![LibraryFileTypeFilter::Pdf];
        assert!(matching_books(&books, &candidates, &held, &BrowseOptions::from_query(&query, true)).books.is_empty());
    }
}
