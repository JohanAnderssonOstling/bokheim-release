//! What a browse page lists — the folders or subjects under one location, and
//! the books with them — and how it is fetched from the library.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use gpui::{App, Entity, SharedString};
use library_model::{BrowseBookSort, BrowseChipSort, BrowseContents, BrowseRow, LibraryBrowseQuery, LibraryFileTypeFilter};
use subject_projection::ROOT_SUBJECT_PATH;
use sync_common::ROOT_DIR_ID;
use ui_components as components;

use super::controls::{BrowseBookOrder, BrowseControls, BrowseFormatFilter};
use crate::library::services::LibraryContext;
use crate::library::widgets::{BookCardView, OpenBookDetail, PlaceBook, RemoveBook};

/// Whether sections lay the listing out as cards, each with its own rails.
/// While they do, chips are never grouped under headings.
pub(super) const SECTION_LAYOUT: bool = true;

pub(crate) type BrowseFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// Which tree a browse page walks. Folders are real directories, with the
/// commands that go with them; subjects are queries over the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowseKind {
    Folder,
    Subject,
}

impl BrowseKind {
    pub(super) fn root_location(self) -> String {
        match self {
            Self::Folder => ROOT_DIR_ID.to_string(),
            Self::Subject => ROOT_SUBJECT_PATH.to_owned(),
        }
    }

    pub(super) fn root_label(self) -> SharedString {
        match self {
            Self::Folder => "Folders".into(),
            Self::Subject => "Subjects".into(),
        }
    }

    /// Prefixes the element ids the page hands out.
    pub(super) fn list_prefix(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Subject => "subject",
        }
    }

    pub(super) fn child_icon(self) -> components::NavigationIcon {
        match self {
            Self::Folder => components::NavigationIcon::Folder,
            Self::Subject => components::NavigationIcon::Tags,
        }
    }

    /// Real folder parents belong in the trail; subject groups are only a way
    /// of laying chips out, so they do not.
    pub(super) fn includes_group_in_path(self) -> bool {
        self == Self::Folder
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowseQuery {
    pub(crate) location: String,
    pub(crate) search: String,
    pub(crate) formats: Vec<BrowseFormatFilter>,
    pub(crate) chip_sort: Option<BrowseChipSort>,
    pub(crate) book_sort: Option<BrowseBookOrder>,
    pub(crate) languages: Vec<&'static str>,
    pub(crate) hide_finished: bool,
    pub(crate) include_direct_child_books: bool,
}

impl BrowseQuery {
    fn backend(&self) -> LibraryBrowseQuery {
        let file_types = self
            .formats
            .iter()
            .filter_map(|format| match format {
                BrowseFormatFilter::Library(format) => Some(*format),
                BrowseFormatFilter::Any => None,
            })
            .collect::<Vec<LibraryFileTypeFilter>>();
        let book_sort = match self.book_sort {
            Some(BrowseBookOrder::Library(sort)) => sort,
            None => BrowseBookSort::default(),
        };
        LibraryBrowseQuery {
            location: self.location.clone(),
            search: self.search.clone(),
            file_types,
            chip_sort: self.chip_sort.unwrap_or_default(),
            book_sort,
            languages: self.languages.iter().map(|language| (*language).to_owned()).collect(),
            hide_finished: self.hide_finished,
            include_direct_child_books: self.include_direct_child_books,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BrowseCrumb {
    pub(crate) location: String,
    pub(crate) label: String,
}

/// A browsable panel with the immediate children and directly assigned books
/// of its parent location.
pub(crate) struct BrowseSection {
    pub(crate) parent: BrowseRow,
    pub(crate) children: Vec<BrowseRow>,
    pub(crate) books: Vec<Entity<BookCardView>>,
}

pub(crate) enum BrowseResponse {
    Contents {
        /// Whether this answers a search. A search runs from the current
        /// location, never across the library, and the library answers it with
        /// every folder or subject below that location whose name matches —
        /// each with the path that places it — rather than with the location's
        /// sections, so those matches are the chips.
        searched: bool,
        contents: BrowseContents,
        chip_groups: Vec<library_model::BrowseChipGroup>,
        sections: Vec<library_model::BrowseSection>,
        path: Vec<BrowseCrumb>,
        language_counts: Vec<library_model::LibraryLanguageCount>,
        format_counts: Vec<library_model::LibraryFormatCount>,
    },
}

/// Whether a chip still has books to bring onto this device.
pub(super) fn can_download(child: &BrowseRow) -> bool {
    child.downloaded_book_count < child.book_count
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
/// book still in the listing keeps its entity and its decoded cover.
pub(super) struct BrowseListing {
    ui: LibraryContext,
    kind: BrowseKind,
    controls: BrowseControls,
    children: Rc<Vec<BrowseRow>>,
    chip_groups: Vec<library_model::BrowseChipGroup>,
    sections: Vec<BrowseSection>,
    books: Rc<Vec<Entity<BookCardView>>>,
    open_detail: OpenBookDetail,
    place_book: Option<PlaceBook>,
    remove_book: Option<RemoveBook>,
}

impl BrowseListing {
    pub(super) fn new(ui: LibraryContext, kind: BrowseKind, open_detail: OpenBookDetail, place_book: Option<PlaceBook>, remove_book: Option<RemoveBook>) -> Self {
        Self { ui, kind, controls: Self::controls_for(kind, BrowseControls::library()), children: Rc::new(Vec::new()), chip_groups: Vec::new(), sections: Vec::new(), books: Rc::new(Vec::new()), open_detail, place_book, remove_book }
    }

    fn controls_for(kind: BrowseKind, mut controls: BrowseControls) -> BrowseControls {
        controls.supports_direct_child_books = kind == BrowseKind::Subject;
        controls
    }

    pub(super) fn request(&self, query: &BrowseQuery) -> BrowseFuture<Result<BrowseResponse, String>> {
        let query = query.backend();
        let library = self.ui.backend().clone();
        let searched = !query.search.is_empty();
        let kind = self.kind;
        Box::pin(async move {
            let response: app::LibraryBrowseData = match kind {
                BrowseKind::Folder => library.folder_contents(query).await,
                BrowseKind::Subject => library.subject_contents(query).await,
            }?;
            let path = response.path.into_iter().map(|segment| BrowseCrumb { location: segment.id, label: segment.name }).collect();
            Ok(BrowseResponse::Contents { searched, contents: response.contents, chip_groups: response.chip_groups, sections: response.sections, path, language_counts: response.language_counts, format_counts: response.format_counts })
        })
    }

    pub(super) fn download_child(&self, query: &BrowseQuery) -> BrowseFuture<Result<(), String>> {
        let library = self.ui.backend().clone();
        let query = query.backend();
        let kind = self.kind;
        Box::pin(async move {
            match kind {
                BrowseKind::Folder => library.download_folder(query).await,
                BrowseKind::Subject => library.download_subject(query).await,
            }
        })
    }

    /// Installs a response, and hands back the trail the library says leads to
    /// it, where it knows one.
    pub(super) fn install(&mut self, response: BrowseResponse, cx: &mut App) -> Option<Vec<BrowseCrumb>> {
        let BrowseResponse::Contents { searched, contents, chip_groups, sections, path, language_counts, format_counts } = response;
        let children = if searched { contents.children } else { sections.iter().map(|section| section.parent.clone()).collect() };
        let (books, path) = (contents.books, Some(path));
        self.controls = Self::controls_for(self.kind, BrowseControls::library_with_facets(&language_counts, &format_counts));
        self.children = Rc::new(children);
        self.chip_groups = chip_groups;
        // A card outlives a reload wherever its book is still listed — in the
        // grid or in a section card — so a refresh keeps every decoded cover
        // rather than drawing each book blank until its cover loads again. A
        // library change reloads the listing, and during a scan that is often.
        let mut previous: HashMap<sync_common::ContentHash, Vec<Entity<BookCardView>>> = HashMap::new();
        for card in self.books.iter().chain(self.sections.iter().flat_map(|section| section.books.iter())) {
            previous.entry(card.read(cx).content_hash()).or_default().push(card.clone());
        }
        let mut card_for = |book: library_model::BookCardRow, cover_only: bool, section_cover: bool, cx: &mut App| {
            let card = match previous.get_mut(&book.content_hash).and_then(Vec::pop) {
                Some(card) => {
                    let source_directory = book.source_directory;
                    card.update(cx, |card, _| card.replace_row(book, source_directory));
                    card
                }
                None => self.create_card(book, cx),
            };
            card.update(cx, |card, cx| {
                card.set_cover_only(cover_only, cx);
                card.set_section_cover_left_aligned(section_cover, cx);
                card.set_cover_top_aligned(false, cx);
            });
            card
        };
        let sections = sections
            .into_iter()
            .map(|section| BrowseSection { parent: section.parent, children: section.children, books: section.books.into_iter().map(|book| card_for(book, true, true, cx)).collect() })
            .collect();
        let books = Rc::new(books.into_iter().map(|book| card_for(book, false, false, cx)).collect());
        self.sections = sections;
        self.books = books;
        // A subject's trail is built from its location instead; see `path_to`.
        path.filter(|_| self.kind == BrowseKind::Folder)
    }

    fn create_card(&self, book: library_model::BookCardRow, cx: &mut App) -> Entity<BookCardView> {
        let source_directory = book.source_directory;
        BookCardView::create_in_grid(book, self.ui.clone(), source_directory, self.open_detail.clone(), self.place_book.clone(), self.remove_book.clone(), cx)
    }

    /// The full trail to `child`, where its id spells one out. A subject's id
    /// is its path, segments joined by `" / "`; a folder's id names only
    /// itself.
    pub(super) fn path_to(&self, child: &BrowseRow) -> Option<Vec<BrowseCrumb>> {
        if self.kind == BrowseKind::Folder {
            return None;
        }
        let segments = child.id.split(" / ").filter(|segment| !segment.is_empty() && *segment != ROOT_SUBJECT_PATH).collect::<Vec<_>>();
        let mut location = String::new();
        let path = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                if !location.is_empty() {
                    location.push_str(" / ");
                }
                location.push_str(segment);
                let label = if index + 1 == segments.len() { child.name.clone() } else { (*segment).to_owned() };
                BrowseCrumb { location: location.clone(), label }
            })
            .collect();
        Some(path)
    }

    pub(super) fn children(&self) -> Rc<Vec<BrowseRow>> {
        self.children.clone()
    }

    pub(super) fn chip_groups(&self) -> &[library_model::BrowseChipGroup] {
        if SECTION_LAYOUT { &[] } else { &self.chip_groups }
    }

    pub(super) fn sections(&self) -> &[BrowseSection] {
        &self.sections
    }

    pub(super) fn books(&self) -> Rc<Vec<Entity<BookCardView>>> {
        self.books.clone()
    }

    pub(super) fn controls(&self) -> &BrowseControls {
        &self.controls
    }
}
