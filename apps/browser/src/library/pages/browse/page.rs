//! Shared browse-page state and layout for folders and subjects.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, DismissEvent, Entity, FocusHandle, Focusable as _, IntoElement, KeyDownEvent, Pixels, Point, Render, SharedString, Subscription, WeakEntity, Window, div, px, rems};
use gpui_component::IconName;
use gpui_component::breadcrumb::BreadcrumbItem;
use gpui_component::menu::PopupMenu;
use library_model::{BrowseChipSort, BrowseRow};
use sync_common::{DirId, ROOT_DIR_ID};
use ui_components as components;

use super::detail::{BookDetailPage, CloseBookDetail};
use super::controls::{BrowseBookOrder, BrowseControls, BrowseFormatFilter, BrowseOptionsEvent, BrowseOptionsSelection, browse_options_control, browse_options_sheet};
use super::folder_actions::{FolderActions, FolderActionsEvent, FolderTargets, ItemCommands};
use super::graph::{BrowseGraph, GraphTap};
use super::layout::{acted_on, folder_drop_target, folder_targets};
use super::listing::{BrowseCrumb, BrowseKind, BrowseListing, BrowseQuery, BrowseResponse, SECTION_LAYOUT, can_download};
use super::search::{BrowseSearch, SearchChanged};
use super::section_card::SectionCard;
use crate::library::services::{LatestRequest, LibraryContentsChanged, LibraryContext, LibraryDownloadChanged, LibraryScanChanged};
use crate::library::widgets::{CursorMove, LoadedCover, OpenBookDetail, Paginator, PaginatorAxis, PaginatorChild, PaginatorGroup, PaginatorMarqueeChanged, PaginatorSelectionChanged, graph_cover_policy, mobile_context_menu};
use crate::{DismissSettings, OpenContextMenu, UndoLibraryAction};

/// How narrow the desktop graph's cover sidebar may be dragged: enough for the
/// row of topbar controls it carries (280px at the default UI font), below
/// which they would run off its edge. It was a fixed 360px.
const GRAPH_SIDEBAR_MINIMUM_WIDTH_REM: f32 = 17.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowseSplitSide {
    Left,
    Right,
}

#[derive(Clone, Debug)]
pub(crate) struct BrowseSplitRequest {
    pub(crate) path: Vec<BrowseCrumb>,
    pub(crate) side: BrowseSplitSide,
}

pub(crate) struct BrowseSplitClose;

pub(crate) type OpenTrash = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
struct BrowseSelection {
    search: String,
    formats: Vec<BrowseFormatFilter>,
    chip_sort: Option<BrowseChipSort>,
    book_sort: Option<BrowseBookOrder>,
    languages: Vec<&'static str>,
    hide_finished: bool,
    include_direct_child_books: bool,
}

impl BrowseSelection {
    fn new(controls: &BrowseControls) -> Self {
        Self {
            search: String::new(),
            formats: Vec::new(),
            chip_sort: controls.collection_sorts.first().copied(),
            book_sort: controls.book_sorts.first().copied(),
            languages: Vec::new(),
            hide_finished: false,
            include_direct_child_books: false,
        }
    }

    fn query(&self, location: String) -> BrowseQuery {
        BrowseQuery {
            location,
            search: self.search.clone(),
            formats: self.formats.clone(),
            chip_sort: self.chip_sort,
            book_sort: self.book_sort,
            languages: self.languages.clone(),
            hide_finished: self.hide_finished,
            include_direct_child_books: self.include_direct_child_books,
        }
    }

    fn options(&self) -> BrowseOptionsSelection {
        BrowseOptionsSelection::new(self.chip_sort, self.book_sort, self.formats.clone(), self.languages.clone(), self.hide_finished, self.include_direct_child_books)
    }

    /// Applies a choice from the sort and filter controls, reporting whether
    /// it changed anything. A choice the controls do not offer is ignored.
    fn apply(&mut self, event: BrowseOptionsEvent, controls: &BrowseControls) -> bool {
        match event {
            BrowseOptionsEvent::SelectChipSort(sort) => controls.collection_sorts.contains(&sort) && std::mem::replace(&mut self.chip_sort, Some(sort)) != Some(sort),
            BrowseOptionsEvent::SelectBookSort(sort) => controls.book_sorts.contains(&sort) && std::mem::replace(&mut self.book_sort, Some(sort)) != Some(sort),
            BrowseOptionsEvent::ToggleFormat(format) => toggle(&mut self.formats, (format != BrowseFormatFilter::Any).then_some(format), &controls.formats),
            BrowseOptionsEvent::ToggleLanguage(language) => toggle(&mut self.languages, language, &controls.languages),
            BrowseOptionsEvent::ToggleHideFinished => {
                self.hide_finished = !self.hide_finished;
                true
            }
            BrowseOptionsEvent::ToggleDirectChildBooks => {
                self.include_direct_child_books = !self.include_direct_child_books;
                true
            }
        }
    }
}

/// Toggles `choice` in `selected`, or clears `selected` for `None`, the "any"
/// row. Reports whether anything changed.
fn toggle<T: Copy + PartialEq>(selected: &mut Vec<T>, choice: Option<T>, offered: &[T]) -> bool {
    let Some(choice) = choice else {
        let changed = !selected.is_empty();
        selected.clear();
        return changed;
    };
    if !offered.contains(&choice) {
        return false;
    }
    match selected.iter().position(|item| *item == choice) {
        Some(index) => {
            selected.remove(index);
        }
        None => selected.push(choice),
    }
    true
}

/// How the page shows its location: as section cards, or as a graph whose
/// column or circle layout is chosen separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowseView {
    Cards,
    Graph,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoadIntent {
    Initial,
    Reload,
    Navigate,
    Refresh,
}

impl LoadIntent {
    fn reveals_end(self) -> bool {
        false
    }
}

#[derive(Clone)]
struct PendingLoad {
    location: String,
    path: Vec<BrowseCrumb>,
    intent: LoadIntent,
}

/// What the cursor was on when a location was left.
///
/// A book is remembered by hash rather than by index, because the folder can be
/// reordered by a rescan while the reader is off looking at one of its books,
/// and an index would then point at a different book — confidently.
enum SavedCursor {
    Chip(String),
    Book(sync_common::ContentHash),
}

struct SavedChipSelection {
    cursor: SavedCursor,
    child_anchor: usize,
}

/// One open chip context menu. Chips are render closures, so the menu they open
/// lives on the page rather than on the chip, and only one is open at a time.
struct ChipContextMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

pub(crate) struct BrowsePage {
    ui: LibraryContext,
    open_trash: OpenTrash,
    kind: BrowseKind,
    location: String,
    path: Vec<BrowseCrumb>,
    listing: BrowseListing,
    paginator: Entity<Paginator>,
    graph_cover_paginator: Entity<Paginator>,
    /// The cards of a sectioned listing, by the id of the chip each belongs to.
    /// Kept across refreshes so a card keeps its rails' positions.
    section_cards: HashMap<String, Entity<SectionCard>>,
    selected_chips: HashMap<String, SavedChipSelection>,
    /// A book's page, shown over the grid while `detail_visible`. The grid
    /// stays installed underneath, so closing the page returns to it as it
    /// was. Kept after closing, so coming back to the same book — from its
    /// reader, say — reuses it.
    detail: Option<Entity<BookDetailPage>>,
    detail_visible: bool,
    chip_menu: Option<ChipContextMenu>,
    focus: FocusHandle,
    search: Entity<BrowseSearch>,
    /// The compact bottom-sheet counterpart to the desktop sort/filter dropdown.
    browse_options_sheet_open: bool,
    /// Which of the two card modes the grid is in. A view choice rather than a
    /// query one, so it lives beside the search box instead of in the sort and
    /// filter menu, and changing it never reloads.
    detailed_cards: bool,
    view: BrowseView,
    graph: BrowseGraph,
    graph_cover_sheet_expanded: bool,
    graph_sidebar_width: f32,
    graph_sidebar_drag: Option<(f32, f32)>,
    selection: BrowseSelection,
    loaded: bool,
    error: Option<SharedString>,
    request: LatestRequest,
    /// The folder commands, on a folder page only: a subject is a query, not
    /// a directory, and has nothing to rename, move or trash.
    folder_actions: Option<Entity<FolderActions>>,
    _subscriptions: Vec<Subscription>,
    split_close_enabled: bool,
}

impl gpui::EventEmitter<BrowseSplitRequest> for BrowsePage {}
impl gpui::EventEmitter<BrowseSplitClose> for BrowsePage {}

/// Lets a card ask for its book's page. Weak, because the page holds the
/// cards that hold this.
fn open_detail(cx: &mut Context<BrowsePage>) -> OpenBookDetail {
    let page = cx.entity().downgrade();
    Rc::new(move |book, cover, cx| {
        let _ = page.update(cx, |page, cx| page.show_book_detail(book, cover, cx));
    })
}

impl BrowsePage {
    pub(crate) fn new(kind: BrowseKind, ui: LibraryContext, open_trash: OpenTrash, cx: &mut Context<Self>) -> Self {
        let folder_actions = (kind == BrowseKind::Folder).then(|| cx.new(|_| FolderActions::new(ui.clone(), ROOT_DIR_ID, None)));
        let listing = BrowseListing::new(ui.clone(), kind, open_detail(cx), folder_actions.as_ref().map(FolderActions::place_book), folder_actions.as_ref().map(FolderActions::remove_book));
        let selection = BrowseSelection::new(listing.controls());
        let page = cx.entity().downgrade();
        let selectable = folder_actions.is_some();
        let paginator = cx.new(|_| {
            let paginator = Paginator::new(format!("{}-browse-paginator", kind.list_prefix()), []).flow_axis(PaginatorAxis::Vertical);
            if !selectable {
                return paginator;
            }
            paginator.selectable().on_context_menu(move |child_index, position, window, cx| {
                let _ = page.update(cx, |page, cx| match child_index {
                    // Control with a secondary click opens a chip in the
                    // right pane, which the chip answers itself.
                    Some(child_index) if window.modifiers().control && page.can_split(window) && child_index < page.listing.children().len() => {}
                    Some(child_index) => page.open_items_menu(child_index, position, window, cx),
                    None => page.open_folder_menu(position, window, cx),
                });
            })
        });
        let graph_cover_paginator = cx.new(|_| Paginator::new(format!("{}-graph-covers", kind.list_prefix()), []).flow_axis(PaginatorAxis::Vertical));
        let search = cx.new(|_| BrowseSearch::new());
        let updates = ui.updates();
        let mut subscriptions = vec![
            cx.observe_global::<crate::stores::CoverTextSetting>(|page, cx| page.refresh_cover_text(cx)),
            cx.subscribe(&search, |page, _, SearchChanged(query): &SearchChanged, cx| {
                page.selection.search = query.clone();
                page.reload(cx);
            }),
            cx.subscribe(&updates, |page, _, _: &LibraryContentsChanged, cx| page.refresh_contents(cx)),
            cx.subscribe(&updates, |page, _, event: &LibraryDownloadChanged, cx| {
                if matches!(event.status.state, app::DownloadState::Downloaded | app::DownloadState::NotDownloaded) {
                    page.refresh_contents(cx);
                }
            }),
            // Only repaints: the empty listing reads whether a scan is running.
            cx.subscribe(&updates, |_, _, _: &LibraryScanChanged, cx| cx.notify()),
        ];
        if let Some(actions) = &folder_actions {
            subscriptions.push(cx.subscribe(actions, |page, _, event: &FolderActionsEvent, cx| page.handle_folder_action(event.clone(), cx)));
            // Chips read the selection and the band off the paginator as they
            // draw; book cards are entities, and are told.
            subscriptions.push(cx.subscribe(&paginator, |page, _, _: &PaginatorSelectionChanged, cx| page.sync_selection(cx)));
            subscriptions.push(cx.subscribe(&paginator, |page, paginator, _: &PaginatorMarqueeChanged, cx| {
                let marquee = paginator.read(cx).marquee_active();
                for book in page.listing.books().iter() {
                    book.update(cx, |book, cx| book.set_marquee(marquee, cx));
                }
            }));
        }
        let mut page = Self {
            ui,
            open_trash,
            kind,
            location: kind.root_location(),
            path: Vec::new(),
            listing,
            paginator,
            graph_cover_paginator,
            section_cards: HashMap::new(),
            selected_chips: HashMap::new(),
            detail: None,
            detail_visible: false,
            chip_menu: None,
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            search,
            browse_options_sheet_open: false,
            detailed_cards: false,
            view: BrowseView::Cards,
            graph: BrowseGraph::new(if kind == BrowseKind::Subject { components::GraphLayoutKind::AdaptiveRadial } else { components::GraphLayoutKind::Columns }, cx),
            graph_cover_sheet_expanded: false,
            graph_sidebar_width: 380.0,
            graph_sidebar_drag: None,
            selection,
            loaded: false,
            error: None,
            request: LatestRequest::default(),
            folder_actions,
            _subscriptions: subscriptions,
            split_close_enabled: false,
        };
        page.load(Vec::new(), LoadIntent::Initial, cx);
        page
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.load_at(self.path.clone(), self.location.clone(), LoadIntent::Reload, cx);
    }

    pub(super) fn download_child(&mut self, location: String, cx: &mut Context<Self>) {
        if !self.listing.children().iter().chain(self.listing.sections().iter().flat_map(|section| section.children.iter())).any(|child| child.id == location && can_download(child)) {
            return;
        }
        let mut query = self.selection.query(location);
        query.search.clear();
        let operation = self.listing.download_child(&query);
        cx.spawn(async move |page, cx| {
            let result = operation.await;
            let _ = page.update_in(cx, |page, window, cx| {
                if let Err(error) = result {
                    crate::services::notify_error("hierarchy-download-error", format!("Could not queue downloads: {error}"), window, cx);
                }
                page.refresh_contents(cx);
            });
        })
        .detach();
    }

    /// Refreshes scanner-driven content without replacing the current page by
    /// a loading frame. Newly discovered covers can prepare in place while the
    /// already-visible book cards remain painted.
    fn refresh_contents(&mut self, cx: &mut Context<Self>) {
        self.load_at(self.path.clone(), self.location.clone(), LoadIntent::Refresh, cx);
    }

    fn handle_folder_action(&mut self, event: FolderActionsEvent, cx: &mut Context<Self>) {
        match event {
            FolderActionsEvent::Reload => self.reload(cx),
            FolderActionsEvent::Renamed(name) => {
                if let Some(crumb) = self.path.last_mut() {
                    crumb.label = name;
                }
                self.reload(cx);
            }
            FolderActionsEvent::OpenRoot => self.open_path(Vec::new(), cx),
        }
    }

    /// The folders and books at `indices`, for the folder commands.
    fn menu_targets(&self, indices: &[usize], cx: &App) -> Option<FolderTargets> {
        let source = DirId::parse_str(&self.location).ok()?;
        folder_targets(indices, &self.listing.children(), self.listing.chip_groups(), &self.listing.books(), source, cx)
    }

    /// What "Open in New Window" opens: the folders and books targeted, or on
    /// a listing without folders, the one chip clicked — a subject is a query,
    /// opened by its location.
    fn window_targets(&self, targets: Option<&FolderTargets>, single_chip: Option<&BrowseRow>, cx: &App) -> Vec<crate::NewWindowTarget> {
        let library_id = *self.ui.backend().id();
        if !crate::native_windows_available() {
            return Vec::new();
        }
        let Some(targets) = targets else {
            return single_chip.map(|chip| crate::NewWindowTarget::Subject { library_id, location: chip.id.clone(), label: chip.name.clone() }).into_iter().collect();
        };
        let chips = self.listing.children();
        let folders = targets.folders.iter().map(|id| {
            let title = chips.iter().chain(single_chip).find(|chip| DirId::parse_str(&chip.id).ok() == Some(*id)).map(|chip| chip.name.clone()).unwrap_or_default();
            crate::NewWindowTarget::Folder { library_id, directory_id: *id, title }
        });
        let books = targets.books.iter().map(|hash| crate::NewWindowTarget::Book {
            locator: library_model::BookLocator::new(library_id, *hash),
            title: self.listing.books().iter().map(|book| book.read(cx)).find(|book| book.content_hash() == *hash).map(|book| book.title().to_owned()).unwrap_or_default(),
        });
        folders.chain(books).collect()
    }

    /// Opens `child_id` in a split pane and dismisses the menu it was chosen
    /// from. The split-action row's own click doesn't route through the
    /// framework's confirm/dismiss path — it's a custom element with its own
    /// handlers, not a plain menu item — so nothing closes the menu unless
    /// this does it explicitly.
    fn open_split_and_dismiss(&mut self, child_id: String, side: BrowseSplitSide, cx: &mut Context<Self>) {
        self.request_split_child(child_id, side, cx);
        self.chip_menu = None;
        cx.notify();
    }

    /// Opens the commands over a child, or over the selection it belongs to.
    /// Whole-item opens — a new window, a split pane — come first; the folder
    /// commands follow on folder listings only, since an author or subject
    /// chip is a query rather than a directory.
    pub(super) fn open_items_menu(&mut self, child_index: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let indices = acted_on(child_index, self.paginator.read(cx).selection());
        let chips = self.listing.children();
        // Opening in a split pane only makes sense for one chip at a time: a
        // book has nowhere to browse into, and a multi-selection has no one
        // path to hand the other pane.
        let single_chip = match indices.as_slice() {
            [index] => chips.get(*index),
            _ => None,
        };
        let split_target = single_chip.filter(|_| self.can_split(window)).map(|chip| chip.id.clone());
        // Every folder or subject acted on that still has books off this
        // device. The chip only marks a partly downloaded one, so this is the
        // way to fetch a wholly remote one.
        let download_targets = indices.iter().filter_map(|index| chips.get(*index)).filter(|chip| can_download(chip)).map(|chip| chip.id.clone()).collect::<Vec<_>>();
        let targets = self.folder_actions.as_ref().and_then(|_| self.menu_targets(&indices, cx));
        let window_targets = self.window_targets(targets.as_ref(), single_chip, cx);
        let folder_commands = self.folder_actions.clone().zip(targets).map(|(actions, targets)| {
            let folder = |id: DirId| chips.iter().find(|chip| DirId::parse_str(&chip.id).ok() == Some(id));
            let commands = actions.read(cx).item_commands(targets, |id| folder(id).map(|chip| chip.name.clone()), |id| folder(id).is_some_and(|chip| chip.downloaded_book_count > 0), cx);
            (actions, commands)
        });
        self.show_items_menu(window_targets, split_target, download_targets, folder_commands, position, window, cx);
    }

    /// Section children use the same item menu as ordinary chips.
    pub(super) fn open_section_child_menu(&mut self, child_id: &str, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some((parent, child)) = self.listing.sections().iter().find_map(|section| section.children.iter().find(|child| child.id == child_id).map(|child| (&section.parent, child))) else { return };
        let targets = self.folder_actions.as_ref().and_then(|_| {
            let folder = DirId::parse_str(&child.id).ok()?;
            let source = DirId::parse_str(&parent.id).ok()?;
            Some(FolderTargets { folders: vec![folder], folder_parents: Default::default(), books: Vec::new(), source })
        });
        let window_targets = self.window_targets(targets.as_ref(), Some(child), cx);
        let folder_commands = self.folder_actions.clone().zip(targets).map(|(actions, targets)| {
            let commands = actions.read(cx).item_commands(targets, |_| Some(child.name.clone()), |_| child.downloaded_book_count > 0, cx);
            (actions, commands)
        });
        self.show_items_menu(window_targets, self.can_split(window).then(|| child.id.clone()), can_download(child).then(|| child.id.clone()).into_iter().collect(), folder_commands, position, window, cx);
    }

    fn show_items_menu(&mut self, window_targets: Vec<crate::NewWindowTarget>, split_target: Option<String>, download_targets: Vec<String>, folder_commands: Option<(Entity<FolderActions>, ItemCommands)>, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if window_targets.is_empty() && split_target.is_none() && download_targets.is_empty() && folder_commands.is_none() {
            return;
        }
        let mobile_size = mobile_context_menu::size(window);
        let page = cx.entity();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if !window_targets.is_empty() {
                let targets = window_targets.clone();
                menu = menu.item(components::menu_item_with_icon("Open in New Window", IconName::ExternalLink, move |_, window, cx| {
                    for target in &targets {
                        window.dispatch_action(Box::new(crate::OpenInNewWindow(target.clone())), cx);
                    }
                }));
            }
            if let Some(child_id) = split_target.clone() {
                let (left_page, left_id) = (page.clone(), child_id.clone());
                let right_page = page.clone();
                menu = menu.item(components::menu_item_with_split_actions(
                    "Open left",
                    "Open right",
                    move |_, cx| left_page.update(cx, |page, cx| page.open_split_and_dismiss(left_id.clone(), BrowseSplitSide::Left, cx)),
                    move |_, cx| right_page.update(cx, |page, cx| page.open_split_and_dismiss(child_id.clone(), BrowseSplitSide::Right, cx)),
                ));
            }
            if !download_targets.is_empty() {
                let (download_page, targets) = (page.clone(), download_targets.clone());
                menu = menu.item(components::menu_item_with_icon("Download to This Device", components::download_glyph(), move |_, _, cx| {
                    download_page.update(cx, |page, cx| {
                        for target in &targets {
                            page.download_child(target.clone(), cx);
                        }
                    })
                }));
            }
            let menu = match folder_commands.clone() {
                Some((actions, commands)) => {
                    if !window_targets.is_empty() || split_target.is_some() || !download_targets.is_empty() {
                        menu = menu.separator();
                    }
                    FolderActions::item_menu_items(actions, commands, menu)
                }
                None => menu,
            };
            mobile_context_menu::configure(menu, mobile_size)
        });
        self.show_menu(menu, position, window, cx);
    }

    fn show_menu(&mut self, menu: Entity<PopupMenu>, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = cx.subscribe_in(&menu, window, |page, _, _: &DismissEvent, window, cx| {
            page.chip_menu = None;
            // The page had the keyboard before the menu took it, and a menu
            // dismissed by Escape must not leave the arrows dead.
            window.focus(&page.focus, cx);
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        self.chip_menu = Some(ChipContextMenu { menu, position, _subscription: subscription });
        cx.notify();
    }

    /// Imports paths dropped from the desktop into `destination`, or into the
    /// folder being browsed when the drop lands on open space.
    pub(super) fn import_dropped_paths(&mut self, destination: Option<DirId>, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let Some(parent) = destination.or_else(|| DirId::parse_str(&self.location).ok()) else { return };
        let ui = self.ui.clone();
        #[cfg(not(target_arch = "wasm32"))]
        ui.import_dropped_paths(parent, paths, cx);
        // The desktop hands over paths this build has no filesystem for.
        #[cfg(target_arch = "wasm32")]
        let _ = (ui, parent, paths, cx);
    }

    /// Asks what a drop onto a folder should do, rather than deciding it from a
    /// modifier held at the moment of release. The question is cheap, the two
    /// answers are not interchangeable, and a mistaken copy of a folder tree is
    /// tedious to unpick.
    pub(super) fn drop_onto_folder(&mut self, destination: DirId, targets: FolderTargets, window: &mut Window, cx: &mut Context<Self>) {
        if targets.all_in_folder(destination) {
            return;
        }
        let Some(actions) = self.folder_actions.clone() else { return };
        let name = self.listing.children().iter().find(|chip| DirId::parse_str(&chip.id).ok() == Some(destination)).map(|chip| chip.name.clone()).unwrap_or_else(|| "this folder".to_owned());
        let position = window.mouse_position();
        let mobile_size = mobile_context_menu::size(window);
        let menu = PopupMenu::build(window, cx, move |menu, _, _| mobile_context_menu::configure(FolderActions::drop_menu_items(actions.clone(), targets.clone(), destination, &name, menu), mobile_size));
        self.show_menu(menu, position, window, cx);
        // What was dragged is about to land elsewhere; the indices it was
        // selected under will not survive the reload either way.
        self.paginator.update(cx, |paginator, cx| paginator.clear_selection(cx));
    }

    /// Opens the folder's own commands where the listing has no item — the
    /// same menu the toolbar's Add button drops down, at the pointer.
    pub(super) fn open_folder_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(actions) = self.folder_actions.clone() else { return };
        let mobile_size = mobile_context_menu::size(window);
        let menu = PopupMenu::build(window, cx, move |menu, _, _| mobile_context_menu::configure(FolderActions::add_menu_items(actions.clone(), menu), mobile_size));
        self.show_menu(menu, position, window, cx);
    }

    /// Opens the items menu from the keyboard, anchored under the cursor's own
    /// child rather than under a pointer.
    fn open_cursor_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let paginator = self.paginator.read(cx);
        let Some(cursor) = paginator.cursor() else { return false };
        let Some(bounds) = paginator.child_bounds(cursor) else { return false };
        let position = gpui::point(bounds.left(), bounds.bottom());
        self.open_items_menu(cursor, position, window, cx);
        self.chip_menu.is_some()
    }

    /// Tells each book card whether the selection holds it, the way
    /// [`Self::sync_cursor`] tells them about the cursor.
    fn sync_selection(&mut self, cx: &mut Context<Self>) {
        let selection = self.paginator.read(cx).selection().clone();
        let chip_count = self.listing.children().len();
        for (index, book) in self.listing.books().iter().enumerate() {
            book.update(cx, |book, cx| book.set_selected(selection.contains(&(index + chip_count)), cx));
        }
    }

    fn select_option(&mut self, event: BrowseOptionsEvent, cx: &mut Context<Self>) {
        if self.selection.apply(event, self.listing.controls()) {
            self.reload(cx);
        }
    }

    /// Hides the search box and hands the keyboard back to the listing.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.close(cx));
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.read(cx).is_open() {
            self.close_search(window, cx);
        } else {
            self.search.update(cx, |search, cx| search.open(window, cx));
            cx.notify();
        }
    }

    fn close_browse_options_sheet(&mut self, cx: &mut Context<Self>) {
        self.browse_options_sheet_open = false;
        cx.notify();
    }

    fn toggle_browse_options_sheet(&mut self, cx: &mut Context<Self>) {
        self.browse_options_sheet_open = !self.browse_options_sheet_open;
        cx.notify();
    }

    /// Leaves the current folder for the one above it. Already at the root is
    /// not a failure, but it is unhandled, so the key falls through.
    fn open_parent(&mut self, cx: &mut Context<Self>) -> bool {
        // A book's page sits over the folder rather than inside it, so going
        // back closes it before leaving anything.
        if self.close_book_detail(cx) {
            return true;
        }
        if self.path.is_empty() {
            return false;
        }
        let mut path = self.path.clone();
        path.pop();
        self.open_path(path, cx);
        true
    }

    /// Moves the keyboard cursor. On a section card the card takes the move
    /// first — entering it, or moving within it — and only a move off its edge
    /// continues in the outer listing, entering whichever card that lands on.
    fn move_cursor(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        let mut focused = false;
        if let Some(card) = self.cursor_card(cx) {
            focused = card.read(cx).is_focused();
            if card.update(cx, |card, cx| if focused { card.move_cursor(movement, cx) } else { card.enter(movement, cx) }) {
                return true;
            }
        }
        if !self.move_outer_cursor(movement, cx) {
            // Off a focused card and off the listing too: the key is still
            // spent, and the card keeps the keyboard.
            return focused;
        }
        if let Some(card) = self.cursor_card(cx) {
            card.update(cx, |card, cx| card.enter(movement, cx));
        }
        true
    }

    fn move_outer_cursor(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        let left = self.cursor_card(cx);
        if !self.paginator.update(cx, |paginator, cx| paginator.move_cursor(movement, cx)) {
            return false;
        }
        if let Some(card) = left {
            card.update(cx, |card, cx| card.leave(cx));
        }
        self.sync_cursor(cx);
        true
    }

    /// The section card the outer cursor is on, in a sectioned listing.
    fn cursor_card(&self, cx: &App) -> Option<Entity<SectionCard>> {
        let index = self.paginator.read(cx).cursor()?;
        self.section_cards.get(&self.listing.children().get(index)?.id).cloned()
    }

    /// The section card holding the keyboard, if one is.
    fn focused_card(&self, cx: &App) -> Option<Entity<SectionCard>> {
        self.cursor_card(cx).filter(|card| card.read(cx).is_focused())
    }

    /// Tab walks into the cursor's section card and through its rails. Returns
    /// false once it walks out, for the ordinary focus chain to carry on.
    fn tab_through_card(&mut self, shift: bool, cx: &mut Context<Self>) -> bool {
        let Some(card) = self.cursor_card(cx) else { return false };
        card.update(cx, |card, cx| if card.is_focused() { card.tab(shift, cx) } else { card.enter(if shift { CursorMove::Up } else { CursorMove::Down }, cx) })
    }

    fn navigate_paginator(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.read(cx).is_open() && event.keystroke.key == "escape" {
            self.close_search(window, cx);
            cx.stop_propagation();
            return;
        }

        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let search_focused = self.search.read(cx).is_focused(window, cx);
        if self.view == BrowseView::Graph && !search_focused && modifiers == gpui::Modifiers::default() {
            if key == "l" {
                self.toggle_graph_layout(cx);
                cx.stop_propagation();
                return;
            }
            let action = self.graph.key(key);
            if let Some(action) = action {
                match action {
                    GraphTap::Select(trail) => self.open_path(trail, cx),
                    GraphTap::Open => { self.view = BrowseView::Cards; self.sync_card_mode(cx); cx.notify(); }
                }
                cx.stop_propagation();
                return;
            }
            // The card paginator is hidden while the graph is displayed.
            if matches!(key, "left" | "right" | "up" | "down" | "enter" | "backspace" | "pageup" | "pagedown") {
                cx.stop_propagation();
                return;
            }
        }
        let focused_card = self.focused_card(cx);
        if !search_focused && key == "escape"
            && let Some(card) = &focused_card
        {
            card.update(cx, |card, cx| card.leave(cx));
            cx.stop_propagation();
            return;
        }

        let only_shift = modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.platform && !modifiers.function;
        let no_modifiers = modifiers == gpui::Modifiers::default();
        if !search_focused && key == "tab" && (no_modifiers || only_shift) && self.tab_through_card(only_shift, cx) {
            cx.stop_propagation();
            return;
        }

        let control_arrow = modifiers.control && !modifiers.alt && !modifiers.shift && !modifiers.platform && !modifiers.function;
        if !search_focused && control_arrow {
            let movement = match key {
                "left" => Some(CursorMove::Left),
                "right" => Some(CursorMove::Right),
                "up" => Some(CursorMove::Up),
                "down" => Some(CursorMove::Down),
                _ => None,
            };
            if let Some(movement) = movement {
                if self.move_outer_cursor(movement, cx) {
                    cx.stop_propagation();
                }
                return;
            }
        }

        if event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }

        // Arrows move the cursor; the page turns as a consequence of the cursor
        // walking off it. PageUp and PageDown remain the explicit whole-page
        // jump, and are the only way to page without moving the cursor.
        let handled = match key {
            "left" if !search_focused => self.move_cursor(CursorMove::Left, cx),
            "right" if !search_focused => self.move_cursor(CursorMove::Right, cx),
            "up" if !search_focused => self.move_cursor(CursorMove::Up, cx),
            "down" if !search_focused => self.move_cursor(CursorMove::Down, cx),
            // Deferred, because this handler runs inside BrowsePage's own update
            // and a chip's activation calls back into the page to open a folder.
            // A click reaches the same callback from outside any update, which is
            // why only the key path could re-enter. The activation is also taken
            // out of the paginator before being run, for the same reason one step
            // further down: it can navigate somewhere that needs no query, and
            // come straight back to the paginator to install what it found.
            "enter" if !search_focused => match focused_card.as_ref().map(|card| card.read(cx).activation(cx)) {
                Some(Some(activate)) => {
                    window.defer(cx, move |window, cx| activate(window, cx));
                    true
                }
                Some(None) => false,
                None => {
                    let paginator = self.paginator.clone();
                    window.defer(cx, move |window, cx| {
                        if let Some(activate) = paginator.read(cx).cursor_activation() {
                            activate(window, cx);
                        }
                    });
                    true
                }
            },
            // Backspace is guarded on the search field: while typing a filter it
            // has to keep deleting characters rather than leaving the folder.
            "backspace" if !search_focused => self.open_parent(cx),
            // A focused card turns its own rail's page, sideways.
            "pageup" | "pagedown" => {
                let next = key == "pagedown";
                match &focused_card {
                    Some(card) => card.update(cx, |card, cx| card.turn_page(next, cx)),
                    None => self.paginator.update(cx, |paginator, cx| if next { paginator.next_with_axis(PaginatorAxis::Vertical, cx) } else { paginator.previous_with_axis(PaginatorAxis::Vertical, cx) }),
                }
            }
            _ => false,
        };

        if handled {
            cx.stop_propagation();
        }
    }

    /// Opens the menu over the keyboard cursor, for readers who are not holding
    /// a pointer. The menu takes the keyboard from here and walks with the
    /// arrows on its own.
    fn on_open_context_menu(&mut self, _: &OpenContextMenu, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open_cursor_menu(window, cx) {
            cx.propagate();
        }
    }

    /// Takes back the last library action — the menu command, drop, or rename
    /// that this library performed, whichever page performed it.
    fn on_undo(&mut self, _: &UndoLibraryAction, _window: &mut Window, cx: &mut Context<Self>) {
        let ui = self.ui.clone();
        let Some(entry) = ui.undo().take() else {
            cx.propagate();
            return;
        };
        let label = entry.label.clone();
        let library = ui.backend().clone();
        cx.spawn(async move |page, cx| {
            let result = crate::library::services::UndoStack::apply(entry, &library).await;
            let _ = page.update_in(cx, |page, window, cx| {
                page.reload(cx);
                match result {
                    Ok(()) => crate::services::notify_info("undo", format!("Undid: {label}"), window, cx),
                    // Undo is an inverse operation, not a rollback: the library
                    // can have moved on since, and then the inverse no longer
                    // fits what is there.
                    Err(error) => crate::services::notify_error("undo-error", format!("Could not undo {label}: {error}"), window, cx),
                }
            });
        })
        .detach();
    }

    fn on_dismiss(&mut self, _: &DismissSettings, window: &mut Window, cx: &mut Context<Self>) {
        // A selection is the shallowest thing Escape can dismiss.
        if self.paginator.update(cx, |paginator, cx| paginator.clear_selection(cx)) {
            return;
        }
        if let Some(actions) = &self.folder_actions {
            if actions.read(cx).has_modal() {
                actions.update(cx, |actions, cx| actions.dismiss_modal(cx));
                return;
            }
        }
        if self.search.read(cx).is_open() {
            self.close_search(window, cx);
            return;
        }
        // Then back one step: out of a book's page, or up a folder.
        if !self.open_parent(cx) {
            cx.propagate();
        }
    }

    /// The trail to the child `child_id`: under the current path, through its
    /// group where the group is a real folder. A search hit instead gets the
    /// full trail its id spells out, where it spells one.
    fn child_path(&self, child_id: &str) -> Option<Vec<BrowseCrumb>> {
        let listing = &self.listing;
        let child = listing.children().iter().chain(listing.chip_groups().iter().map(|group| &group.parent)).chain(listing.sections().iter().flat_map(|section| section.children.iter())).find(|child| child.id == child_id).cloned()?;
        if !self.selection.search.is_empty()
            && let Some(path) = listing.path_to(&child)
        {
            return Some(path);
        }
        let mut path = self.path.clone();
        if self.kind.includes_group_in_path()
            && let Some(section) = listing.sections().iter().find(|section| section.children.iter().any(|row| row.id == child.id))
        {
            path.push(BrowseCrumb { location: section.parent.id.clone(), label: section.parent.name.clone() });
        }
        if self.kind.includes_group_in_path()
            && let Some(group) = listing.chip_groups().iter().find(|group| listing.children()[group.start..group.end].iter().any(|row| row.id == child.id))
        {
            path.push(BrowseCrumb { location: group.parent.id.clone(), label: group.parent.name.clone() });
        }
        path.push(BrowseCrumb { location: child.id, label: child.name });
        Some(path)
    }

    pub(super) fn open_child(&mut self, child_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.child_path(&child_id) else {
            log::warn!("ignored missing {} child {child_id}", self.kind.list_prefix());
            return;
        };
        if !self.selection.search.is_empty() {
            self.selection.search.clear();
            self.search.update(cx, |search, cx| search.clear(window, cx));
        }
        self.load(path, LoadIntent::Navigate, cx);
    }

    fn open_path(&mut self, path: Vec<BrowseCrumb>, cx: &mut Context<Self>) {
        self.load(path, LoadIntent::Navigate, cx);
    }

    fn toggle_view(&mut self, cx: &mut Context<Self>) {
        self.view = match self.view {
            BrowseView::Cards => BrowseView::Graph,
            BrowseView::Graph => BrowseView::Cards,
        };
        self.sync_card_mode(cx);
        self.fetch_graph_path(cx);
        cx.notify();
    }

    fn toggle_graph_layout(&mut self, cx: &mut Context<Self>) {
        let next = match self.graph.layout_kind() {
            components::GraphLayoutKind::Columns => components::GraphLayoutKind::AdaptiveRadial,
            components::GraphLayoutKind::AdaptiveRadial => components::GraphLayoutKind::Columns,
        };
        self.graph.set_layout_kind(next);
        cx.notify();
    }

    /// Fetches the children of the locations above this one that the graph has
    /// not seen, for the fans along its path. The location itself arrives with
    /// every load.
    fn fetch_graph_path(&mut self, cx: &mut Context<Self>) {
        if self.view != BrowseView::Graph {
            return;
        }
        for location in self.graph.take_missing(&self.kind.root_location(), &self.path) {
            let mut query = self.selection.query(location.clone());
            query.search.clear();
            let operation = self.listing.request(&query);
            cx.spawn(async move |page, cx| {
                let result = operation.await;
                let _ = page.update(cx, |page, cx| {
                    match result {
                        Ok(BrowseResponse::Contents { sections, .. }) => page.graph.remember(&location, sections.into_iter().map(|section| section.parent).collect()),
                        Err(error) => {
                            log::warn!("Graph fetch failed for {location}: {error}");
                            page.graph.forget_fetch(&location);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn graph_tap(&mut self, world: Point<Pixels>, cx: &mut Context<Self>) {
        match self.graph.tap(world, cx) {
            Some(GraphTap::Select(trail)) => self.open_path(trail, cx),
            Some(GraphTap::Open) => {
                self.view = BrowseView::Cards;
                self.sync_card_mode(cx);
                cx.notify();
            }
            None => {}
        }
    }

    fn graph_hover(&mut self, world: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        if self.graph.hover(world, cx) { cx.notify(); }
    }

    fn load(&mut self, path: Vec<BrowseCrumb>, intent: LoadIntent, cx: &mut Context<Self>) {
        let location = path.last().map(|crumb| crumb.location.clone()).unwrap_or_else(|| self.kind.root_location());
        self.load_at(path, location, intent, cx);
    }

    /// Loads `location` under the breadcrumb trail `path`. Navigating anywhere
    /// leaves a book's page that is open over the grid.
    fn load_at(&mut self, path: Vec<BrowseCrumb>, location: String, intent: LoadIntent, cx: &mut Context<Self>) {
        if intent == LoadIntent::Navigate {
            self.close_book_detail(cx);
        }
        if location != self.location {
            if let Some(actions) = &self.folder_actions {
                actions.update(cx, |actions, cx| actions.cancel_move_selection(cx));
            }
        }
        let query = self.selection.query(location.clone());
        #[cfg(target_os = "android")]
        log::info!("Browse request: location={} formats={:?} languages={:?} search_length={}", location, query.formats, query.languages, query.search.len());
        let pending = PendingLoad { location, path, intent };
        let generation = self.request.begin();
        // Navigation keeps the current frame painted until the next one is
        // ready. A browse query is a local read that returns inside a frame,
        // so covering the grid in the meantime only ever produced a flash —
        // one that appeared going back up the tree and not going down it, for
        // the same query against the same database.
        let operation = self.listing.request(&query);
        self.error = None;
        self.request.install(cx.spawn(async move |page, cx| {
            let result = operation.await;
            let _ = page.update(cx, |page, cx| page.finish_request(generation, pending, result, cx));
        }));
    }

    fn finish_request(&mut self, generation: u64, pending: PendingLoad, result: Result<BrowseResponse, String>, cx: &mut Context<Self>) {
        if !self.request.is_current(generation) {
            return;
        }
        self.loaded = true;
        match result {
            Ok(response) => {
                let changing_location = self.location != pending.location;
                if changing_location {
                    let (cursor, child_anchor) = {
                        let paginator = self.paginator.read(cx);
                        (paginator.cursor(), paginator.child_anchor())
                    };
                    // Books are declared after chips, so a cursor past the last
                    // chip is on a book — which is the case that matters when
                    // the destination being left for is that book's own page.
                    let children = self.listing.children();
                    let saved = cursor.and_then(|index| match children.get(index) {
                        Some(chip) => Some(SavedCursor::Chip(chip.id.clone())),
                        None => {
                            let book = self.listing.books();
                            let book = book.get(index.checked_sub(children.len())?)?;
                            Some(SavedCursor::Book(book.read(cx).content_hash()))
                        }
                    });
                    match saved {
                        Some(cursor) => {
                            self.selected_chips.insert(self.location.clone(), SavedChipSelection { cursor, child_anchor });
                        }
                        None => {
                            self.selected_chips.remove(&self.location);
                        }
                    }
                }
                let is_location = matches!(response, BrowseResponse::Contents { .. });
                let authoritative_path = self.listing.install(response, cx);
                self.location = pending.location;
                self.path = authoritative_path.unwrap_or(pending.path);
                if is_location {
                    self.graph.remember(&self.location, self.listing.children().to_vec());
                }
                self.fetch_graph_path(cx);
                self.error = None;
                // Only where a folder command could act. A location that is not
                // a folder — a book's own page — leaves these pointed at the
                // folder it was opened from, which is where adding or importing
                // still belongs. Telling them otherwise was rejected anyway, and
                // logged on every refresh.
                if let Some(actions) = &self.folder_actions
                    && DirId::parse_str(&self.location).is_ok()
                {
                    let name = self.path.last().map(|crumb| crumb.label.clone());
                    actions.update(cx, |actions, cx| actions.set_location(&self.location, name, cx));
                }
                #[cfg(target_os = "android")]
                log::info!("Browse loaded: location={} children={} books={}", self.location, self.listing.children().len(), self.listing.books().len());
                self.sync_card_mode(cx);
                let groups = self.paginator_groups(cx);
                let graph_cover_groups = self.graph_cover_groups();
                // Refresh in place; navigation restores a remembered chip and
                // page anchor after installing the destination's children.
                let preserve_position = matches!(pending.intent, LoadIntent::Refresh);
                let restored_cursor = changing_location.then(|| self.selected_chips.get(&self.location)).flatten().and_then(|saved| {
                    let index = match &saved.cursor {
                        SavedCursor::Chip(chip_id) => self.listing.children().iter().position(|chip| &chip.id == chip_id)?,
                        SavedCursor::Book(content_hash) => self.listing.children().len() + self.listing.books().iter().position(|book| book.read(cx).content_hash() == *content_hash)?,
                    };
                    Some((index, saved.child_anchor))
                });
                self.paginator.update(cx, |paginator, cx| {
                    if preserve_position {
                        paginator.refresh_groups(groups, cx);
                    } else {
                        paginator.set_groups(groups, cx);
                    }
                    if pending.intent.reveals_end() {
                        paginator.show_end(cx);
                    }
                    if let Some((index, child_anchor)) = restored_cursor {
                        paginator.restore_cursor(index, child_anchor, cx);
                    }
                });
                self.graph_cover_paginator.update(cx, |paginator, cx| {
                    if preserve_position {
                        paginator.refresh_groups(graph_cover_groups, cx);
                    } else {
                        paginator.set_groups(graph_cover_groups, cx);
                    }
                });
                // Replacing groups resets the keyboard cursor; refreshing can
                // clamp it. Publish that state to chip and book highlights too.
                self.sync_cursor(cx);
            }
            Err(error) => {
                log::warn!("Browse failed for {}: {}", pending.location, error);
                self.error = Some(error.into());
            }
        }
        cx.notify();
    }

    fn breadcrumb(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let page = cx.entity();
        let mut root = components::folder_breadcrumb_root_item(self.kind.root_label(), self.path.is_empty(), theme);
        if !self.path.is_empty() {
            let root_page = page.clone();
            root = root.on_click(move |_, _, cx| root_page.update(cx, |page, cx| page.open_path(Vec::new(), cx)));
        }
        let mut breadcrumb = components::folder_breadcrumb().child(Self::breadcrumb_drop_target(root, sync_common::ROOT_DIR_ID, self.kind.root_label(), page.downgrade(), theme));
        for (index, crumb) in self.path.iter().enumerate() {
            let current = index + 1 == self.path.len();
            let mut item = components::folder_breadcrumb_item(crumb.label.clone(), current, theme);
            if !current {
                let path = self.path[..=index].to_vec();
                let path_page = page.clone();
                item = item.on_click(move |_, _, cx| path_page.update(cx, |page, cx| page.open_path(path.clone(), cx)));
            }
            if let Ok(destination) = DirId::parse_str(&crumb.location) {
                item = Self::breadcrumb_drop_target(item, destination, crumb.label.clone().into(), page.downgrade(), theme);
            }
            breadcrumb = breadcrumb.child(item);
        }
        components::folder_breadcrumb_trail(breadcrumb).into_any_element()
    }

    /// An ancestor folder is exactly as valid a destination for a drag as one
    /// shown in the grid below it, so each breadcrumb segment takes drops, and
    /// shows it will, the way a folder chip does.
    fn breadcrumb_drop_target(item: BreadcrumbItem, destination: DirId, name: SharedString, page: WeakEntity<Self>, theme: components::BrowserTheme) -> BreadcrumbItem {
        item.with_interaction(move |item| folder_drop_target(item, destination, name.clone(), true, page.clone(), theme))
    }

    fn paginator_groups(&mut self, cx: &mut Context<Self>) -> Vec<PaginatorGroup> {
        let mut cards = HashMap::new();
        if self.uses_section_layout() {
            let page = cx.entity().downgrade();
            for section in self.listing.sections() {
                let id = section.parent.id.clone();
                let card = match self.section_cards.remove(&id) {
                    Some(card) => {
                        card.update(cx, |card, cx| card.set_section(section, cx));
                        card
                    }
                    None => cx.new(|cx| SectionCard::new(page.clone(), format!("{}-section-{id}", self.kind.list_prefix()), self.kind.child_icon(), section, cx)),
                };
                cards.insert(id, card);
            }
        }
        self.section_cards = cards;
        // Only a folder listing can be dragged: its children are directories,
        // and its books sit in a folder a move can name as their source.
        let source = self.folder_actions.is_some().then(|| DirId::parse_str(&self.location).ok()).flatten();
        super::layout::paginator_groups(
            cx.entity().downgrade(),
            self.listing.children(),
            self.listing.chip_groups(),
            self.kind.child_icon(),
            self.listing.books(),
            self.detailed_cards,
            crate::stores::current_cover_text(cx),
            source,
            &self.section_cards,
            self.kind == BrowseKind::Folder && self.location == ROOT_DIR_ID.to_string(),
            self.open_trash.clone(),
        )
    }

    fn uses_section_layout(&self) -> bool {
        SECTION_LAYOUT && self.selection.search.is_empty()
    }

    /// Tells every card which mode it is in. Cards outlive a load — a book still
    /// in the folder keeps its entity — but new ones arrive at the default, so
    /// this runs after each install as well as on the switch itself.
    fn sync_card_mode(&mut self, cx: &mut Context<Self>) {
        for book in self.listing.books().iter() {
            book.update(cx, |book, cx| {
                book.set_detailed(self.view == BrowseView::Cards && self.detailed_cards, cx);
                book.set_cover_only(self.view == BrowseView::Graph, cx);
                book.set_cover_top_aligned(self.view == BrowseView::Graph, cx);
            });
        }
    }

    /// Tells each book card whether it holds the paginator's cursor. Chips read
    /// the cursor as they draw; cards are entities, and are told.
    ///
    /// Books are declared after chips, so the flat child index of a book is its
    /// position plus the chip count.
    fn sync_cursor(&mut self, cx: &mut Context<Self>) {
        let cursor = self.paginator.read(cx).cursor();
        let chip_count = self.listing.children().len();
        let focused_book = cursor.and_then(|index| index.checked_sub(chip_count));
        for (index, book) in self.listing.books().iter().enumerate() {
            book.update(cx, |book, cx| book.set_focused(focused_book == Some(index), cx));
        }
    }

    fn navigation_heading(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        if components::uses_mobile_navigation(window) {
            let title = if self.view == BrowseView::Graph { String::new() } else { self.path.last().map(|crumb| crumb.label.clone()).unwrap_or_else(|| self.kind.root_label().to_string()) };
            let back = (!components::has_native_back_button()).then(|| components::mobile_back_button(components::browser_theme(cx)).on_click(|_, window, cx| window.dispatch_action(Box::new(DismissSettings), cx)));
            components::mobile_navigation_heading(title, back)
        } else {
            components::browser_topbar_left().when(self.view == BrowseView::Cards, |heading| heading.child(self.breadcrumb(cx)))
        }
    }

    fn render_topbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let page = cx.entity();
        let search_page = page.clone();
        let search_field = self.search.read(cx).render();
        let search_open = search_field.is_some();
        let inline_search_field = if self.view == BrowseView::Cards { search_field } else { None };
        let search_button_label = if search_open { "Hide search" } else { "Search books or authors" };
        let browse_controls = self.listing.controls().clone();
        let has_options = !browse_controls.collection_sorts.is_empty() || !browse_controls.book_sorts.is_empty() || !browse_controls.formats.is_empty() || !browse_controls.languages.is_empty() || browse_controls.supports_direct_child_books;
        let compact = components::WindowWidthClass::for_window(window).is_compact();
        let close_split_page = page.clone();
        let split_page = page.clone();
        let view_page = page.clone();
        let (view_label, view_icon) = match self.view {
            BrowseView::Cards => ("Show as graph", IconName::Network),
            BrowseView::Graph => ("Show as cards", IconName::LayoutDashboard),
        };
        let graph_layout_switch = (self.view == BrowseView::Graph).then(|| self.graph_layout_switch(cx));
        // In the graph the controls float at the bottom, so their menus open upward.
        let menu_anchor = if self.view == BrowseView::Graph { gpui::Anchor::BottomRight } else { gpui::Anchor::TopRight };
        let controls = components::browser_topbar_right()
            .when(inline_search_field.is_some(), |controls| controls.flex_1())
            .children(inline_search_field.map(|field| div().flex_1().min_w(rems(8.0)).child(field)))
            .child(
                components::outlined_icon_button("toggle-browse-search", search_button_label, IconName::Search, theme)
                    .when(search_open || !self.selection.search.is_empty(), |button| button.bg(theme.accent).text_color(theme.accent_text))
                    .on_click(move |_, window, cx| search_page.update(cx, |page, cx| page.toggle_search(window, cx))),
            )
            .child(components::outlined_icon_button("toggle-browse-view", view_label, view_icon, theme).on_click(move |_, _, cx| view_page.update(cx, |page, cx| page.toggle_view(cx))))
            .children(graph_layout_switch)
            .children(self.folder_actions.as_ref().map(|actions| FolderActions::render_trigger(actions.clone(), menu_anchor, cx)))
            .when(self.can_split(window), |controls| {
                controls.child(
                    components::outlined_icon_button("split-browse-page", "Open split pane", IconName::PanelLeft, theme)
                        .debug_selector(|| "split-browse-button".into())
                        .on_click(move |_, _, cx| split_page.update(cx, |page, cx| page.request_split(BrowseSplitSide::Right, cx))),
                )
            })
            .when(self.split_close_enabled && self.can_split(window), |controls| {
                controls.child(components::outlined_icon_button("close-split-pane", "Close split pane", IconName::Close, theme).on_click(move |_, _, cx| close_split_page.update(cx, |page, cx| page.request_close_split(cx))))
            })
            .when(has_options, |controls| {
                controls.child(browse_options_control(
                    page.clone(),
                    browse_controls,
                    self.selection.options(),
                    |page: &mut Self, event, cx| page.select_option(event, cx),
                    compact,
                    |page: &mut Self, cx| page.toggle_browse_options_sheet(cx),
                    menu_anchor,
                    theme,
                ))
            });
        if self.view == BrowseView::Graph {
            div().h(gpui::rems(components::TOPBAR_HEIGHT_REM)).flex_none().flex().items_center()
                .p(px(4.0)).rounded(px(8.0)).bg(theme.page_bg).border_1().border_color(theme.rule)
                .child(controls)
                .into_any_element()
        } else {
            components::browser_topbar(theme).child(self.navigation_heading(window, cx)).child(controls).into_any_element()
        }
    }

    fn render_browse_options_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.browse_options_sheet_open {
            return None;
        }
        let theme = components::browser_theme(cx);
        let page = cx.entity();
        let browse_controls = self.listing.controls().clone();
        Some(browse_options_sheet(page, browse_controls, self.selection.options(), |page: &mut Self, event, cx| page.select_option(event, cx), |page: &mut Self, cx| page.close_browse_options_sheet(cx), theme))
    }

    pub(super) fn can_split(&self, window: &Window) -> bool {
        crate::split_panes_available(window)
    }

    pub(super) fn set_split_close_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.split_close_enabled = enabled;
        cx.notify();
    }

    /// Rebuilds the grid's row height for the current "cover text" setting.
    ///
    /// The cards themselves pick the change up on their own — see
    /// `BookCardView`'s own global observer — but the row height the
    /// paginator reserves for them is decided here, and has to be recomputed
    /// alongside them or the grid keeps the old gap under a card that no
    /// longer has anything in it.
    fn refresh_cover_text(&mut self, cx: &mut Context<Self>) {
        let groups = self.paginator_groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.refresh_groups(groups, cx));
        cx.notify();
    }

    pub(crate) fn set_library_view(&mut self, detailed: bool, cx: &mut Context<Self>) {
        if self.detailed_cards == detailed {
            return;
        }
        self.detailed_cards = detailed;
        self.sync_card_mode(cx);
        let groups = self.paginator_groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.refresh_groups(groups, cx));
        self.sync_cursor(cx);
        cx.notify();
    }

    pub(crate) fn request_split(&mut self, side: BrowseSplitSide, cx: &mut Context<Self>) {
        cx.emit(BrowseSplitRequest { path: self.path.clone(), side });
    }

    pub(crate) fn request_split_child(&mut self, child_id: String, side: BrowseSplitSide, cx: &mut Context<Self>) {
        if let Some(path) = self.child_path(&child_id) {
            cx.emit(BrowseSplitRequest { path, side });
        }
    }

    pub(crate) fn request_close_split(&mut self, cx: &mut Context<Self>) {
        cx.emit(BrowseSplitClose);
    }

    pub(crate) fn open_split_path(&mut self, path: Vec<BrowseCrumb>, cx: &mut Context<Self>) {
        self.open_path(path, cx);
    }

    /// Shows a book's page over the grid, reusing the kept one when it is for
    /// the same book.
    pub(crate) fn show_book_detail(&mut self, book: library_model::BookCardRow, cover: Option<LoadedCover>, cx: &mut Context<Self>) {
        let ui = self.ui.clone();
        if !self.detail.as_ref().is_some_and(|detail| detail.read(cx).content_hash() == book.content_hash) {
            let page = cx.entity().downgrade();
            let close: CloseBookDetail = Rc::new(move |cx| {
                let _ = page.update(cx, |page, cx| page.close_book_detail(cx));
            });
            self.detail = Some(BookDetailPage::create(book, cover, ui, close, cx));
        }
        self.detail_visible = true;
        cx.notify();
    }

    /// Opens a book's page over the folder at `path`, for a book opened from
    /// outside the grid. A grid already showing that folder is left as it is.
    pub(crate) fn open_book_detail(&mut self, book: library_model::BookCardRow, path: Vec<BrowseCrumb>, cx: &mut Context<Self>) {
        if path != self.path {
            self.open_path(path, cx);
        }
        self.show_book_detail(book, None, cx);
    }

    /// Closes the book's page, back to the grid underneath. Returns whether one
    /// was open.
    pub(crate) fn close_book_detail(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.detail_visible {
            return false;
        }
        self.detail_visible = false;
        self.ui.notify_book_detail_closed(cx);
        cx.notify();
        true
    }

    pub(crate) fn clear_cached_book_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        self.detail_visible = false;
        cx.notify();
    }

    fn graph_cover_groups(&self) -> Vec<PaginatorGroup> {
        if self.listing.books().is_empty() {
            return Vec::new();
        }
        let books = self.listing.books();
        let children = books.iter().cloned().map(|book| {
            let rendered = book.clone();
            let activated = book.clone();
            PaginatorChild::new(move |_, _, _| div().w_full().h_full().min_w_0().child(rendered.clone()).into_any_element())
                .with_activate(move |window, cx| crate::library::widgets::BookCardView::activate(&activated, window, cx))
                .with_prepare(move |window, cx| crate::library::widgets::BookCardView::prefetch_for_window(&book, window, cx))
        });
        vec![PaginatorGroup::new(graph_cover_policy(), children)]
    }

    /// Direct books of the graph selection, sized and paged in the adjacent pane.
    fn graph_cover_grid(&self) -> AnyElement {
        self.graph_cover_paginator.clone().into_any_element()
    }


    fn graph_layout_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let (icon, next_label) = match self.graph.layout_kind() {
            components::GraphLayoutKind::Columns => (IconName::GraphColumns, "Switch to circle graph"),
            components::GraphLayoutKind::AdaptiveRadial => (IconName::GraphCircle, "Switch to column graph"),
        };
        let page = cx.entity();
        components::outlined_icon_button("toggle-graph-layout", next_label, icon, theme)
            .on_click(move |_, _, cx| page.update(cx, |page, cx| page.toggle_graph_layout(cx)))
            .into_any_element()
    }

    fn graph_mobile_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let page = cx.entity();
        let cards_page = page.clone();
        let options_page = page.clone();
        div().flex_none().flex().items_center().gap(px(8.0))
            .p(px(4.0)).rounded(px(8.0)).bg(theme.page_bg).border_1().border_color(theme.rule)
            .child(components::outlined_icon_button("graph-mobile-cards", "Show as cards", IconName::LayoutDashboard, theme)
                .on_click(move |_, _, cx| cards_page.update(cx, |page, cx| page.toggle_view(cx))))
            .child(self.graph_layout_switch(cx))
            .child(components::outlined_icon_button("graph-mobile-options", "Sort and filter", IconName::Settings2, theme)
                .on_click(move |_, _, cx| options_page.update(cx, |page, cx| page.toggle_browse_options_sheet(cx))))
            .into_any_element()
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let page = cx.entity();
        let listing = match self.view {
            BrowseView::Cards => self.paginator.clone().into_any_element(),
            BrowseView::Graph => {
                let tap_page = page.clone();
                let on_tap = move |world, _: &mut Window, cx: &mut App| tap_page.update(cx, |page, cx| page.graph_tap(world, cx));
                let hover_page = page.clone();
                let on_hover = move |world, _: &mut Window, cx: &mut App| hover_page.update(cx, |page, cx| page.graph_hover(world, cx));
                let root_book_count = self.ui.updates().read(cx).book_count().unwrap_or(0);
                let graph = self.graph.render(&self.kind.root_location(), &self.kind.root_label(), root_book_count, &self.path, on_tap, on_hover, window, cx);
                let compact = components::WindowWidthClass::for_window(window).is_compact();
                let covers = self.graph_cover_grid();
                let controls = if compact { self.graph_mobile_controls(cx) } else { self.render_topbar(window, cx) };
                let graph_area = div().relative().flex_1().min_h_0().min_w_0().child(graph)
                    .child(div().absolute().right(px(8.0)).bottom(px(8.0)).child(controls));
                // The grid holds the selection's own books, so with none it is
                // not drawn at all rather than drawn empty. An open search keeps
                // it: the field lives there, and a query with no matches yet
                // must not take the field away mid-word.
                let search_field = self.search.read(cx).render();
                let has_covers = !self.listing.books().is_empty() || search_field.is_some();
                if !has_covers {
                    return graph_area.size_full().into_any_element();
                }
                if compact {
                    let expanded = self.graph_cover_sheet_expanded;
                    let sheet_page = page.clone();
                    let swipe_page = page.clone();
                    let handle_page = page.clone();
                    let handle_swipe_page = page.clone();
                    let cover_sheet = div().relative().w_full()
                        .when(expanded, |sheet| sheet.flex_1().min_h_0())
                        .when(!expanded, |sheet| sheet.h(px(117.0)).flex_none())
                        .bg(theme.page_bg).border_t_1().border_color(theme.rule).flex().flex_col()
                        .child(div().flex_1().min_h_0().child(covers))
                        .when(!expanded, |sheet| sheet.child(div().id("graph-cover-sheet-peek").absolute().top_0().left_0().right_0().bottom_0()
                            .on_click(move |_, _, cx| sheet_page.update(cx, |page, cx| { page.graph_cover_sheet_expanded = true; cx.notify(); }))
                            .on_scroll_wheel(move |event, _, cx| {
                                if let gpui::ScrollDelta::Pixels(delta) = event.delta {
                                    if f32::from(delta.y) < -3.0 {
                                        swipe_page.update(cx, |page, cx| { page.graph_cover_sheet_expanded = true; cx.notify(); });
                                    }
                                }
                                cx.stop_propagation();
                            })))
                        // The handle floats over the top of the grid.
                        .child(div().id("graph-cover-sheet-handle").absolute().top_0().left_0().right_0().h(px(20.0)).flex().items_center().justify_center()
                            .child(div().w(px(32.0)).h(px(3.0)).rounded(px(2.0)).bg(theme.rule))
                            .on_click(move |_, _, cx| handle_page.update(cx, |page, cx| { page.graph_cover_sheet_expanded = !page.graph_cover_sheet_expanded; cx.notify(); }))
                            .on_scroll_wheel(move |event, _, cx| {
                                if let gpui::ScrollDelta::Pixels(delta) = event.delta {
                                    if f32::from(delta.y) > 3.0 {
                                        handle_swipe_page.update(cx, |page, cx| { page.graph_cover_sheet_expanded = false; cx.notify(); });
                                    }
                                }
                            }));
                    div().size_full().flex().flex_col().child(graph_area).child(cover_sheet).into_any_element()
                } else {
                    let sidebar = div().size_full().min_h_0().flex().flex_col()
                        .children(search_field.map(|field| div().w_full().px(px(components::CONTENT_INSET)).pt(px(components::CONTENT_INSET)).pb(px(components::CONTENT_INSET)).child(field)))
                        .child(div().flex_1().min_h_0().child(covers));
                    let sidebar_width = self.graph_sidebar_width;
                    let start_resize = page.clone();
                    let move_resize = page.clone();
                    let finish_resize = page.clone();
                    let finish_resize_out = page.clone();
                    div().relative().size_full().flex().flex_row()
                        .child(graph_area)
                        .child(div().relative().flex_none().w(px(sidebar_width)).max_w(gpui::relative(0.65)).min_h_0()
                            .child(sidebar)
                            .child(div().id("graph-sidebar-resize-handle").absolute().top_0().left(px(-4.0)).h_full().w(px(9.0)).cursor_col_resize().flex().justify_center().child(div().h_full().w(px(1.0)).bg(theme.rule)).on_mouse_down(
                                gpui::MouseButton::Left,
                                move |event, _, cx| {
                                    cx.stop_propagation();
                                    start_resize.update(cx, |page, _| {
                                        page.graph_sidebar_drag = Some((f32::from(event.position.x), page.graph_sidebar_width));
                                    });
                                },
                            )))
                        .on_mouse_move(move |event, window, cx| {
                            move_resize.update(cx, |page, cx| {
                                if let Some((start_x, start_width)) = page.graph_sidebar_drag {
                                    if event.pressed_button == Some(gpui::MouseButton::Left) {
                                        // The narrowest sidebar still holds its topbar's
                                        // controls, which are rems, so the floor is too.
                                        let minimum = f32::from(window.rem_size()) * GRAPH_SIDEBAR_MINIMUM_WIDTH_REM;
                                        let width = (start_width + start_x - f32::from(event.position.x)).clamp(minimum, 720.0_f32.max(minimum));
                                        if (page.graph_sidebar_width - width).abs() >= 0.5 {
                                            page.graph_sidebar_width = width;
                                            cx.notify();
                                        }
                                    } else {
                                        page.graph_sidebar_drag = None;
                                    }
                                }
                            });
                        })
                        .on_mouse_up(gpui::MouseButton::Left, move |_, _, cx| {
                            finish_resize.update(cx, |page, _| page.graph_sidebar_drag = None);
                        })
                        .on_mouse_up_out(gpui::MouseButton::Left, move |_, _, cx| {
                            finish_resize_out.update(cx, |page, _| page.graph_sidebar_drag = None);
                        })
                        .into_any_element()
                }
            }
        };
        let paginator = self.paginator.read(cx);
        let scanning_empty_library = paginator.is_empty() && self.folder_actions.as_ref().is_some_and(|actions| actions.read(cx).scanner_running(cx));
        let waiting_for_library = paginator.is_empty() && self.folder_actions.as_ref().is_some_and(|actions| actions.read(cx).library_loading(cx));
        // Only the states that are genuinely a wait: a library still opening,
        // and a scan that has not yet found anything. Neither is a browse
        // request, and there is nothing painted underneath either to keep.
        let show_loading = (scanning_empty_library || waiting_for_library) && self.error.is_none();
        let loading_label = if scanning_empty_library { "Scanning library…" } else { "Loading…" };
        let mut body = div().relative().size_full().min_h_0().min_w_0();
        // Open space is the folder being browsed, and takes drops into it.
        if let Ok(destination) = DirId::parse_str(&self.location) {
            // Not outlined: it holds every other target, and the one under
            // the pointer is the one to show.
            let name = self.path.last().map_or_else(|| self.kind.root_label(), |crumb| crumb.label.clone().into());
            body = folder_drop_target(body, destination, name, false, page.downgrade(), theme);
        }
        let body = body
            .child(listing)
            .when(show_loading, |body| body.child(components::absolute_full_size().bg(theme.page_bg).flex().items_center().justify_center().child(components::browser_loading_message(loading_label, theme))))
            .into_any_element();
        if let Some(error) = self.error.clone() {
            let retry_page = page.clone();
            let retry = components::topbar_action_button("retry-section", "Retry", IconName::Redo2, theme).on_click(move |_, _, cx| retry_page.update(cx, |page, cx| page.reload(cx)));
            components::browser_error_banner(error, retry, body).into_any_element()
        } else {
            body
        }
    }

    fn render_shell(&self, topbar: AnyElement, body: AnyElement, options_sheet: Option<AnyElement>, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let folder_overlay = self.folder_actions.as_ref().and_then(|actions| FolderActions::render_overlay(actions.clone(), cx));
        let chip_menu = self.chip_menu.as_ref().map(|active| mobile_context_menu::render(active.menu.clone(), active.position, window));
        components::full_size_column()
            .track_focus(&self.focus)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|page, _, window, cx| {
                    if !page.focus.contains_focused(window, cx) {
                        page.focus.focus(window, cx);
                    }
                }),
            )
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_open_context_menu))
            .on_action(cx.listener(Self::on_undo))
            .on_key_down(cx.listener(Self::navigate_paginator))
            .child(topbar)
            .child(components::remaining_space_column().child(body))
            .children(chip_menu)
            .children(folder_overlay)
            .children(options_sheet)
    }
}

impl Render for BrowsePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(detail) = self.detail.clone().filter(|_| self.detail_visible) {
            let theme = components::browser_theme(cx);
            let page = cx.entity();
            let close = components::outlined_icon_button("close-book-detail", "Close", IconName::Close, theme).on_click(move |_, _, cx| {
                page.update(cx, |page, cx| page.close_book_detail(cx));
            });
            let body = div().relative().size_full().child(detail).child(div().absolute().top(px(12.0)).right(px(12.0)).child(close)).into_any_element();
            self.render_shell(div().h(px(0.0)).flex_none().into_any_element(), body, None, window, cx)
        } else {
            let topbar = if self.view == BrowseView::Graph {
                div().h(px(0.0)).flex_none().into_any_element()
            } else {
                self.render_topbar(window, cx)
            };
            let body = self.render_body(window, cx);
            let options_sheet = self.render_browse_options_sheet(cx);
            self.render_shell(topbar, body, options_sheet, window, cx)
        }
    }
}

impl gpui::Focusable for BrowsePage {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}
