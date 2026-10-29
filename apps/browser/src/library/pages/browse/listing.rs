//! What a browse page lists — the folders or subjects under one location, and
//! the books with them.

use library_backend::LibraryBrowseData;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::{App, Entity, SharedString};
use library_model::{BrowseBookSort, BrowseChipSort, BrowseLocation, BrowsePageRequest, BrowsePageState, BrowsePathSegment, BrowseRow, CardKey};
use sync_common::{DirId, ROOT_DIR_ID};
use ui_components as components;

use super::controls::BrowseControls;
use crate::library::services::LibraryContext;
use crate::library::widgets::{BookCardEvents, BookCardView};
use crate::{LibraryRoute, Route};

/// What a browse page lists. Folders are real directories, with the
/// commands that go with them; subjects and authors are queries over the
/// library, Books is the flat library, and Home is its shelves of recent activity.
/// Catalog is the public-domain catalog's subject tree: books not in the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BrowseKind {
    Home,
    Books,
    Folder,
    Subject,
    Author,
    Catalog,
}

impl BrowseKind {
    pub(super) fn route(self) -> LibraryRoute {
        match self {
            Self::Home => LibraryRoute::Home,
            Self::Books => LibraryRoute::Books,
            Self::Folder => LibraryRoute::Folders,
            Self::Subject => LibraryRoute::Subjects,
            Self::Author => LibraryRoute::Authors,
            Self::Catalog => LibraryRoute::Catalog,
        }
    }

    pub(super) fn root_location(self) -> BrowseLocation {
        // The catalog shown by author starts at `CatalogAuthor(None)`
        // instead; the navigator decides which.
        match self {
            Self::Home => BrowseLocation::Home(None),
            Self::Books => BrowseLocation::Books,
            Self::Folder => BrowseLocation::Folder(ROOT_DIR_ID),
            Self::Subject => BrowseLocation::Subject(library_model::ROOT_SUBJECT_ROUTE_ID),
            Self::Author => BrowseLocation::Author(None),
            Self::Catalog => BrowseLocation::Catalog(library_model::ROOT_SUBJECT_ROUTE_ID),
        }
    }

    pub(super) fn root_label(self) -> SharedString {
        Route::Library(self.route()).presentation().0.into()
    }

    /// Prefixes the element ids the page hands out.
    pub(super) fn list_prefix(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Books => "books",
            Self::Folder => "folder",
            Self::Subject => "subject",
            Self::Author => "author",
            Self::Catalog => "catalog",
        }
    }

    /// The graph a kind can be shown as, and the layout it opens in: only the
    /// trees have one. Books, Home's shelves and the author index are flat.
    pub(super) fn graph_layout(self) -> Option<components::GraphLayoutKind> {
        if cfg!(feature = "kobo") {
            return None;
        }
        match self {
            Self::Folder => Some(components::GraphLayoutKind::Columns),
            Self::Subject | Self::Catalog => Some(components::GraphLayoutKind::AdaptiveRadial),
            Self::Home | Self::Author | Self::Books => None,
        }
    }

    pub(super) fn child_icon(self) -> components::NavigationIcon {
        Route::Library(self.route()).presentation().1
    }
}

/// A browsable panel with the immediate children and directly assigned books
/// of its parent location.
#[derive(Clone)]
pub(crate) struct BrowseSection {
    pub(crate) parent: BrowseRow,
    pub(crate) children: Vec<BrowseRow>,
    pub(crate) books: Vec<Entity<BookCardView>>,
    pub(crate) book_count: Option<usize>,
}

/// Resolved display items carry their placement and card, not layout indices.
#[derive(Clone)]
pub(super) enum BrowseItem {
    Child { row: BrowseRow, parent: Option<DirId> },
    Book(Entity<BookCardView>),
}

impl BrowseItem {
    pub(super) fn child(&self) -> Option<&BrowseRow> {
        match self {
            Self::Child { row, .. } => Some(row),
            Self::Book(_) => None,
        }
    }
    pub(super) fn book(&self) -> Option<&Entity<BookCardView>> {
        match self {
            Self::Book(book) => Some(book),
            Self::Child { .. } => None,
        }
    }

    pub(super) fn id(&self, cx: &App) -> SharedString {
        match self {
            Self::Child { row, .. } => chip_id(row.id),
            Self::Book(book) => book_id(book.read(cx).key(), book.read(cx).source_directory()),
        }
    }
}

pub(super) fn chip_id(id: BrowseLocation) -> SharedString {
    format!("chip:{id}").into()
}

pub(super) fn book_id(key: CardKey, directory: Option<DirId>) -> SharedString {
    match directory {
        Some(directory) => format!("book:{key}:placement:{directory}").into(),
        None => format!("book:{key}").into(),
    }
}
pub(super) type BrowseItems = Rc<HashMap<SharedString, BrowseItem>>;

/// The listed items among `ids`, in their order.
pub(super) fn resolve_items(ids: &[SharedString], items: &HashMap<SharedString, BrowseItem>) -> Vec<BrowseItem> {
    ids.iter().filter_map(|id| items.get(id).cloned()).collect()
}

/// Route and displayed ancestry travel together, including missing routes
/// whose backend trail is empty. Requests cannot install one without the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BrowseDestination {
    pub(super) location: BrowseLocation,
    pub(super) path: Vec<BrowsePathSegment>,
}

impl BrowseDestination {
    /// `root` is where an empty path leads: the kind's root, or for the
    /// catalog shown by author, its author index.
    pub(super) fn from_path(root: BrowseLocation, path: Vec<BrowsePathSegment>) -> Self {
        let location = path.last().map_or(root, |crumb| crumb.id);
        Self { location, path }
    }
}

/// Whether a chip still has books to bring onto this device. The browser
/// streams books it does not hold instead of downloading them, and catalog
/// and feed books are added to the library, not downloaded.
pub(super) fn can_download(child: &BrowseRow) -> bool {
    cfg!(not(target_arch = "wasm32")) && !matches!(child.id, BrowseLocation::Catalog(_) | BrowseLocation::CatalogAuthor(_) | BrowseLocation::Opds { .. } | BrowseLocation::Store(_)) && child.downloaded_book_count < child.book_count
}

/// The count a chip or card row shows. A feed gives a link's count only
/// sometimes, and one it does not give arrives as 0; a feed itself has none.
pub(super) fn shown_count(child: &BrowseRow) -> Option<usize> {
    (!matches!(child.id, BrowseLocation::Opds { .. } | BrowseLocation::Store(_)) || child.book_count > 0).then_some(child.book_count)
}

/// Whether a chip carries the download mark: only when it is partly on this
/// device. Which state is the common one differs from device to device, and on
/// one that holds almost nothing a mark on every remote chip would be on every
/// chip. A partly downloaded one is the exception either way — something begun
/// and not finished. A wholly remote one is downloaded from its menu instead.
pub(super) fn shows_download_mark(child: &BrowseRow) -> bool {
    child.downloaded_book_count > 0 && can_download(child)
}

/// The listing a page has installed: one location's children, sections and
/// books, and the controls its facets allow. Book cards outlive a reload, so a
/// book still in the listing keeps its entity and can reuse a decoded cover
/// while it remains in the shared cover cache.
pub(super) struct BrowseListing {
    ui: LibraryContext,
    kind: BrowseKind,
    pub(super) controls: BrowseControls,
    pub(super) children: Rc<Vec<BrowseRow>>,
    pub(super) sections: Vec<BrowseSection>,
    pub(super) books: Rc<Vec<Entity<BookCardView>>>,
    /// The children of the locations above this one, for the graph.
    pub(super) ancestors: Vec<(BrowseLocation, Vec<BrowseRow>)>,
    /// Every child and book, by the id its paginator child carries.
    pub(super) items: BrowseItems,
    /// A feed page's next page, appended to this listing when the grid reaches its end.
    pub(super) next: Option<BrowseLocation>,
    /// The feed refused this device; the page asks to sign in.
    pub(super) sign_in: bool,
    /// How the book cards reach the page.
    book_events: BookCardEvents,
    folder_commands: bool,
    pub(super) revision: u64,
    pub(super) page: Option<BrowsePageState>,
    pub(super) heading_rows: BTreeMap<usize, BrowseRow>,
    pub(super) book_indices: BTreeMap<usize, Entity<BookCardView>>,
}

impl BrowseListing {
    /// `folder_commands` offers copying, moving and removing books on their
    /// cards, for a listing of real folders.
    pub(super) fn new(ui: LibraryContext, kind: BrowseKind, book_events: BookCardEvents, folder_commands: bool) -> Self {
        Self {
            ui,
            kind,
            controls: Self::controls_for(kind, BrowseControls::library()),
            children: Rc::new(Vec::new()),
            sections: Vec::new(),
            books: Rc::new(Vec::new()),
            ancestors: Vec::new(),
            items: Rc::new(HashMap::new()),
            next: None,
            sign_in: false,
            book_events,
            folder_commands,
            revision: 0,
            page: None,
            heading_rows: BTreeMap::new(),
            book_indices: BTreeMap::new(),
        }
    }

    fn controls_for(kind: BrowseKind, mut controls: BrowseControls) -> BrowseControls {
        if kind == BrowseKind::Books {
            controls.collection_sorts.clear();
        }
        // Catalog books have no reading history to sort by. The catalog can
        // be shown by subject or by author.
        if kind == BrowseKind::Catalog {
            controls.book_sorts.retain(|sort| *sort != BrowseBookSort::RecentlyRead);
            controls.hide_finished = false;
            controls.groupings = true;
        }
        // Home keeps activity order by default; sorting changes presentation,
        // while its shelves still select books by their original activity rules.
        if kind == BrowseKind::Home {
            controls.collection_sorts = vec![BrowseChipSort::DefaultOrder, BrowseChipSort::Alphabetical, BrowseChipSort::BookCount, BrowseChipSort::Popularity];
            controls.book_sorts = vec![BrowseBookSort::DefaultOrder, BrowseBookSort::Alphabetical, BrowseBookSort::Popularity];
        }
        controls
    }

    /// Installs a response, and hands back the trail the library says leads to
    /// it, where it knows one.
    /// `searched` says whether `data` answers a search.
    pub(super) fn install(&mut self, data: LibraryBrowseData, searched: bool, cx: &mut App) -> Vec<BrowsePathSegment> {
        self.revision = self.revision.wrapping_add(1);
        let LibraryBrowseData { contents, sections, path, ancestors, language_counts, format_counts, feed, page } = data;
        self.ancestors = ancestors;
        // A search lists its matches flat; the sections are the unsearched
        // listing's, and the page lays its children out as cards only if any.
        // A feed page has no sections: its navigation links are chips, as
        // only the page itself is fetched. The catalog root's feeds and
        // stores have no section either, and stay chips beside the sections;
        // a child the sections leave out is otherwise not listed.
        let sections = if searched { Vec::new() } else { sections };
        self.page = page;
        self.heading_rows = self.page.as_ref().filter(|page| page.headings.is_some()).map(|page| (page.section_offset..).zip(contents.children.iter().filter(|row| is_author_heading(row.id)).cloned()).collect()).unwrap_or_default();
        let children = if searched || feed.is_some() || self.page.as_ref().is_some_and(|page| page.headings.is_some()) {
            contents.children
        } else if let Some(page) = &self.page {
            let section_ids: HashSet<_> = page.section_ids.iter().copied().collect();
            contents.children.into_iter().filter(|child| section_ids.contains(&child.id) || matches!(child.id, BrowseLocation::Opds { .. } | BrowseLocation::Store(_))).collect()
        } else {
            let feeds = contents.children.into_iter().filter(|child| matches!(child.id, BrowseLocation::Opds { page: None, .. } | BrowseLocation::Store(_)));
            sections.iter().map(|section| section.parent.clone()).chain(feeds).collect()
        };
        let books = contents.books;
        (self.controls, self.next, self.sign_in) = match feed {
            Some(feed) => (BrowseControls::feed(feed.facets), feed.next, feed.sign_in),
            None => (Self::controls_for(self.kind, BrowseControls::library_with_facets(&language_counts, &format_counts)), None, false),
        };
        self.children = Rc::new(children);
        // A card outlives a reload wherever its book is still listed — in the
        // grid or in a section card — so a refresh keeps every decoded cover
        // rather than drawing each book blank until its cover loads again. A
        // library change reloads the listing, and during a scan that is often.
        // Pixel ownership stays in the bounded shared cache, not these cards.
        let mut previous: HashMap<(CardKey, Option<DirId>), Vec<Entity<BookCardView>>> = HashMap::new();
        for card in self.books.iter().chain(self.sections.iter().flat_map(|section| section.books.iter())) {
            previous.entry((card.read(cx).key(), card.read(cx).source_directory())).or_default().push(card.clone());
        }
        let mut card_for = |book: library_model::BookCardRow, cx: &mut App| match previous.get_mut(&(book.key(), book.source_directory)).and_then(Vec::pop) {
            Some(card) => {
                card.update(cx, |card, cx| card.replace_row(book, cx));
                card
            }
            None => BookCardView::create_in_grid(book, self.ui.clone(), self.book_events.clone(), self.folder_commands, self.kind == BrowseKind::Books, cx),
        };
        let sections = sections
            .into_iter()
            .map(|section| {
                let books = section.books.into_iter().map(|book| card_for(book, cx)).collect();
                BrowseSection { parent: section.parent, children: section.children, books, book_count: section.book_count }
            })
            .collect();
        let books: Rc<Vec<_>> = Rc::new(books.into_iter().map(|book| card_for(book, cx)).collect());
        self.book_indices = self.page.as_ref().map(|page| (page.book_offset..).zip(books.iter().cloned()).collect()).unwrap_or_default();
        self.sections = sections;
        self.books = books;
        let direct = self.children.iter().cloned().map(|row| BrowseItem::Child { row, parent: None });
        let nested = self.sections.iter().flat_map(|section| section.children.iter().cloned().map(move |row| BrowseItem::Child { row, parent: section.parent.id.folder() }));
        let books = self.books.iter().cloned().map(BrowseItem::Book);
        self.items = Rc::new(direct.chain(nested).chain(books).map(|item| (item.id(cx), item)).collect());
        path
    }

    /// Adds a feed's next page after what is listed. A book or chip the
    /// listing already has is not listed twice.
    pub(super) fn append(&mut self, data: LibraryBrowseData, cx: &mut App) {
        self.next = data.feed.and_then(|feed| feed.next);
        for row in data.contents.children {
            let item = BrowseItem::Child { row: row.clone(), parent: None };
            if Rc::make_mut(&mut self.items).insert(item.id(cx), item).is_none() {
                Rc::make_mut(&mut self.children).push(row);
            }
        }
        for book in data.contents.books {
            let id = book_id(book.key(), book.source_directory);
            if self.items.contains_key(&id) {
                continue;
            }
            let card = BookCardView::create_in_grid(book, self.ui.clone(), self.book_events.clone(), self.folder_commands, false, cx);
            Rc::make_mut(&mut self.items).insert(id, BrowseItem::Book(card.clone()));
            Rc::make_mut(&mut self.books).push(card);
        }
    }

    /// Merge a window without changing the listing revision or existing card
    /// entities. Section arrows retain the pages they have already fetched.
    pub(super) fn append_page(&mut self, data: LibraryBrowseData, cx: &mut App) -> bool {
        let Some(page) = data.page else { return false };
        let Some(current) = &self.page else { return false };
        if !compatible_pages(current, &page) {
            return false;
        }
        if page.headings.is_some() {
            self.heading_rows.extend((page.section_offset..).zip(data.contents.children.into_iter().filter(|row| is_author_heading(row.id))));
            let extras = self.children.iter().filter(|row| !is_author_heading(row.id)).cloned();
            self.children = Rc::new(self.heading_rows.values().cloned().chain(extras).collect());
            for row in self.heading_rows.values() {
                let item = BrowseItem::Child { row: row.clone(), parent: None };
                Rc::make_mut(&mut self.items).insert(item.id(cx), item);
            }
        }
        for section in data.sections {
            if self.sections.iter().any(|found| found.parent.id == section.parent.id) {
                continue;
            }
            let books = self.section_book_cards(section.books, cx);
            for child in &section.children {
                let item = BrowseItem::Child { row: child.clone(), parent: section.parent.id.folder() };
                Rc::make_mut(&mut self.items).insert(item.id(cx), item);
            }
            self.sections.push(BrowseSection { parent: section.parent, children: section.children, books, book_count: section.book_count });
        }
        for (index, row) in (page.book_offset..).zip(data.contents.books) {
            if self.book_indices.contains_key(&index) {
                continue;
            }
            let book = BookCardView::create_in_grid(row, self.ui.clone(), self.book_events.clone(), self.folder_commands, self.kind == BrowseKind::Books, cx);
            let item = BrowseItem::Book(book.clone());
            Rc::make_mut(&mut self.items).insert(item.id(cx), item);
            self.book_indices.insert(index, book);
        }
        self.books = Rc::new(self.book_indices.values().cloned().collect());
        true
    }

    pub(super) fn missing_page(&self, slots: Range<usize>, compact: usize) -> Option<BrowsePageRequest> {
        let page = self.page.as_ref()?;
        page_request_for_slots(
            slots,
            compact,
            page.heading_count(),
            page.book_total,
            |index| if page.headings.is_some() { self.heading_rows.contains_key(&index) } else { self.sections.iter().any(|section| section.parent.id == page.section_ids[index]) },
            |index| self.book_indices.contains_key(&index),
        )
    }

    pub(super) fn compact_slot_count(&self) -> usize {
        match &self.page {
            Some(page) if page.headings.is_some() => self.children.iter().filter(|row| !is_author_heading(row.id)).count(),
            Some(page) => self.children.len().saturating_sub(page.section_ids.len()),
            None => self.children.len(),
        }
    }

    pub(super) fn item(&self, id: &SharedString) -> Option<&BrowseItem> {
        self.items.get(id)
    }

    pub(super) fn section_book_cards(&self, books: Vec<library_model::BookCardRow>, cx: &mut App) -> Vec<Entity<BookCardView>> {
        books.into_iter().map(|book| BookCardView::create_in_grid(book, self.ui.clone(), self.book_events.clone(), self.folder_commands, false, cx)).collect()
    }

    pub(super) fn child(&self, id: BrowseLocation) -> Option<&BrowseRow> {
        self.items.get(&chip_id(id))?.child()
    }
}

pub(super) fn is_author_heading(location: BrowseLocation) -> bool {
    matches!(location, BrowseLocation::Author(Some(_)) | BrowseLocation::CatalogAuthor(Some(_)))
}

fn compatible_pages(current: &BrowsePageState, next: &BrowsePageState) -> bool {
    current.result_revision == next.result_revision
        && current.book_total == next.book_total
        && match (&current.headings, &next.headings) {
            (Some(current), Some(next)) => current == next,
            (None, None) => current.section_ids == next.section_ids,
            _ => false,
        }
}

fn page_request_for_slots(slots: Range<usize>, compact: usize, sections: usize, books: usize, section_loaded: impl Fn(usize) -> bool, book_loaded: impl Fn(usize) -> bool) -> Option<BrowsePageRequest> {
    let mut section_range = slots.start.saturating_sub(compact).min(sections)..slots.end.saturating_sub(compact).min(sections);
    let section = section_range.find(|index| !section_loaded(*index));
    let books_start = compact + sections;
    let mut book_range = slots.start.saturating_sub(books_start).min(books)..slots.end.saturating_sub(books_start).min(books);
    let book = book_range.find(|index| !book_loaded(*index));
    if section.is_none() && book.is_none() {
        return None;
    }
    Some(BrowsePageRequest { section_offset: section.unwrap_or(0), section_limit: if section.is_some() { 24 } else { 0 }, book_offset: book.unwrap_or(0), book_limit: if book.is_some() { 48 } else { 0 } })
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_placements_have_distinct_selection_and_cursor_ids() {
        let key = CardKey([1; 32]);
        let first = book_id(key, Some(DirId::from_u128(1)));
        let second = book_id(key, Some(DirId::from_u128(2)));
        let unplaced = book_id(key, None);
        assert_ne!(first, second, "two placements of one book must remain independently selectable");
        assert_ne!(first, unplaced);
        assert_ne!(second, unplaced);
    }

    use super::{book_id, compatible_pages, page_request_for_slots};
    use library_model::{BrowseHeadingState, BrowseLocation, BrowsePageState, CardKey};
    use sync_common::DirId;

    #[test]
    fn matching_counts_do_not_allow_pages_from_a_different_result_revision() {
        let first = BrowsePageState { section_ids: Vec::new(), section_offset: 0, book_offset: 0, book_total: 50, headings: None, result_revision: Some("old".into()) };
        let mut next = first.clone();
        next.book_offset = 24;
        assert!(compatible_pages(&first, &next));
        next.result_revision = Some("new".into());
        assert!(!compatible_pages(&first, &next));
    }

    #[test]
    fn heading_windows_share_a_revision_and_reject_reordered_or_changed_indexes() {
        let mut first = BrowsePageState {
            section_ids: vec![BrowseLocation::Author(Some(uuid::Uuid::from_u128(1)))],
            section_offset: 0,
            book_offset: 0,
            book_total: 0,
            result_revision: None,
            headings: Some(BrowseHeadingState { total: 37, revision: "one".into(), cards: true }),
        };
        let mut next = first.clone();
        next.section_offset = 24;
        next.section_ids = vec![BrowseLocation::Author(Some(uuid::Uuid::from_u128(25)))];
        assert!(compatible_pages(&first, &next));
        next.headings.as_mut().unwrap().revision = "two".into();
        assert!(!compatible_pages(&first, &next));
        next.headings.as_mut().unwrap().revision = "one".into();
        next.headings.as_mut().unwrap().total = 38;
        assert!(!compatible_pages(&first, &next));
        first.headings = None;
        assert!(!compatible_pages(&first, &next));
    }

    #[test]
    fn outer_pages_skip_compact_chips_and_keep_section_and_book_ranges_independent() {
        assert!(page_request_for_slots(0..2, 2, 25, 100, |_| false, |_| false).is_none());
        let panels = page_request_for_slots(24..27, 2, 25, 100, |index| index < 24, |_| false).unwrap();
        assert_eq!((panels.section_offset, panels.section_limit, panels.book_limit), (24, 24, 0));
        let mixed = page_request_for_slots(26..30, 2, 25, 100, |index| index < 24, |_| false).unwrap();
        assert_eq!((mixed.section_offset, mixed.book_offset), (24, 0));
        assert_eq!((mixed.section_limit, mixed.book_limit), (24, 48));
        let covers = page_request_for_slots(77..81, 2, 25, 100, |_| true, |index| index < 48).unwrap();
        assert_eq!((covers.book_offset, covers.book_limit, covers.section_limit), (50, 48, 0));
    }

    #[test]
    fn cached_backward_pages_and_empty_or_past_end_ranges_need_no_request() {
        assert!(page_request_for_slots(2..8, 2, 25, 100, |_| true, |_| false).is_none());
        assert!(page_request_for_slots(27..31, 2, 25, 100, |_| true, |index| index < 48).is_none());
        assert!(page_request_for_slots(0..50, 0, 0, 0, |_| false, |_| false).is_none());
        assert!(page_request_for_slots(200..220, 2, 25, 100, |_| false, |_| false).is_none());
        let distant = page_request_for_slots(4020..4024, 0, 4707, 0, |index| index < 24, |_| false).unwrap();
        assert_eq!(distant.section_offset, 4020);
        assert_eq!(distant.book_limit, 0);
    }

    #[cfg(all(not(target_arch = "wasm32"), not(feature = "kobo")))]
    #[gpui::test]
    async fn overlapping_feed_pages_preserve_snapshots_order_and_existing_cards(cx: &mut gpui::TestAppContext) {
        use super::{BrowseKind, BrowseListing, chip_id};
        use crate::navigation::LibrarySession;
        use library_backend::LibraryBrowseData;
        use library_model::{BookCardRow, BookSource, BrowseContents, BrowseRow, FeedPage, LibraryFileTypeFilter};
        use std::rc::Rc;
        use sync_common::BookId;

        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let mut listing = BrowseListing::new(library, BrowseKind::Catalog, Rc::new(|_, _, _| {}), false);
        let child = |index| BrowseRow { id: BrowseLocation::Folder(DirId::from_u128(index)), name: format!("Link {index}"), path: None, book_count: 0, downloaded_book_count: 0 };
        let book = |index| BookCardRow {
            source: BookSource::Catalog { content_hash: BookId::from_bytes([index; 32]) },
            title: format!("Book {index}"),
            subtitle: None,
            author: "Writer".into(),
            progress: 0.0,
            downloaded: false,
            download_requested: false,
            format_category: LibraryFileTypeFilter::Book,
            audiobook_duration_ms: None,
            added_at: 0,
            source_directory: None,
        };
        let next = BrowseLocation::Folder(DirId::from_u128(10));
        let first = LibraryBrowseData { contents: BrowseContents { children: vec![child(1)], books: vec![book(1)] }, feed: Some(FeedPage { next: Some(next), ..Default::default() }), ..Default::default() };
        cx.update(|cx| listing.install(first, false, cx));
        assert_eq!(listing.next, Some(next));
        let children = listing.children.clone();
        let books = listing.books.clone();
        let items = listing.items.clone();
        let mut duplicate_child = child(1);
        duplicate_child.name = "Updated link".into();
        let mut duplicate_book = book(1);
        duplicate_book.title = "Updated book".into();
        let second = LibraryBrowseData { contents: BrowseContents { children: vec![duplicate_child, child(2), child(2)], books: vec![duplicate_book, book(2), book(2)] }, feed: Some(FeedPage::default()), ..Default::default() };
        cx.update(|cx| listing.append(second, cx));
        assert!(listing.next.is_none());
        assert_eq!(&*children, &[child(1)]);
        assert_eq!(books.len(), 1);
        assert_eq!(items.len(), 2);
        assert_eq!(items[&chip_id(child(1).id)].child().unwrap().name, "Link 1");
        assert_eq!(&*listing.children, &[child(1), child(2)]);
        assert_eq!(listing.items.len(), 4);
        assert_eq!(listing.items[&chip_id(child(1).id)].child().unwrap().name, "Updated link");
        assert_eq!(listing.books[0], books[0]);
        cx.update(|cx| {
            let titles: Vec<_> = listing.books.iter().map(|book| book.read(cx).title()).collect();
            assert_eq!(titles, ["Book 1", "Book 2"]);
            for card in listing.books.iter() {
                let id = book_id(card.read(cx).key(), card.read(cx).source_directory());
                assert_eq!(listing.items[&id].book(), Some(card));
            }
        });
    }
}
