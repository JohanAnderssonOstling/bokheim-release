//! A browse page: a navigator and the views of what it lists, with the
//! chrome around them. Every kind has the cards; the trees also have a graph,
//! and folders the commands that go with real directories.

use ui_components::user_facing_error;

use crate::services::{import_catalog_book_with_progress, notify_error};
use crate::{BrowsePageSession, NavigationSession, NewWindowTarget, PaneRequest};
use std::rc::Rc;
use std::time::Duration;

use app::host::startup_profile::StartupProfile;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, DismissEvent, Div, Entity, FocusHandle, Focusable as _, IntoElement, KeyDownEvent, Pixels, Point, Render, SharedString, Stateful, Subscription, Task, Window, div, px, rems};
use gpui_component::IconName;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::menu::PopupMenu;
use library_model::{BookCardRow, BookLocator, BrowseLocation, BrowsePathSegment};
use sync_common::{BookId, DirId, ROOT_DIR_ID};
use ui_components as components;

use super::cards::{CardEvent, CardEvents, CardView};
use super::clipboard::{BookClipboard, FocusedBookClipboard};
use super::controls::{BrowseControls, BrowseOptionsEvent, BrowseOptionsSelection, browse_language_control, browse_options_control, browse_options_sheet};
use super::detail::{BookDetailPage, CloseBookDetail};
use super::drag_drop::{FolderDrop, folder_drop_target, import_drop_target};
use super::feed_actions::{FeedActions, FeedsChanged};
use super::folder_actions::{FolderActions, FolderActionsEvent, FolderTargets};
use super::graph::{BrowseGraph, GraphTap};
use super::listing::{BrowseDestination, BrowseItem, BrowseKind, BrowseListing, can_download};
use super::navigator::{BrowseNavigator, Loaded};
use super::profile::Stage;
use crate::library::pages::with_retry_banner;
use crate::library::services::{FolderPlacement, LibraryContext, LibraryScanChanged};
use crate::library::widgets::mobile_context_menu::ContextMenu;
use crate::library::widgets::{BookCardEvent, BookCardEvents, BookCardView, LoadedCover, SelectionAction};
use crate::{CopyBooks, CutBooks, DismissSettings, OpenContextMenu, PasteBooks, ToggleGridSelection, UndoLibraryAction};

pub(crate) use ui_components::SplitSide as BrowseSplitSide;

/// How the page shows its location: as cards, or as a graph whose column or
/// circle layout is chosen separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowseView {
    Cards,
    Graph,
}

pub(crate) struct BrowsePage {
    ui: LibraryContext,
    navigator: Entity<BrowseNavigator>,
    cards: Entity<CardView>,
    /// How the cards reach the page; also given to the drop targets the page
    /// draws itself.
    card_events: CardEvents,
    /// The graph, on the kinds that are trees.
    graph: Option<Entity<BrowseGraph>>,
    view: BrowseView,
    /// Folder commands and the shared copy picker on All books.
    folder_actions: Option<Entity<FolderActions>>,
    /// Adding and removing OPDS feeds, on the catalog.
    feed_actions: Option<Entity<FeedActions>>,
    /// The book page shown over the location, and its close requests.
    detail: Option<(Entity<BookDetailPage>, Subscription)>,
    chip_menu: Option<ContextMenu>,
    focus: FocusHandle,
    restore_focus: bool,
    search_input: Option<Entity<InputState>>,
    search_open: bool,
    search_debounce: Option<Task<()>>,
    /// The compact bottom-sheet counterpart to the desktop sort/filter dropdown.
    browse_options_sheet_open: bool,
    _subscriptions: Vec<Subscription>,
    /// Keep the requested trail while its asynchronous listing is still loading.
    session_path: Vec<BrowsePathSegment>,
    restoring_detail: Option<BookId>,
    restore_task: Option<Task<()>>,
    profile_render_pending: Option<Stage>,
}

impl gpui::EventEmitter<PaneRequest> for BrowsePage {}

impl BrowsePage {
    pub(crate) fn new(kind: BrowseKind, ui: LibraryContext, cx: &mut Context<Self>) -> Self {
        let folder_actions = matches!(kind, BrowseKind::Folder | BrowseKind::Books).then(|| cx.new(|_| FolderActions::new(ui.clone())));
        // OPDS feeds are native only; the browser lists none.
        let feed_actions = (kind == BrowseKind::Catalog && cfg!(not(target_arch = "wasm32"))).then(|| cx.new(|cx| FeedActions::new(ui.clone(), cx)));
        // Weak, because the page holds the cards that hold this.
        let page = cx.weak_entity();
        let book_events: BookCardEvents = Rc::new(move |event, _, cx| {
            let _ = page.update(cx, |page, cx| page.handle_book_event(event, cx));
        });
        let listing = BrowseListing::new(ui.clone(), kind, book_events, kind == BrowseKind::Folder);
        let navigator = cx.new(|cx| BrowseNavigator::new(ui.clone(), kind, listing, cx));
        let page = cx.weak_entity();
        let card_events: CardEvents = Rc::new(move |event, window, cx| {
            let _ = page.update(cx, |page, cx| page.handle_card_event(event, window, cx));
        });
        let cards = cx.new(|cx| CardView::new(navigator.clone(), folder_actions.is_some() || kind == BrowseKind::Catalog, card_events.clone(), cx));
        let graph = kind.graph_layout().map(|layout| cx.new(|cx| BrowseGraph::new(navigator.clone(), layout, cx)));
        let updates = ui.updates();
        let mut subscriptions = vec![
            cx.observe(&navigator, |_, _, cx| cx.notify()),
            cx.observe(&cards, |_, _, cx| cx.notify()),
            cx.subscribe(&navigator, |page, navigator, _: &Loaded, cx| {
                page.session_path = navigator.read(cx).destination.path.clone();
                StartupProfile::new("browse").mark("listing_installed");
                if log::log_enabled!(target: "browse_profile", log::Level::Debug) || StartupProfile::enabled() {
                    if let Some(previous) = page.profile_render_pending.replace(Stage::deferred("listing installation to rendered frame")) {
                        previous.cancel();
                    }
                }
            }),
            // Only repaints: the empty listing reads whether a scan is running.
            cx.subscribe(&updates, |_, _, _: &LibraryScanChanged, cx| cx.notify()),
        ];
        if let Some(graph) = &graph {
            subscriptions.push(cx.observe(graph, |_, _, cx| cx.notify()));
            subscriptions.push(cx.subscribe(graph, |page, _, tap: &GraphTap, cx| match tap {
                GraphTap::Select(path) => page.open_path(path.clone(), cx),
                GraphTap::Open => page.set_view(BrowseView::Cards, cx),
            }));
        }
        if let Some(actions) = &folder_actions {
            subscriptions.push(cx.subscribe(actions, |page, _, event: &FolderActionsEvent, cx| page.handle_folder_action(event.clone(), cx)));
        }
        if let Some(actions) = &feed_actions {
            subscriptions.push(cx.observe(actions, |_, _, cx| cx.notify()));
            subscriptions.push(cx.subscribe(actions, |page, _, _: &FeedsChanged, cx| page.reload(cx)));
        }
        Self {
            ui,
            navigator,
            cards,
            card_events,
            graph,
            view: BrowseView::Cards,
            folder_actions,
            feed_actions,
            detail: None,
            chip_menu: None,
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            restore_focus: false,
            search_input: None,
            search_open: false,
            search_debounce: None,
            browse_options_sheet_open: false,
            _subscriptions: subscriptions,
            session_path: Vec::new(),
            restoring_detail: None,
            restore_task: None,
            profile_render_pending: None,
        }
    }

    /// The graph, while it is the view shown.
    fn shown_graph(&self) -> Option<&Entity<BrowseGraph>> {
        self.graph.as_ref().filter(|_| self.view == BrowseView::Graph)
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.navigator.update(cx, |navigator, cx| navigator.reload(cx));
    }

    fn handle_card_event(&mut self, event: CardEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            CardEvent::OpenChild(location) => self.open_child(location, window, cx),
            CardEvent::SplitChild(location, side) => self.request_split_child(location, side, cx),
            CardEvent::ItemsMenu(items, position) => {
                self.open_items_menu(items, position, window, cx);
            }
            CardEvent::SpaceMenu(position) => self.open_folder_menu(position, window, cx),
            CardEvent::Download(location) => self.download_child(location, cx),
            CardEvent::DropOnto(destination, contents) => self.drop_onto_folder(destination, contents, window, cx),
        }
    }

    fn handle_book_event(&mut self, event: BookCardEvent, cx: &mut Context<Self>) {
        match (event, self.folder_actions.clone()) {
            (BookCardEvent::OpenDetail(book, cover), _) => self.show_book_detail(book, cover, cx),
            (BookCardEvent::Place(books, source, placement), Some(actions)) => actions.update(cx, |actions, cx| actions.open_placement_picker(placement, FolderTargets { books, source, ..FolderTargets::default() }, cx)),
            (BookCardEvent::Remove(book, source), Some(actions)) => actions.update(cx, |actions, cx| actions.remove_books_from_folder(FolderTargets { books: vec![book], source: Some(source), ..FolderTargets::default() }, cx)),
            // Cards offer the folder commands only on a page that has them.
            (_, None) => {}
        }
    }

    fn download_child(&mut self, location: BrowseLocation, cx: &mut Context<Self>) {
        let navigator = self.navigator.read(cx);
        if !navigator.listing.child(location).is_some_and(can_download) {
            return;
        }
        let query = navigator.selection.backend_query(location, false);
        let library = self.ui.backend().clone();
        cx.spawn(async move |page, cx| {
            let result = library.download_browse(query).await;
            let _ = page.update_in(cx, |page, window, cx| {
                if let Err(error) = result {
                    notify_error("hierarchy-download-error", format!("Could not queue downloads. {}", user_facing_error(&error, "Please try again.")), window, cx);
                }
                page.navigator.update(cx, |navigator, cx| navigator.refresh(cx));
            });
        })
        .detach();
    }

    fn handle_folder_action(&mut self, event: FolderActionsEvent, cx: &mut Context<Self>) {
        let FolderActionsEvent::Moved(folders) = event;
        if matches!(self.navigator.read(cx).destination.location, BrowseLocation::Folder(id) if folders.contains(&id)) {
            self.open_path(Vec::new(), cx);
        }
    }

    /// Every entry point uses the same resolved rows, placements and availability.
    fn open_items_menu(&mut self, items: Vec<BrowseItem>, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // One book opens its overlay, in a folder too: its own commands
        // include the folder's.
        if let [BrowseItem::Book(book)] = items.as_slice() {
            BookCardView::open_items_menu(std::slice::from_ref(book), position, window, cx);
            return true;
        }
        if self.navigator.read(cx).destination.location.folder().is_none() && !items.is_empty() && items.iter().all(|item| matches!(item, BrowseItem::Book(_))) {
            let books = items.iter().filter_map(BrowseItem::book).cloned().collect::<Vec<_>>();
            BookCardView::open_items_menu(&books, position, window, cx);
            return true;
        }
        let single_chip = match items.as_slice() {
            [item] => item.child(),
            _ => None,
        };
        let remove_source = self.feed_actions.clone().zip(single_chip).and_then(|(actions, chip)| FeedActions::removal_action(actions, chip.id, chip.name.clone()));
        // A saved store opens a separate window rather than a browse page.
        let single_chip = single_chip.filter(|chip| !matches!(chip.id, BrowseLocation::Store(_)) || remove_source.is_none());
        let split_target = single_chip.filter(|_| crate::split_panes_available(window)).map(|chip| chip.id);
        let source = self.navigator.read(cx).destination.location.folder();
        let mut commands = self.selection_actions(source, &items, cx).into_iter().peekable();
        let new_window = commands.next_if(|action| action.id == "new-window");
        let download_action = commands.next_if(|action| action.id == "download");
        let folder_commands = commands.collect::<Vec<_>>();
        if new_window.is_none() && split_target.is_none() && download_action.is_none() && folder_commands.is_empty() && remove_source.is_none() {
            return false;
        }
        let page = cx.entity();
        self.show_menu(position, window, cx, move |mut menu| {
            if let Some(action) = &new_window {
                menu = action.menu_item(menu);
            }
            if let Some(child_id) = split_target {
                let page = page.clone();
                menu = menu.item(components::menu_item_with_split_actions("Open left", "Open right", move |side, _, cx| {
                    page.update(cx, |page, cx| {
                        page.request_split_child(child_id, side, cx);
                        page.dismiss_menu(cx);
                    })
                }));
            }
            if let Some(action) = &download_action {
                menu = action.menu_item(menu);
            }
            if let Some(action) = &remove_source {
                menu = action.menu_item(menu);
            }
            if !folder_commands.is_empty() && (new_window.is_some() || split_target.is_some() || download_action.is_some()) {
                menu = menu.separator();
            }
            folder_commands.iter().fold(menu, |menu, action| action.menu_item(menu))
        });
        true
    }

    fn dismiss_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.chip_menu.take().is_none() {
            return false;
        }
        self.restore_focus = !self.has_popup(cx);
        cx.notify();
        true
    }

    /// Opens the page's one context menu at `position`, built by `items`.
    fn show_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>, items: impl Fn(PopupMenu) -> PopupMenu + 'static) {
        self.chip_menu = Some(ContextMenu::new(position, window, cx, items, |menu, window, cx| {
            cx.subscribe_in(menu, window, |page, menu, _: &DismissEvent, window, cx| {
                let owned_focus = menu.focus_handle(cx).contains_focused(window, cx);
                page.dismiss_menu(cx);
                // A menu action can focus a window-level dialog or another view
                // as well as one of this page's own popups.
                page.restore_focus &= owned_focus;
            })
        }));
        cx.notify();
    }

    /// Imports paths dropped from the desktop into the resolved destination.
    fn import_dropped_paths(&mut self, destination: DirId, paths: Vec<std::path::PathBuf>, placement: FolderPlacement, cx: &mut Context<Self>) {
        let ui = self.ui.clone();
        #[cfg(not(target_arch = "wasm32"))]
        ui.import_dropped_paths(destination, paths, placement == FolderPlacement::Move, cx);
        // The desktop hands over paths this build has no filesystem for.
        #[cfg(target_arch = "wasm32")]
        let _ = (ui, destination, paths, placement, cx);
    }

    /// Catalog books import immediately; existing items and files offer placement.
    fn drop_onto_folder(&mut self, destination: DirId, contents: FolderDrop, window: &mut Window, cx: &mut Context<Self>) {
        let contents = match contents {
            FolderDrop::Catalog(books) => {
                self.dismiss_menu(cx);
                self.import_catalog_books(destination, books, window, cx);
                self.cards.update(cx, |cards, cx| cards.clear_selection(cx));
                return;
            }
            contents => contents,
        };
        if let FolderDrop::Items(targets) = &contents
            && (!targets.accepts(*self.ui.backend().id(), destination) || self.folder_actions.is_none())
        {
            return;
        }
        let actions = self.folder_actions.clone();
        let page = cx.entity();
        let name = self
            .navigator
            .read(cx)
            .listing
            .children
            .iter()
            .find(|chip| chip.id == BrowseLocation::Folder(destination))
            .map(|chip| chip.name.clone())
            .unwrap_or_else(|| if destination == ROOT_DIR_ID { "library root" } else { "this folder" }.to_owned());
        self.show_menu(window.mouse_position(), window, cx, move |menu| {
            let (contents, actions, page, name) = (contents.clone(), actions.clone(), page.clone(), name.clone());
            let destination_name = SharedString::from(name.clone());
            FolderActions::drop_menu_items(&name, menu, move |placement, cx| match &contents {
                FolderDrop::Files(paths) => page.update(cx, |page, cx| page.import_dropped_paths(destination, paths.clone(), placement, cx)),
                FolderDrop::Items(targets) => {
                    if let Some(actions) = &actions {
                        actions.update(cx, |actions, cx| {
                            if targets.library_id == *actions.library_id() {
                                actions.place_targets(placement, targets.targets.clone(), destination, Some(destination_name.clone()), cx);
                            } else {
                                #[cfg(not(target_arch = "wasm32"))]
                                actions.transfer_targets(targets.library_id, placement, targets.targets.clone(), destination, Some(destination_name.clone()), cx);
                            }
                        });
                    }
                }
                FolderDrop::Catalog(_) => unreachable!("catalog drops import before opening a placement menu"),
            })
        });
        self.cards.update(cx, |cards, cx| cards.clear_selection(cx));
    }

    fn import_catalog_books(&self, destination: DirId, books: Vec<BookCardRow>, window: &mut Window, cx: &mut Context<Self>) {
        let library = self.ui.backend().clone();
        let app = self.ui.app().clone();
        cx.spawn_in(window, async move |_, cx| {
            for book in books {
                let _ = import_catalog_book_with_progress(&app, &library, book, destination, None, cx).await;
            }
        })
        .detach();
    }

    /// Opens the folder's own commands where the listing has no item — the
    /// same menu the toolbar's Add button drops down, at the pointer.
    fn open_folder_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(actions) = self.folder_actions.clone() else { return };
        let BrowseLocation::Folder(parent_id) = self.navigator.read(cx).destination.location else { return };
        self.show_menu(position, window, cx, move |menu| FolderActions::add_menu_items(actions.clone(), parent_id, menu));
    }

    pub(super) fn select_option(&mut self, event: BrowseOptionsEvent, cx: &mut Context<Self>) {
        // Authors are not a tree: the catalog by author has no graph to show.
        if matches!(event, BrowseOptionsEvent::ShowByAuthor(true)) {
            self.set_view(BrowseView::Cards, cx);
        }
        self.navigator.update(cx, |navigator, cx| navigator.select_option(event, cx));
    }

    pub(super) fn options_state(&self, cx: &App) -> (BrowseControls, BrowseOptionsSelection) {
        let navigator = self.navigator.read(cx);
        (navigator.listing.controls.clone(), navigator.selection.clone())
    }

    /// Hides the search box and hands the keyboard back to the listing.
    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_open = false;
        self.restore_focus = true;
        cx.notify();
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_open {
            self.close_search(cx);
        } else {
            let input = self.search_input.get_or_insert_with(|| {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search books or authors"));
                self._subscriptions.push(cx.subscribe(&input, |page, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        page.search_changed(input.read(cx).value().to_string(), cx);
                    }
                }));
                input
            });
            input.read(cx).focus_handle(cx).focus(window, cx);
            self.search_open = true;
            cx.notify();
        }
    }

    fn search_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_debounce = None;
        if query == self.navigator.read(cx).selection.search {
            return;
        }
        let timer = cx.background_executor().timer(Duration::from_millis(250));
        self.search_debounce = Some(cx.spawn(async move |page, cx| {
            timer.await;
            let _ = page.update(cx, |page, cx| {
                page.search_debounce = None;
                page.navigator.update(cx, |navigator, cx| navigator.search(query, cx));
            });
        }));
    }

    fn search_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input.as_ref().is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
    }

    fn render_search(&self) -> Option<Div> {
        self.search_input.as_ref().filter(|_| self.search_open).map(components::browser_search_control)
    }

    pub(super) fn close_browse_options_sheet(&mut self, cx: &mut Context<Self>) {
        self.browse_options_sheet_open = false;
        self.restore_focus = true;
        cx.notify();
    }

    pub(super) fn toggle_browse_options_sheet(&mut self, cx: &mut Context<Self>) {
        self.browse_options_sheet_open = !self.browse_options_sheet_open;
        cx.notify();
    }

    /// Leaves the current location for the one above it. Already at the root
    /// is not a failure, but it is unhandled, so the key falls through.
    fn open_parent(&mut self, cx: &mut Context<Self>) -> bool {
        // A book's page sits over the location rather than inside it, so going
        // back closes it before leaving anything.
        if self.close_book_detail(cx) {
            return true;
        }
        // The last installed listing can still belong to the previous page
        // while navigation loads. Back follows the requested trail instead.
        let mut path = self.session_path.clone();
        if path.pop().is_none() {
            return false;
        }
        self.open_path(path, cx);
        true
    }

    fn selection_keys_enabled(&self, cx: &App) -> bool {
        matches!(self.navigator.read(cx).kind, BrowseKind::Folder | BrowseKind::Books | BrowseKind::Catalog) && self.menu_keys_enabled(cx)
    }

    fn menu_keys_enabled(&self, cx: &App) -> bool {
        self.detail.is_none() && self.shown_graph().is_none() && !self.has_popup(cx)
    }

    fn clipboard_keys_enabled(&self, cx: &App) -> bool {
        self.navigator.read(cx).kind != BrowseKind::Catalog && self.menu_keys_enabled(cx)
    }

    fn on_copy_books(&mut self, _: &CopyBooks, window: &mut Window, cx: &mut Context<Self>) {
        self.clipboard_books(false, None, window, cx);
    }

    fn on_cut_books(&mut self, _: &CutBooks, window: &mut Window, cx: &mut Context<Self>) {
        self.clipboard_books(true, None, window, cx);
    }

    fn on_focused_book_clipboard(&mut self, action: &FocusedBookClipboard, window: &mut Window, cx: &mut Context<Self>) {
        self.clipboard_books(action.cut, Some((action.book, action.source)), window, cx);
    }

    fn clipboard_books(&mut self, cut: bool, focused: Option<(BookId, Option<DirId>)>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.clipboard_keys_enabled(cx) || self.search_focused(window, cx) {
            cx.propagate();
            return;
        }
        let cards = self.cards.read(cx);
        let (mut source, mut items) = cards.selected_items(cx);
        if focused.is_none()
            && items.is_empty()
            && let Some(CardEvent::ItemsMenu(cursor_items, _)) = cards.cursor_menu(cx)
        {
            items = cursor_items;
        }
        let mut books = items
            .iter()
            .filter_map(|item| match item {
                BrowseItem::Book(book) if book.read(cx).can_drag() => book.read(cx).content_hash(),
                _ => None,
            })
            .collect::<Vec<_>>();
        if let Some((book, directory)) = focused
            && (!books.contains(&book) || directory != source)
        {
            books = vec![book];
            source = directory;
        }
        if books.is_empty() {
            cx.propagate();
            return;
        }
        let clipboard = BookClipboard { library_id: *self.ui.backend().id(), books, source, cut };
        if let Some(item) = clipboard.to_item() {
            cx.write_to_clipboard(item);
        }
    }

    fn on_paste_books(&mut self, _: &PasteBooks, window: &mut Window, cx: &mut Context<Self>) {
        if !self.clipboard_keys_enabled(cx) || self.search_focused(window, cx) {
            cx.propagate();
            return;
        }
        let Some(actions) = self.folder_actions.clone() else {
            cx.propagate();
            return;
        };
        let navigator = self.navigator.read(cx);
        let destination = navigator.destination.location.folder().unwrap_or(ROOT_DIR_ID);
        let clipboard = cx.read_from_clipboard_async();
        cx.spawn(async move |page, cx| {
            let clipboard = match clipboard.await {
                Ok(Some(item)) => BookClipboard::from_item(&item),
                Ok(None) => None,
                Err(error) => {
                    let _ = page.update_in(cx, |_, window, cx| notify_error("book-paste-error", format!("Could not read the clipboard. {error}"), window, cx));
                    None
                }
            };
            let Some(clipboard) = clipboard else { return };
            let _ = page.update_in(cx, |_, window, cx| actions.update(cx, |actions, cx| actions.paste_books(clipboard, destination, window, cx)));
        })
        .detach();
    }

    fn has_popup(&self, cx: &App) -> bool {
        self.chip_menu.is_some() || self.browse_options_sheet_open || self.folder_actions.as_ref().is_some_and(|actions| actions.read(cx).has_modal()) || self.feed_actions.as_ref().is_some_and(|actions| actions.read(cx).has_modal())
    }

    fn on_toggle_grid_selection(&mut self, action: &ToggleGridSelection, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selection_keys_enabled(cx) || self.search_focused(window, cx) {
            cx.propagate();
            return;
        }
        self.cards.update(cx, |cards, cx| cards.toggle_selection(action.0, cx));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            if self.dismiss(cx) {
                cx.stop_propagation();
            }
            return;
        }
        // A popup or detail page owns its keys even if they bubble here or
        // focus has not yet moved off the listing.
        if self.detail.is_some() || self.has_popup(cx) || event.keystroke.modifiers != gpui::Modifiers::default() || self.search_focused(window, cx) {
            return;
        }
        let key = event.keystroke.key.as_str();
        let handled = match self.shown_graph().cloned() {
            Some(graph) => graph.update(cx, |graph, cx| graph.handle_key(key, cx)),
            None => self.cards.update(cx, |cards, cx| cards.handle_key(key, window, cx)),
        } || key == "backspace" && self.open_parent(cx);
        if handled {
            cx.stop_propagation();
        }
    }

    /// Opens the menu over the keyboard cursor, for readers who are not holding
    /// a pointer. The menu takes the keyboard from here and walks with the
    /// arrows on its own.
    fn on_open_context_menu(&mut self, _: &OpenContextMenu, window: &mut Window, cx: &mut Context<Self>) {
        if !self.menu_keys_enabled(cx) || self.search_focused(window, cx) {
            cx.propagate();
            return;
        }
        let opened = match self.cards.read(cx).cursor_menu(cx) {
            Some(CardEvent::ItemsMenu(items, position)) => self.open_items_menu(items, position, window, cx),
            None if self.folder_actions.is_some() && self.navigator.read(cx).destination.location.folder().is_some() => {
                self.open_folder_menu(window.mouse_position(), window, cx);
                true
            }
            _ => false,
        };
        if !opened {
            cx.propagate();
        }
    }

    /// Takes back the last library action — the menu command, drop, or rename
    /// that this library performed, whichever page performed it.
    fn on_undo(&mut self, _: &UndoLibraryAction, _window: &mut Window, cx: &mut Context<Self>) {
        let ui = self.ui.clone();
        let Some(entry) = ui.take_undo(cx) else {
            cx.propagate();
            return;
        };
        let label = entry.label.clone();
        let library = ui.backend().clone();
        cx.spawn(async move |page, cx| {
            let result = crate::library::services::UndoStack::apply(entry, &library).await;
            let _ = page.update_in(cx, |_, window, cx| {
                match result {
                    Ok(()) => crate::services::notify_info("undo", format!("Undid: {label}"), window, cx),
                    // Undo is an inverse operation, not a rollback: the library
                    // can have moved on since, and then the inverse no longer
                    // fits what is there.
                    Err(error) => notify_error("undo-error", format!("Could not undo {label}. {}", user_facing_error(&error, "Please try again.")), window, cx),
                }
            });
        })
        .detach();
    }

    /// Dismiss the active surface before changing the view beneath it.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if self.dismiss_menu(cx) {
            return true;
        }
        if self.browse_options_sheet_open {
            self.close_browse_options_sheet(cx);
            return true;
        }
        if let Some(actions) = &self.folder_actions
            && actions.read(cx).has_modal()
        {
            actions.update(cx, |actions, cx| actions.dismiss_modal(cx));
            self.restore_focus = !actions.read(cx).has_modal();
            cx.notify();
            return true;
        }
        if let Some(actions) = &self.feed_actions
            && actions.read(cx).has_modal()
        {
            actions.update(cx, |actions, cx| actions.dismiss_modal(cx));
            self.restore_focus = !actions.read(cx).has_modal();
            cx.notify();
            return true;
        }
        if self.search_open {
            self.close_search(cx);
            return true;
        }
        if self.cards.update(cx, |cards, cx| cards.clear_selection(cx)) {
            return true;
        }
        self.open_parent(cx)
    }

    fn on_dismiss(&mut self, _: &DismissSettings, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.dismiss(cx) {
            cx.propagate();
        }
    }

    fn open_child(&mut self, child_id: BrowseLocation, window: &mut Window, cx: &mut Context<Self>) {
        // A store is not browsed here: it opens its own window.
        if let BrowseLocation::Store(id) = child_id {
            if let Some(actions) = &self.feed_actions {
                actions.update(cx, |actions, cx| actions.open_store(id, window, cx));
            }
            return;
        }
        let navigator = self.navigator.read(cx);
        let Some(path) = navigator.child_path(child_id) else {
            log::warn!("ignored missing {} child {child_id}", navigator.kind.list_prefix());
            return;
        };
        // Opening a match leaves the search: what it found is now the page.
        if !navigator.selection.search.is_empty() {
            self.navigator.update(cx, |navigator, _| navigator.selection.search.clear());
            self.search_debounce = None;
            if let Some(input) = &self.search_input {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
        self.open_path(path, cx);
    }

    pub(crate) fn open_path(&mut self, path: Vec<BrowsePathSegment>, cx: &mut Context<Self>) {
        self.close_book_detail(cx);
        self.session_path = path.clone();
        let navigator = self.navigator.read(cx);
        if BrowseDestination::from_path(navigator.root(), path.clone()).location != navigator.destination.location
            && let Some(actions) = &self.folder_actions
        {
            actions.update(cx, |actions, cx| actions.cancel_move_selection(cx));
        }
        self.navigator.update(cx, |navigator, cx| navigator.open_path(path, cx));
    }

    fn set_view(&mut self, view: BrowseView, cx: &mut Context<Self>) {
        if self.graph.is_some() {
            self.view = view;
            cx.notify();
        }
    }

    fn toggle_view(&mut self, cx: &mut Context<Self>) {
        self.set_view(if self.view == BrowseView::Cards { BrowseView::Graph } else { BrowseView::Cards }, cx);
    }

    /// The root, then the path; the trail folds itself to the topbar's width.
    fn breadcrumb(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let navigator = self.navigator.read(cx);
        let (root_label, path) = (navigator.kind.root_label(), navigator.destination.path.clone());
        let mut root = components::folder_breadcrumb_root_item(root_label.clone(), path.is_empty(), theme);
        if !path.is_empty() {
            root = root.on_click(cx.listener(|page, _, _, cx| page.open_path(Vec::new(), cx)));
        }
        let mut crumbs = vec![self.breadcrumb_drop_target(root, ROOT_DIR_ID, root_label.clone(), theme, cx)];
        for (index, crumb) in path.iter().enumerate() {
            let current = index + 1 == path.len();
            let mut item = components::folder_breadcrumb_item(index + 1, crumb.name.clone(), current, theme);
            if !current {
                let trail = path[..=index].to_vec();
                item = item.on_click(cx.listener(move |page, _, _, cx| page.open_path(trail.clone(), cx)));
            }
            if let BrowseLocation::Folder(destination) = crumb.id {
                item = self.breadcrumb_drop_target(item, destination, crumb.name.clone().into(), theme, cx);
            }
            crumbs.push(item);
        }
        let labels = std::iter::once(root_label).chain(path.iter().map(|crumb| SharedString::from(crumb.name.clone()))).collect();
        components::folder_breadcrumb(crumbs, labels, theme).into_any_element()
    }

    /// An ancestor folder is exactly as valid a destination for a drag as one
    /// shown in the grid below it, so each breadcrumb segment takes drops, and
    /// shows it will, the way a folder chip does.
    fn breadcrumb_drop_target(&self, item: Stateful<Div>, destination: DirId, name: SharedString, theme: components::BrowserTheme, cx: &App) -> Stateful<Div> {
        let events = self.card_events.clone();
        let library_id = *self.ui.backend().id();
        if self.navigator.read(cx).kind == BrowseKind::Books { import_drop_target(item, library_id, destination, "library root".into(), events) } else { folder_drop_target(item, library_id, destination, name, true, events, theme) }
    }

    fn navigation_heading(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        if components::uses_mobile_navigation(window) {
            let navigator = self.navigator.read(cx);
            let theme = components::browser_theme(cx);
            let route = navigator.kind.route();
            if self.view == BrowseView::Graph {
                if components::touch_ui() && crate::navigation::LIBRARY_VIEWS.contains(&route) {
                    return crate::navigation::library_root_heading(route, theme);
                }
                return components::mobile_navigation_heading("", crate::navigation::mobile_back(true), theme);
            }
            // At a page's root the heading names the page; anywhere deeper, the
            // title is the place itself.
            let Some(crumb) = navigator.destination.path.last() else {
                return crate::navigation::library_root_heading(route, theme);
            };
            crate::navigation::library_heading(route, crumb.name.clone(), theme)
        } else {
            components::browser_topbar_left().when(self.view == BrowseView::Cards, |heading| heading.child(self.breadcrumb(cx)))
        }
    }

    fn trash_button(&self, cx: &App) -> Option<AnyElement> {
        (self.navigator.read(cx).kind == BrowseKind::Folder).then(|| {
            components::outlined_icon_button("open-browse-trash", "Trash", components::navigation_icon(components::NavigationIcon::Trash2))
                .debug_selector(|| "open-browse-trash".into())
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::OpenTrash), cx))
                .into_any_element()
        })
    }

    fn render_topbar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let graph_shown = self.shown_graph().cloned();
        let compact_graph = graph_shown.is_some() && components::WindowWidthClass::for_window(window).is_compact();
        let search_field = self.render_search();
        let search_open = search_field.is_some();
        let inline_search_field = if graph_shown.is_none() { search_field } else { None };
        let search_button_label = if search_open { "Hide search" } else { "Search books or authors" };
        let navigator = self.navigator.read(cx);
        let (browse_controls, selection, folder, all_books) = (navigator.listing.controls.clone(), navigator.selection.clone(), navigator.destination.location.folder(), navigator.kind == BrowseKind::Books);
        let catalog_root = navigator.destination.path.is_empty();
        let add_feed = self.feed_actions.clone().filter(|_| catalog_root && !compact_graph).map(|actions| FeedActions::render_trigger(actions));
        let open_browser = self.feed_actions.clone().and_then(|actions| FeedActions::render_store_trigger(actions, cx));
        let searching = !selection.search.is_empty();
        let compact = components::WindowWidthClass::for_window(window).is_compact();
        let view_toggle = (self.graph.is_some() && !selection.by_author).then(|| {
            let (label, icon) = if graph_shown.is_some() { ("Show as cards", IconName::LayoutDashboard) } else { ("Show as graph", IconName::Network) };
            components::outlined_icon_button("toggle-browse-view", label, icon).on_click(cx.listener(|page, _, _, cx| page.toggle_view(cx)))
        });
        let graph_layout_switch = graph_shown.as_ref().map(|graph| graph.update(cx, |graph, cx| graph.layout_control(cx)));
        let menu_anchor = gpui::Anchor::TopRight;
        let controls = components::browser_topbar_right()
            .when(inline_search_field.is_some(), |controls| controls.flex_1())
            .children(inline_search_field.map(|field| div().flex_1().min_w(rems(8.0)).child(field)))
            .when(!compact_graph, |controls| {
                controls.child(
                    components::selectable_button(components::outlined_icon_button("toggle-browse-search", search_button_label, IconName::Search), search_open || searching, theme)
                        .on_click(cx.listener(|page, _, window, cx| page.toggle_search(window, cx))),
                )
            })
            .children(view_toggle)
            .children(graph_layout_switch)
            .when(!compact_graph, |controls| controls.children(self.folder_actions.as_ref().zip(folder).map(|(actions, parent)| FolderActions::render_trigger(actions.clone(), parent, menu_anchor))))
            .when(all_books, |controls| controls.child(components::outlined_icon_button("all-books-add", "Add book", IconName::Plus).on_click(cx.listener(|page, _, _, cx| page.ui.choose_and_import_book(ROOT_DIR_ID, cx)))))
            .children(add_feed)
            .children(open_browser)
            .children(self.trash_button(cx))
            .children(browse_language_control(cx.entity(), &browse_controls, &selection, compact, menu_anchor))
            .child(browse_options_control(cx.entity(), &selection, compact, menu_anchor, theme));
        if graph_shown.is_some() {
            div().h(gpui::rems(components::topbar_height_rem())).flex_none().flex().items_center().p(px(4.0)).rounded(px(8.0)).bg(theme.page_bg).border_1().border_color(theme.rule).child(controls).into_any_element()
        } else {
            components::topbar(theme).child(self.navigation_heading(window, cx)).child(controls).into_any_element()
        }
    }

    fn render_browse_options_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.browse_options_sheet_open {
            return None;
        }
        let theme = components::browser_theme(cx);
        let navigator = self.navigator.read(cx);
        let (browse_controls, selection) = (navigator.listing.controls.clone(), navigator.selection.clone());
        Some(browse_options_sheet(cx.entity(), browse_controls, selection, theme))
    }

    pub(crate) fn request_split_child(&mut self, child_id: BrowseLocation, side: BrowseSplitSide, cx: &mut Context<Self>) {
        if matches!(child_id, BrowseLocation::Store(_)) {
            return;
        }
        if let Some(path) = self.navigator.read(cx).child_path(child_id) {
            let session = NavigationSession {
                library_id: Some(*self.ui.backend().id()),
                route: self.navigator.read(cx).kind.route().into(),
                pages: vec![BrowsePageSession { path: path.into_iter().map(|crumb| (crumb.id, crumb.name)).collect(), detail: None }],
                ..Default::default()
            };
            cx.emit(PaneRequest::Open { session, side });
        }
    }

    pub(crate) fn saved_session(&self, cx: &App) -> BrowsePageSession {
        BrowsePageSession { path: self.session_path.iter().map(|crumb| (crumb.id, crumb.name.clone())).collect(), detail: self.shown_book_detail(cx).map(|locator| locator.content_hash()).or(self.restoring_detail) }
    }

    pub(crate) fn restore_session(&mut self, saved: BrowsePageSession, cx: &mut Context<Self>) {
        self.open_path(saved.path.into_iter().map(|(location, label)| BrowsePathSegment { id: location, name: label }).collect(), cx);
        if let Some(hash) = saved.detail {
            self.restoring_detail = Some(hash);
            let library = self.ui.backend().clone();
            self.restore_task = Some(cx.spawn(async move |page, cx| {
                let result = library.book_detail(hash).await;
                let _ = page.update(cx, |page, cx| {
                    if page.restoring_detail != Some(hash) {
                        return;
                    }
                    page.restoring_detail = None;
                    if let Ok(detail) = result {
                        page.show_book_detail(detail.book, None, cx);
                    }
                    cx.notify();
                });
            }));
        }
    }

    pub(crate) fn shown_book_detail(&self, cx: &App) -> Option<library_model::BookLocator> {
        self.detail.as_ref().and_then(|(detail, _)| detail.read(cx).locator())
    }

    /// Shows a book's page over the grid, unless it is already showing.
    pub(crate) fn show_book_detail(&mut self, book: library_model::BookCardRow, cover: Option<LoadedCover>, cx: &mut Context<Self>) {
        if self.detail.as_ref().is_some_and(|(detail, _)| detail.read(cx).key() == book.key()) {
            return;
        }
        self.restoring_detail = None;
        self.restore_task = None;
        let detail = BookDetailPage::create(book, cover, self.ui.clone(), cx);
        let subscription = cx.subscribe(&detail, |page, _, _: &CloseBookDetail, cx| {
            page.close_book_detail(cx);
        });
        self.detail = Some((detail, subscription));
        cx.notify();
    }

    /// Opens a book's page over the location at `path`, for a book opened from
    /// outside the grid. A grid already showing that location is left as it is.
    pub(crate) fn open_book_detail(&mut self, book: library_model::BookCardRow, path: Vec<BrowsePathSegment>, cx: &mut Context<Self>) {
        if path != self.navigator.read(cx).destination.path {
            self.open_path(path, cx);
        }
        self.show_book_detail(book, None, cx);
    }

    /// Closes the book's page, back to the grid underneath. Returns whether one
    /// was open.
    pub(crate) fn close_book_detail(&mut self, cx: &mut Context<Self>) -> bool {
        self.restore_task = None;
        let restoring = self.restoring_detail.take().is_some();
        if self.detail.take().is_none() && !restoring {
            return false;
        }
        self.restore_focus = true;
        cx.notify();
        true
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let view = match self.shown_graph().cloned() {
            Some(graph) => {
                let controls = self.render_topbar(window, cx);
                let search = self.render_search();
                let count = self.ui.updates().read(cx).book_count().unwrap_or(0);
                graph.update(cx, |graph, cx| graph.render_panel(count, controls, search, window, cx))
            }
            None => self.cards.clone().into_any_element(),
        };
        let empty = self.cards.read(cx).is_empty(cx);
        let updates = self.ui.updates().read(cx);
        let scanning_empty_library = empty && updates.scanning();
        let waiting_for_library = empty && updates.loading();
        let navigator = self.navigator.read(cx);
        // Only the states that are genuinely a wait: a library still opening,
        // and a scan that has not yet found anything. Neither is a browse
        // request, and there is nothing painted underneath either to keep.
        let show_loading = (scanning_empty_library || waiting_for_library) && navigator.error.is_none();
        let loading_label = if scanning_empty_library { "Scanning library…" } else { "Loading…" };
        let error = navigator.error.clone();
        // A feed that refused this device; its name is the first crumb's.
        let sign_in = match (navigator.listing.sign_in, navigator.destination.location, &self.feed_actions) {
            (true, BrowseLocation::Opds { source, .. }, Some(actions)) => {
                let name = navigator.destination.path.first().map(|crumb| crumb.name.clone()).unwrap_or_default();
                Some((actions.clone(), source, name))
            }
            _ => None,
        };
        let mut body = div().relative().size_full().min_h_0().min_w_0();
        // Folder space imports into that folder; library query pages use root.
        if let BrowseLocation::Folder(destination) = navigator.destination.location {
            // Not outlined: it holds every other target, and the one under
            // the pointer is the one to show.
            let name = navigator.destination.path.last().map_or_else(|| navigator.kind.root_label(), |crumb| crumb.name.clone().into());
            body = folder_drop_target(body, *self.ui.backend().id(), destination, name, false, self.card_events.clone(), theme);
        } else if matches!(navigator.kind, BrowseKind::Home | BrowseKind::Books | BrowseKind::Subject | BrowseKind::Author) {
            body = import_drop_target(body, *self.ui.backend().id(), ROOT_DIR_ID, "library root".into(), self.card_events.clone());
        }
        let sign_in = sign_in.map(|(actions, source, name)| FeedActions::render_sign_in(actions, source, name, cx));
        let body = body
            .child(view)
            .children(sign_in)
            .when(show_loading, |body| {
                body.child(components::absolute_full_size().debug_selector(|| "browse-loading-overlay".into()).bg(theme.page_bg).flex().items_center().justify_center().child(components::browser_loading_message(loading_label, theme)))
            })
            .into_any_element();
        with_retry_banner(body, error, "retry-section", theme, cx.listener(|page, _, _, cx| page.reload(cx)))
    }

    fn selection_download_action(&self, items: &[BrowseItem], cx: &Context<Self>) -> Option<SelectionAction> {
        let books = items.iter().filter_map(BrowseItem::book).cloned().collect::<Vec<_>>();
        let book_download = BookCardView::download_selection_action(&books, cx);
        let folders = items.iter().filter_map(BrowseItem::child).filter(|row| can_download(row)).map(|row| row.id).collect::<Vec<_>>();
        if folders.is_empty() {
            return book_download;
        }
        let page = cx.entity();
        Some(SelectionAction::new("download", "Download to This Device", components::download_glyph(), move |window, cx| {
            page.update(cx, |page, cx| {
                for &location in &folders {
                    page.download_child(location, cx);
                }
            });
            if let Some(action) = &book_download {
                action.run(window, cx);
            }
        }))
    }

    fn selection_actions(&self, source: Option<DirId>, items: &[BrowseItem], cx: &Context<Self>) -> Vec<SelectionAction> {
        if source.is_none() && items.iter().all(|item| matches!(item, BrowseItem::Book(_))) {
            let books = items.iter().filter_map(BrowseItem::book).cloned().collect::<Vec<_>>();
            return BookCardView::selection_actions(&books, cx);
        }
        let mut commands = Vec::new();
        if crate::native_windows_available() && (source.is_some() || items.len() == 1) {
            let library_id = *self.ui.backend().id();
            let windows = items
                .iter()
                .filter_map(|item| match item {
                    BrowseItem::Child { row, .. } => match row.id {
                        BrowseLocation::Folder(directory_id) if source.is_some() => Some(NewWindowTarget::Folder { library_id, directory_id, title: row.name.clone() }),
                        BrowseLocation::Store(_) => None,
                        location if items.len() == 1 => Some(NewWindowTarget::Subject { library_id, location, label: row.name.clone() }),
                        _ => None,
                    },
                    BrowseItem::Book(book) => Some(NewWindowTarget::Book { locator: BookLocator::new(library_id, book.read(cx).content_hash()?), title: book.read(cx).title().into() }),
                })
                .collect::<Vec<_>>();
            if !windows.is_empty() {
                commands.push(SelectionAction::new("new-window", "Open in New Window", IconName::ExternalLink, move |window, cx| {
                    for target in &windows {
                        window.dispatch_action(Box::new(crate::OpenInNewWindow(target.clone())), cx);
                    }
                }));
            }
        }
        commands.extend(self.selection_download_action(items, cx));
        if let Some((actions, targets)) = self.folder_actions.clone().zip(source.and_then(|source| FolderTargets::from_items(items, source, cx))) {
            commands.extend(FolderActions::selection_actions(actions, targets, items, cx));
        }
        commands
    }

    fn render_selection_footer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.detail.is_some() || self.shown_graph().is_some() {
            return None;
        }
        let (source, items) = self.cards.read(cx).selected_items(cx);
        if items.len() < 2 {
            return None;
        }
        let theme = components::browser_theme(cx);
        let actions = self.selection_actions(source, &items, cx);
        Some(
            components::selection_footer(theme)
                .debug_selector(|| "browse-selection-footer".into())
                .child(div().flex_none().text_size(rems(components::TEXT_SM)).child(format!("{} selected", items.len())))
                .child(div().flex_1().min_w_0().flex().flex_wrap().items_center().gap(px(components::SPACE_XS)).children(actions.iter().map(|action| {
                    let id = action.id;
                    div().debug_selector(move || format!("selection-action-{id}")).child(action.button(theme))
                })))
                .child(div().flex_none().debug_selector(|| "clear-browse-selection".into()).child(components::outlined_icon_button("clear-selection", "Clear selection", IconName::Close).on_click(cx.listener(|page, _, _, cx| {
                    page.cards.update(cx, |cards, cx| cards.clear_selection(cx));
                }))))
                .into_any_element(),
        )
    }

    fn render_shell(&self, topbar: AnyElement, body: AnyElement, options_sheet: Option<AnyElement>, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let folder_overlay = self.folder_actions.as_ref().and_then(|actions| FolderActions::render_overlay(actions.clone(), window, cx));
        let feed_overlay = self.feed_actions.as_ref().and_then(|actions| FeedActions::render_overlay(actions.clone(), window, cx));
        let chip_menu = self.chip_menu.as_ref().map(|active| active.render(window));
        let selection_footer = self.render_selection_footer(cx);
        let mut key_context = gpui::KeyContext::default();
        if self.selection_keys_enabled(cx) {
            key_context.add("BrowseSelection");
        }
        if self.clipboard_keys_enabled(cx) {
            key_context.add("BrowseClipboard");
        }
        components::full_size_column()
            .track_focus(&self.focus)
            .key_context(key_context)
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
            .on_action(cx.listener(Self::on_toggle_grid_selection))
            .on_action(cx.listener(Self::on_copy_books))
            .on_action(cx.listener(Self::on_cut_books))
            .on_action(cx.listener(Self::on_focused_book_clipboard))
            .on_action(cx.listener(Self::on_paste_books))
            .on_action(cx.listener(Self::on_undo))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(topbar)
            .child(components::remaining_space_column().child(body))
            .children(selection_footer)
            .children(chip_menu)
            .children(folder_overlay)
            .children(feed_overlay)
            .children(options_sheet)
    }
}

impl Render for BrowsePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.detail.is_none() {
            self.navigator.update(cx, |navigator, cx| navigator.refresh_if_needed(cx));
        }
        if let Some(stage) = self.profile_render_pending.take() {
            window.on_next_frame(move |_, _| {
                stage.finish("rendered");
                StartupProfile::new("browse").mark("listing_frame_callback");
            });
        }
        if std::mem::take(&mut self.restore_focus) {
            self.focus.focus(window, cx);
        }
        if let Some(detail) = self.detail.as_ref().map(|(detail, _)| detail.clone()) {
            // The book's page carries its own Back, named for where it goes,
            // over the cover; it needs no bar above it.
            let topbar = div().h(px(0.0)).flex_none().into_any_element();
            let body = div().size_full().child(detail).into_any_element();
            self.render_shell(topbar, body, None, window, cx)
        } else {
            let topbar = if self.shown_graph().is_some() { div().h(px(0.0)).flex_none().into_any_element() } else { self.render_topbar(window, cx) };
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

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use crate::navigation::LibrarySession;
    use gpui::{ClipboardItem, TestAppContext, VisualTestContext};
    use gpui_component::Root;
    use std::{cell::RefCell, fs};

    #[gpui::test]
    async fn search_debounces_across_hiding_and_cancels_when_opening_a_result(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        library.backend().create_directory(ROOT_DIR_ID, "Search destination".into()).await.unwrap();
        let folder = library.backend().folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Search destination").unwrap().id;
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Folder, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.child(BrowseLocation::Folder(folder)).is_some()).await;
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| page.update(cx, |page, cx| page.toggle_search(window, cx)));
        visual.run_until_parked();
        visual.simulate_input("Search");
        visual.run_until_parked();
        visual.executor().advance_clock(Duration::from_millis(200));
        visual.run_until_parked();
        visual.simulate_input(" destination");
        visual.run_until_parked();
        page.update(&mut visual, |page, cx| page.close_search(cx));
        visual.executor().advance_clock(Duration::from_millis(249));
        visual.run_until_parked();
        visual.read_entity(&navigator, |navigator, _| assert!(navigator.selection.search.is_empty()));
        visual.executor().advance_clock(Duration::from_millis(1));
        visual.run_until_parked();
        visual.condition(&navigator, |navigator, _| navigator.selection.search == "Search destination" && navigator.listing.child(BrowseLocation::Folder(folder)).is_some()).await;
        let input = visual.read_entity(&page, |page, _| page.search_input.clone().unwrap());
        visual.update(|window, cx| page.update(cx, |page, cx| page.toggle_search(window, cx)));
        visual.read_entity(&page, |page, cx| {
            assert_eq!(page.search_input.as_ref(), Some(&input));
            assert_eq!(input.read(cx).value().as_str(), "Search destination");
        });
        visual.simulate_input(" stale");
        visual.run_until_parked();
        visual.read_entity(&page, |page, _| assert!(page.search_debounce.is_some()));
        visual.update(|window, cx| page.update(cx, |page, cx| page.open_child(BrowseLocation::Folder(folder), window, cx)));
        visual.read_entity(&page, |page, _| assert!(page.search_debounce.is_none()));
        visual.condition(&navigator, |navigator, _| navigator.destination.location == BrowseLocation::Folder(folder)).await;
        visual.executor().advance_clock(Duration::from_millis(300));
        visual.run_until_parked();
        visual.read_entity(&page, |page, cx| {
            assert_eq!(page.navigator.read(cx).destination.location, BrowseLocation::Folder(folder));
            assert!(page.navigator.read(cx).selection.search.is_empty());
            assert!(input.read(cx).value().is_empty());
            assert!(page.search_debounce.is_none());
        });
    }

    #[gpui::test]
    async fn split_request_keeps_the_library_route_and_child_trail(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        library.backend().create_directory(ROOT_DIR_ID, "Split destination".into()).await.unwrap();
        let folder = library.backend().folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Split destination").unwrap().id;
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Folder, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        cx.condition(&navigator, |navigator, _| navigator.listing.child(BrowseLocation::Folder(folder)).is_some()).await;
        let received = Rc::new(RefCell::new(Vec::new()));
        let target = received.clone();
        let _subscription = cx.update(|cx| cx.subscribe(&page, move |_, request: &PaneRequest, _| target.borrow_mut().push(request.clone())));
        page.update(cx, |page, cx| page.request_split_child(BrowseLocation::Folder(folder), BrowseSplitSide::Right, cx));
        let received = received.borrow();
        let [PaneRequest::Open { session, side }] = received.as_slice() else { panic!("expected one request to open a pane") };
        assert_eq!(*side, BrowseSplitSide::Right);
        assert_eq!(session.library_id, Some(*library.backend().id()));
        assert_eq!(session.route, crate::LibraryRoute::Folders.into());
        assert_eq!(session.pages[0].path, [(BrowseLocation::Folder(folder), "Split destination".into())]);
        assert!(session.pages[0].detail.is_none());
    }

    async fn import_clipboard_books(library: &LibraryContext) -> tempfile::TempDir {
        let external = tempfile::tempdir().unwrap();
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/library-backend/crates/book-inspection/tests/fixtures");
        let paths = ["back-matter-evidence.epub", "ocr-image-priority.epub"].map(|name| {
            let path = external.path().join(name);
            fs::copy(fixtures.join(name), &path).unwrap();
            path
        });
        assert!(library.backend().import_paths(ROOT_DIR_ID, paths.into(), false).await.unwrap().is_empty());
        external
    }

    #[gpui::test]
    async fn scan_overlay_disappears_on_first_folders_or_books_while_scan_continues(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        // Hold activity open independently of the local fixture's own scan.
        updates.update(cx, |updates, cx| updates.set_importing(true, cx));

        for kind in [BrowseKind::Folder, BrowseKind::Books] {
            let page = cx.new(|cx| BrowsePage::new(kind, library.clone(), cx));
            let navigator = page.read_with(cx, |page, _| page.navigator.clone());
            let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
            cx.condition(&navigator, |navigator, _| navigator.listing.revision > 0).await;
            let mut visual = VisualTestContext::from_window(window.into(), cx);
            visual.run_until_parked();
            assert!(visual.debug_bounds("browse-loading-overlay").is_some());

            let _books = if kind == BrowseKind::Folder {
                // Drive includes remote folders before any books are inspected.
                // Install an empty folder before any book inspection completes.
                navigator.update(cx, |navigator, cx| {
                    let row = library_model::BrowseRow {
                        id: BrowseLocation::Folder(uuid::Uuid::new_v4()), name: "First discovered folder".into(),
                        path: None, book_count: 0, downloaded_book_count: 0,
                    };
                    let mut data = library_backend::LibraryBrowseData::default();
                    data.contents.children.push(row.clone());
                    data.sections.push(library_model::BrowseSection { parent: row, children: Vec::new(), books: Vec::new(), book_count: None });
                    navigator.listing.install(data, false, cx);
                    cx.emit(super::super::navigator::Loaded { left: None, refresh: true });
                    cx.notify();
                });
                None
            } else {
                let books = import_clipboard_books(&library).await;
                navigator.update(cx, |navigator, cx| navigator.reload(cx));
                cx.condition(&navigator, |navigator, _| !navigator.listing.books.is_empty()).await;
                Some(books)
            };
            visual.run_until_parked();
            assert!(updates.read_with(cx, |updates, _| updates.scanning()), "the remaining scan continues in the background");
            assert!(visual.debug_bounds("browse-loading-overlay").is_none(), "the first folders or books must be usable immediately");
        }
        updates.update(cx, |updates, cx| updates.set_importing(false, cx));
    }

    #[gpui::test]
    async fn book_clipboard_uses_the_book_focused_with_tab_in_a_section(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let _external = import_clipboard_books(&library).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning() && updates.book_count() == Some(2)).await;
        cx.update(crate::bind_keys);
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Home, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.sections.iter().any(|section| section.books.len() == 2)).await;
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let expected = navigator.read_with(cx, |navigator, cx| navigator.listing.sections.iter().find(|section| section.books.len() == 2).unwrap().books[1].read(cx).content_hash().unwrap());
        visual.update(|window, cx| {
            page.update(cx, |page, cx| {
                page.focus.focus(window, cx);
                page.cards.update(cx, |cards, cx| {
                    cards.handle_key("down", window, cx);
                });
            });
        });
        let mut found = false;
        for _ in 0..40 {
            visual.write_to_clipboard(ClipboardItem::new_string("unrelated text".into()));
            visual.simulate_keystrokes("tab secondary-c");
            if visual.read_from_clipboard().as_ref().and_then(BookClipboard::from_item).is_some_and(|clipboard| clipboard.books == [expected]) {
                found = true;
                break;
            }
        }
        assert!(found, "Tab-focused books must handle Copy instead of the outer folder cursor");
        visual.simulate_keystrokes("secondary-x");
        let clipboard = BookClipboard::from_item(&visual.read_from_clipboard().unwrap()).unwrap();
        assert!(clipboard.cut);
        assert_eq!(clipboard.books, [expected]);
        visual.read_entity(&page, |page, cx| assert!(page.cards.read(cx).selected_items(cx).1.is_empty()));
    }

    #[gpui::test]
    async fn book_clipboard_keeps_only_unfinished_moves_for_retry(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let _external = import_clipboard_books(&library).await;
        library.backend().create_directory(ROOT_DIR_ID, "Retry destination".into()).await.unwrap();
        let destination = library.backend().folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Retry destination").unwrap().id;
        cx.update(crate::bind_keys);
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Books, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.books.len() == 2).await;
        let books = navigator.read_with(cx, |navigator, cx| navigator.listing.books.iter().map(|book| book.read(cx).content_hash().unwrap()).collect::<Vec<_>>());
        // Make the second source unavailable until after the first paste.
        library.backend().remove_book_from_directory(books[1], ROOT_DIR_ID).await.unwrap();
        let clipboard = BookClipboard { library_id: *library.backend().id(), books: books.clone(), source: Some(ROOT_DIR_ID), cut: true };
        cx.write_to_clipboard(clipboard.to_item().unwrap());
        let actions = page.read_with(cx, |page, _| page.folder_actions.clone().unwrap());
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| actions.update(cx, |actions, cx| actions.paste_books(clipboard, destination, window, cx)));
        visual.condition(&actions, |_, cx| cx.read_from_clipboard().as_ref().and_then(BookClipboard::from_item).is_some_and(|clipboard| clipboard.books == [books[1]])).await;
        let remaining = BookClipboard::from_item(&visual.read_from_clipboard().unwrap()).unwrap();
        assert_eq!(remaining.books, [books[1]]);
        assert_eq!(remaining.source, Some(ROOT_DIR_ID));
        assert!(remaining.cut);

        library.backend().restore_book_placement(books[1], ROOT_DIR_ID).await.unwrap();
        visual.update(|window, cx| actions.update(cx, |actions, cx| actions.paste_books(remaining, destination, window, cx)));
        visual.condition(&actions, |_, cx| cx.read_from_clipboard().and_then(|item| item.text()).is_none()).await;
        assert!(visual.read_from_clipboard().and_then(|item| item.text()).is_none());
        let query = visual.read_entity(&navigator, |navigator, _| navigator.selection.backend_query(BrowseLocation::Folder(ROOT_DIR_ID), false));
        let root = library.backend().browse(query).await.unwrap();
        assert!(root.contents.books.is_empty(), "both books should have moved out of the source");
    }

    #[gpui::test]
    async fn book_copy_paste_shortcuts_work_across_windows_for_single_and_multiple_books(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        let _external = import_clipboard_books(&library).await;
        library.backend().create_directory(ROOT_DIR_ID, "Pasted books".into()).await.unwrap();
        let destination = library.backend().folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Pasted books").unwrap();
        cx.update(crate::bind_keys);
        let source_page = cx.new(|cx| BrowsePage::new(BrowseKind::Books, library.clone(), cx));
        let source_navigator = source_page.read_with(cx, |page, _| page.navigator.clone());
        let source_window = cx.add_window(|window, cx| Root::new(source_page.clone(), window, cx));
        cx.condition(&source_navigator, |navigator, _| navigator.listing.books.len() == 2).await;
        let destination_page = cx.new(|cx| BrowsePage::new(BrowseKind::Folder, library.clone(), cx));
        let destination_navigator = destination_page.read_with(cx, |page, _| page.navigator.clone());
        let destination_window = cx.add_window(|window, cx| Root::new(destination_page.clone(), window, cx));
        cx.condition(&destination_navigator, |navigator, _| navigator.listing.revision > 0).await;
        destination_page.update(cx, |page, cx| page.open_path(vec![BrowsePathSegment { id: BrowseLocation::Folder(destination.id), name: "Pasted books".into() }], cx));
        let mut source = VisualTestContext::from_window(source_window.into(), cx);
        source.update(|window, cx| source_page.read(cx).focus.clone().focus(window, cx));
        source.run_until_parked();
        source.simulate_keystrokes("down secondary-c");
        let clipboard = BookClipboard::from_item(&source.read_from_clipboard().unwrap()).unwrap();
        assert_eq!(clipboard.books.len(), 1);
        let mut target = VisualTestContext::from_window(destination_window.into(), cx);
        target.update(|window, cx| destination_page.read(cx).focus.clone().focus(window, cx));
        target.run_until_parked();
        target.simulate_keystrokes("secondary-v");
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.len() == 1).await;

        source.simulate_keystrokes("secondary-right secondary-c");
        let clipboard = BookClipboard::from_item(&source.read_from_clipboard().unwrap()).unwrap();
        assert_eq!(clipboard.books.len(), 2);
        target.simulate_keystrokes("secondary-v");
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.len() == 2).await;
        source.read_entity(&source_page, |page, cx| assert_eq!(page.navigator.read(cx).listing.books.len(), 2));

        target.update(|window, cx| destination_page.update(cx, |page, cx| page.on_undo(&UndoLibraryAction, window, cx)));
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.len() == 1).await;
        target.update(|window, cx| destination_page.update(cx, |page, cx| page.on_undo(&UndoLibraryAction, window, cx)));
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.is_empty()).await;

        source.update(|_, cx| source_page.update(cx, |page, cx| page.cards.update(cx, |cards, cx| cards.clear_selection(cx))));
        source.simulate_keystrokes("secondary-x");
        let clipboard = BookClipboard::from_item(&source.read_from_clipboard().unwrap()).unwrap();
        assert!(clipboard.cut);
        assert_eq!(clipboard.books.len(), 1);
        target.simulate_keystrokes("secondary-v");
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.len() == 1).await;
        target.run_until_parked();
        assert!(target.read_from_clipboard().and_then(|item| item.text()).is_none());

        source.simulate_keystrokes("secondary-left secondary-x");
        let clipboard = BookClipboard::from_item(&source.read_from_clipboard().unwrap()).unwrap();
        assert!(clipboard.cut);
        assert_eq!(clipboard.books.len(), 2);
        target.simulate_keystrokes("secondary-v");
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.len() == 2).await;
        target.update(|window, cx| destination_page.update(cx, |page, cx| page.on_undo(&UndoLibraryAction, window, cx)));
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.len() == 1).await;
        target.update(|window, cx| destination_page.update(cx, |page, cx| page.on_undo(&UndoLibraryAction, window, cx)));
        target.condition(&destination_navigator, |navigator, _| navigator.listing.books.is_empty()).await;

        target.write_to_clipboard(ClipboardItem::new_string("ordinary text".into()));
        target.simulate_keystrokes("secondary-v");
        target.run_until_parked();
        target.update(|_, cx| session.update(cx, |session, _| assert!(session.take_undo().is_none())));
    }

    #[gpui::test]
    async fn catalog_multi_selection_shows_footer_and_clear_removes_it(cx: &mut TestAppContext) {
        check_book_selection_footer(cx, true).await;
    }

    #[gpui::test]
    async fn library_multi_selection_shows_download_in_footer(cx: &mut TestAppContext) {
        check_book_selection_footer(cx, false).await;
    }

    async fn check_book_selection_footer(cx: &mut TestAppContext, catalog: bool) {
        use crate::library::widgets::CursorMove;
        use library_model::{BookCardRow, BookSource, LibraryFileTypeFilter};

        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        cx.update(|cx| {
            cx.set_global(crate::stores::GeneratedCoversOnly(true));
            cx.set_global(crate::stores::CoverTextSetting(app_preferences::CoverText::CoverOnly));
        });
        // Install book rows locally without downloading catalog data or book files.
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Books, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.revision > 0).await;
        navigator.update(cx, |navigator, cx| {
            let books = (0..3)
                .map(|index| {
                    BookCardView::create_in_grid(
                        BookCardRow {
                            source: if catalog { BookSource::Catalog { content_hash: BookId::from_bytes([index + 10; 32]) } } else { BookSource::Library { content_hash: BookId::from_bytes([index + 10; 32]) } },
                            title: format!("Catalog book {index}"),
                            subtitle: None,
                            author: "Writer".into(),
                            progress: 0.0,
                            downloaded: false,
                            download_requested: false,
                            format_category: LibraryFileTypeFilter::Book,
                            audiobook_duration_ms: None,
                            added_at: 0,
                            source_directory: None,
                        },
                        library.clone(),
                        Rc::new(|_, _, _| {}),
                        false,
                        false,
                        cx,
                    )
                })
                .collect::<Vec<_>>();
            navigator.kind = if catalog { BrowseKind::Catalog } else { BrowseKind::Books };
            navigator.listing.items = Rc::new(books.iter().map(|book| (super::super::listing::book_id(book.read(cx).key(), None), BrowseItem::Book(book.clone()))).collect());
            navigator.listing.books = Rc::new(books);
            navigator.listing.page = None;
            cx.emit(Loaded { left: None, refresh: false });
        });
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-selection-footer").is_none());
        visual.update(|_, cx| page.update(cx, |page, cx| page.cards.update(cx, |cards, cx| cards.toggle_selection(CursorMove::Right, cx))));
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-selection-footer").is_none(), "one selected book keeps its individual actions");
        visual.update(|_, cx| page.update(cx, |page, cx| page.cards.update(cx, |cards, cx| cards.toggle_selection(CursorMove::Right, cx))));
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-selection-footer").is_some());
        assert!(visual.debug_bounds("selection-action-download").is_some());
        visual.update(|_, cx| {
            page.update(cx, |page, cx| {
                let (_, items) = page.cards.read(cx).selected_items(cx);
                assert_eq!(items.len(), 2);
                if !catalog {
                    let download = page.selection_download_action(&items, cx).unwrap();
                    assert!(!download.is_disabled());
                    assert!(page.selection_actions(Some(ROOT_DIR_ID), &items, cx).iter().any(|action| action.id == "download" && !action.is_disabled()), "folder book selections must also offer Download");
                }
            })
        });
        let clear = visual.debug_bounds("clear-browse-selection").unwrap().center();
        visual.simulate_click(clear, Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-selection-footer").is_none());
        visual.read_entity(&page, |page, cx| assert!(page.cards.read(cx).selected_items(cx).1.is_empty()));
    }

    #[gpui::test]
    async fn nested_folder_multi_selection_uses_its_parent_in_footer(cx: &mut TestAppContext) {
        use gpui::{MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};

        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        let parent = uuid::Uuid::from_u128(1);
        let first = uuid::Uuid::from_u128(2);
        let second = uuid::Uuid::from_u128(3);
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Folder, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.revision > 0).await;
        // Install a container directly: this exercises nested selection and
        // footer routing independently of the backend's folder promotion.
        navigator.update(cx, |navigator, cx| {
            use super::super::listing::{BrowseSection, chip_id};
            use library_model::BrowseRow;
            let row = |id, name: &str| BrowseRow { id: BrowseLocation::Folder(id), name: name.into(), path: None, book_count: 0, downloaded_book_count: 0 };
            let parent_row = row(parent, "Parent");
            navigator.listing.sections = vec![BrowseSection { parent: parent_row.clone(), children: vec![row(first, "First"), row(second, "Second")], books: Vec::new(), book_count: Some(0) }];
            navigator.listing.children = Rc::new(vec![parent_row.clone()]);
            navigator.listing.items = Rc::new(std::collections::HashMap::from([(chip_id(parent_row.id), BrowseItem::Child { row: parent_row, parent: Some(ROOT_DIR_ID) })]));
            navigator.listing.page = None;
            cx.emit(Loaded { left: None, refresh: false });
        });
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        let first = visual.debug_bounds(format!("browse-section-subject-{}", BrowseLocation::Folder(first)).leak()).unwrap().center();
        let second = visual.debug_bounds(format!("browse-section-subject-{}", BrowseLocation::Folder(second)).leak()).unwrap().center();
        visual.simulate_event(MouseDownEvent { position: first, button: MouseButton::Left, ..Default::default() });
        visual.simulate_event(MouseMoveEvent { position: second, pressed_button: Some(MouseButton::Left), ..Default::default() });
        visual.run_until_parked();
        visual.simulate_event(MouseUpEvent { position: second, button: MouseButton::Left, ..Default::default() });
        visual.run_until_parked();
        visual.read_entity(&page, |page, cx| {
            let (source, items) = page.cards.read(cx).selected_items(cx);
            assert_eq!(source, Some(parent));
            assert_eq!(items.len(), 2);
        });
        assert!(visual.debug_bounds("selection-action-move").is_some());
        assert!(visual.debug_bounds("selection-action-copy").is_some());
        let clear = visual.debug_bounds("clear-browse-selection").unwrap().center();
        visual.simulate_click(clear, Default::default());
        visual.run_until_parked();
        assert!(visual.debug_bounds("browse-selection-footer").is_none());
    }

    #[gpui::test]
    async fn create_folder_from_menu_keeps_enter_in_dialog(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        library.backend().create_directory(ROOT_DIR_ID, "Existing".into()).await.unwrap();
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Folder, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.children.iter().any(|row| row.name == "Existing")).await;
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
            page.update(cx, |page, cx| {
                page.focus.focus(window, cx);
                page.cards.update(cx, |cards, cx| assert!(cards.handle_key("down", window, cx)));
                page.open_folder_menu(Point::default(), window, cx);
            });
        });
        visual.simulate_keystrokes("down down down enter");
        visual.run_until_parked();
        visual.read_entity(&page, |page, cx| assert!(page.has_popup(cx)));
        visual.simulate_keystrokes("enter");
        visual.read_entity(&page, |page, cx| {
            assert!(page.has_popup(cx));
            assert_eq!(page.navigator.read(cx).destination.location, BrowseLocation::Folder(ROOT_DIR_ID));
        });
        visual.simulate_input("Created");
        visual.simulate_keystrokes("enter");
        visual.condition(&navigator, |navigator, _| navigator.listing.children.iter().any(|row| row.name == "Created")).await;
        visual.read_entity(&page, |page, cx| {
            assert!(!page.has_popup(cx));
            assert_eq!(page.navigator.read(cx).destination.location, BrowseLocation::Folder(ROOT_DIR_ID));
        });
    }

    #[gpui::test]
    async fn open_folder_updates_after_a_directory_import(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        let external = tempfile::tempdir().unwrap();
        let epub = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../client/library-backend/crates/book-inspection/tests/fixtures/back-matter-evidence.epub"));
        let shelf_source = external.path().join("Shelf");
        fs::create_dir(&shelf_source).unwrap();
        fs::write(shelf_source.join("existing.epub"), epub).unwrap();
        assert!(library.backend().import_paths(ROOT_DIR_ID, vec![shelf_source], false).await.unwrap().is_empty());
        let page = cx.new(|cx| BrowsePage::new(BrowseKind::Folder, library.clone(), cx));
        let navigator = page.read_with(cx, |page, _| page.navigator.clone());
        let window = cx.add_window(|window, cx| Root::new(page.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.revision > 0).await;
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let shelf = visual.read_entity(&navigator, |navigator, _| navigator.listing.children.iter().find(|row| row.name == "Shelf").unwrap().id.folder().unwrap());

        let source = external.path().join("Imported book");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("book.epub"), epub).unwrap();
        assert!(library.backend().import_paths(shelf, vec![source], false).await.unwrap().is_empty());
        visual.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        visual.condition(&navigator, |navigator, _| navigator.listing.sections.iter().any(|section| section.children.iter().any(|row| row.name == "Imported book"))).await;
        let imported = visual.read_entity(&navigator, |navigator, _| navigator.listing.sections.iter().flat_map(|section| &section.children).find(|row| row.name == "Imported book").unwrap().id);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let selector = format!("browse-section-subject-{imported}").leak();
        assert!(visual.debug_bounds(selector).is_some(), "the existing section must paint its imported child");
    }
}
