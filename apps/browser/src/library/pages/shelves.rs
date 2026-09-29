//! A page of shelves: a vertical paginator of full-width rows, each a heading
//! over a horizontal shelf of book cards. Where the rows come from is a
//! [`ShelfSource`]; everything else (loading, reuse across reloads, the keyboard
//! cursor, paging, the error banner) lives here.
//!
//! The vertical axis is paged rather than scrolled. A paginator turns the wheel
//! into a page it animates, so the page always shows whole rows instead of
//! stopping halfway down a cover.

use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, SharedString, Subscription, Window, div};
use gpui_component::{Disableable, IconName};
use library_backend::LibraryClient;
use library_model::BookCardRow;
use ui_components as components;

use crate::library::pages::library_empty_state;
use crate::library::services::{LatestRequest, LibraryContentsChanged, LibraryContext, LibraryScanChanged};
use crate::library::widgets::{BookCardView, CursorMove, Paginator, PaginatorAxis, PaginatorChild, PaginatorGroup, shelf_card_policy, shelf_rows_policy};

pub(crate) type ShelfFuture = Pin<Box<dyn Future<Output = Result<Vec<ShelfRow>, String>>>>;

/// One row of the page, as its source describes it.
#[derive(Clone)]
pub(crate) struct ShelfRow {
    /// Stable identity across reloads. A row that comes back under the same id
    /// keeps its shelf — and with it the page the reader had turned to.
    pub(crate) id: SharedString,
    pub(crate) title: SharedString,
    pub(crate) books: Vec<BookCardRow>,
}

/// Where a page's rows come from.
pub(crate) trait ShelfSource: 'static {
    /// Prefix for the page's element ids.
    const ID: &'static str;
    /// Shown while the first load is outstanding.
    const LOADING_MESSAGE: &'static str;
    fn load(library: LibraryClient) -> ShelfFuture;
}

/// One row as the page holds it. The heading is data; the shelf is an entity
/// because a carousel owns a page offset that has to survive a repaint.
struct ShelfEntry {
    id: SharedString,
    title: SharedString,
    shelf: Entity<Paginator>,
    cards: Vec<Entity<BookCardView>>,
}

pub(crate) struct ShelfPage<S: ShelfSource> {
    ui: LibraryContext,
    /// One entry per row, in the order the source returned them.
    /// Parallel to the outer paginator's children, so the paginator's cursor
    /// indexes it.
    rows: Vec<ShelfEntry>,
    error: Option<SharedString>,
    refresh: LatestRequest,
    /// Whether the current load has resolved. No rows means "still loading"
    /// before the first result and "nothing to show" after it.
    loaded: bool,
    paginator: Entity<Paginator>,
    focus: FocusHandle,
    _updates: Vec<Subscription>,
    source: PhantomData<S>,
}

impl<S: ShelfSource> ShelfPage<S> {
    pub(crate) fn new(ui: LibraryContext, cx: &mut Context<Self>) -> Self {
        let paginator = cx.new(|_| Paginator::new(format!("{}-rows", S::ID), []));
        let updates = ui.updates();
        let update_subscriptions = vec![
            cx.subscribe(&updates, |page, _, _: &LibraryContentsChanged, cx| page.refresh(cx)),
            cx.subscribe(&updates, |_, _, _: &LibraryScanChanged, cx| cx.notify()),
            cx.observe_global::<crate::stores::CoverTextSetting>(|page, cx| page.refresh_cover_text(cx)),
        ];
        let mut page = Self { ui, rows: Vec::new(), error: None, refresh: LatestRequest::default(), loaded: false, paginator, focus: cx.focus_handle().tab_index(0).tab_stop(true), _updates: update_subscriptions, source: PhantomData };
        page.refresh(cx);
        page
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let generation = self.refresh.begin();
        let library_id = *self.ui.backend().id();
        if S::ID == "home" {
            crate::services::library_add_trace::mark(library_id, "home_request_started");
        }
        let load = S::load(self.ui.backend().clone());
        let task = cx.spawn(async move |page, cx| {
            let result = load.await;
            if S::ID == "home" {
                crate::services::library_add_trace::mark(library_id, "home_response_received");
            }
            cx.defer_update(page, move |page, cx| {
                if !page.refresh.is_current(generation) {
                    return;
                }
                // Either outcome settles the question of whether the page is
                // still waiting: an error surfaces its own banner and must not
                // leave the body claiming to be loading forever.
                if S::ID == "home" {
                    crate::services::library_add_trace::mark(library_id, if result.is_ok() { "home_result_installed" } else { "home_load_failed" });
                }
                page.loaded = true;
                match result {
                    Ok(rows) => {
                        page.error = None;
                        page.install(rows, cx);
                    }
                    Err(error) => {
                        log::error!("failed to load {}: {error}", S::ID);
                        page.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
        self.refresh.install(task);
    }

    fn install(&mut self, rows: Vec<ShelfRow>, cx: &mut Context<Self>) {
        // A reload is not a new page. Cards carry a cover that was fetched and
        // faded in once; rebuilding them for every library update refetches
        // every visible thumbnail and replays every fade. Keep the shelf and the
        // card that already stand for this row and this book, and replace only
        // what they hold.
        //
        // A row states a title over a shelf entity, so the titles and their order
        // are all a rebuilt page would say differently; a shelf's own contents
        // are replaced through the entity the row already holds.
        let unchanged = self.rows.len() == rows.len() && self.rows.iter().zip(&rows).all(|(entry, row)| entry.id == row.id && entry.title == row.title);
        let mut reusable = self.rows.drain(..).map(|entry| (entry.id.clone(), entry)).collect::<HashMap<_, _>>();
        for row in rows {
            let mut entry = reusable.remove(&row.id).unwrap_or_else(|| ShelfEntry {
                id: row.id.clone(),
                title: SharedString::default(),
                shelf: cx.new(|_| Paginator::new(format!("{}-shelf-{}", S::ID, row.id), []).max_rows(1).flow_axis(PaginatorAxis::Horizontal).horizontal_only()),
                cards: Vec::new(),
            });
            entry.title = row.title;

            let shelf_unchanged = entry.cards.iter().map(|card| card.read(cx).content_hash()).eq(row.books.iter().map(|book| book.content_hash));
            let mut cards = entry.cards.drain(..).map(|card| (card.read(cx).content_hash(), card)).collect::<HashMap<_, _>>();
            for book in row.books {
                let card = match cards.remove(&book.content_hash) {
                    Some(card) => {
                        card.update(cx, |card, _| card.replace_row(book, None));
                        card
                    }
                    None => BookCardView::create(book, self.ui.clone(), cx),
                };
                entry.cards.push(card);
            }
            // Rebuilding the children resets the shelf to its first page and
            // discards what it has prefetched. When the same books are still
            // here in the same order, there is nothing to rebuild.
            if !shelf_unchanged {
                let children = Self::shelf_children(&entry.cards);
                let cover_text = crate::stores::current_cover_text(cx);
                entry.shelf.update(cx, |shelf, cx| shelf.set_groups([PaginatorGroup::new(shelf_card_policy(cover_text), children)], cx));
            }
            self.rows.push(entry);
        }
        if unchanged {
            return;
        }
        let groups = self.row_groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.set_groups(groups, cx));
    }

    /// One shelf's cards as the paginator wants them, in the order the shelf
    /// holds them. Shared between `install`, which builds this the first time
    /// a shelf's books change, and the "cover text" observer, which rebuilds
    /// it for every shelf when the reserved row height has to change with it.
    fn shelf_children(cards: &[Entity<BookCardView>]) -> Vec<PaginatorChild> {
        cards
            .iter()
            .map(|card| {
                let card = card.clone();
                let prefetched_card = card.clone();
                let open_card = card.clone();
                PaginatorChild::new(move |_, _, _| div().w_full().min_w_0().child(card.clone()).into_any_element())
                    .with_activate(move |window, cx| BookCardView::activate(&open_card, window, cx))
                    .with_prepare(move |window, cx| BookCardView::prefetch_for_window(&prefetched_card, window, cx))
            })
            .collect()
    }

    /// Rebuilds every shelf's row height and the page's own row heights for
    /// the current "cover text" setting.
    ///
    /// Unlike `install`, this cannot skip a shelf whose books are unchanged —
    /// the setting changed, not the books, so every shelf's reserved height is
    /// stale regardless of whether its cards are.
    fn refresh_cover_text(&mut self, cx: &mut Context<Self>) {
        let cover_text = crate::stores::current_cover_text(cx);
        for entry in &self.rows {
            let children = Self::shelf_children(&entry.cards);
            entry.shelf.update(cx, |shelf, cx| shelf.set_groups([PaginatorGroup::new(shelf_card_policy(cover_text), children)], cx));
        }
        let groups = self.row_groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.set_groups(groups, cx));
        cx.notify();
    }

    /// The rows, as one group of full-width children.
    fn row_groups(&self, cx: &mut Context<Self>) -> Vec<PaginatorGroup> {
        let show_controls = !cx.supports_touch_input();
        let children = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let id = entry.id.clone();
                let title = entry.title.clone();
                let shelf = entry.shelf.clone();
                PaginatorChild::new(move |state, _, cx| {
                    let theme = components::browser_theme(cx);
                    let mut heading = components::shelf_heading().child(components::shelf_title(title.clone(), state.cursor == Some(index), theme));
                    if show_controls {
                        heading = heading.child(shelf_controls(&id, &shelf, theme, cx));
                    }
                    components::shelf_row().child(heading).child(components::shelf_carousel().child(shelf.clone())).into_any_element()
                })
            })
            .collect::<Vec<_>>();
        if children.is_empty() {
            return Vec::new();
        }
        vec![PaginatorGroup::new(shelf_rows_policy(crate::stores::current_cover_text(cx)), children)]
    }

    /// Up and down walk the rows and turn the page at the edges; left and right
    /// walk the cards of the row the cursor is on. The two axes belong to two
    /// different paginators, which is what makes a shelf browsable without
    /// leaving the page.
    fn navigate(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }
        let handled = match event.keystroke.key.as_str() {
            // Above the first row is the library the page belongs to, so the
            // cursor leaves upwards into the switcher rather than stopping.
            "up" => {
                self.move_row(CursorMove::Up, cx) || {
                    window.dispatch_action(Box::new(crate::OpenLibrarySwitcher), cx);
                    true
                }
            }
            "down" => self.move_row(CursorMove::Down, cx),
            "left" => self.move_card(CursorMove::Left, cx),
            "right" => self.move_card(CursorMove::Right, cx),
            "pageup" => self.selected_shelf(cx).is_some_and(|shelf| shelf.update(cx, |shelf, cx| shelf.previous(cx))),
            "pagedown" => self.selected_shelf(cx).is_some_and(|shelf| shelf.update(cx, |shelf, cx| shelf.next(cx))),
            "enter" => self.selected_shelf(cx).is_some_and(|shelf| {
                window.defer(cx, move |window, cx| {
                    if let Some(activate) = shelf.read(cx).cursor_activation() {
                        activate(window, cx);
                    }
                });
                true
            }),
            _ => false,
        };
        if handled {
            self.sync_cursor(cx);
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The cursor lives on the outer paginator, but the rows were built with the
    /// value it held when they were built. Rebuilding them is what carries the
    /// move into the heading's tint.
    fn move_row(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        if !self.paginator.update(cx, |paginator, cx| paginator.move_cursor(movement, cx)) {
            return false;
        }
        // Entering a row puts the cursor on a book, so the next left or right is
        // a step along the shelf rather than the keystroke that places it.
        if let Some(shelf) = self.selected_shelf(cx) {
            shelf.update(cx, |shelf, cx| {
                if shelf.cursor().is_none() {
                    shelf.move_cursor(CursorMove::Right, cx);
                }
            });
        }
        let groups = self.row_groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.refresh_groups(groups, cx));
        true
    }

    /// Left and right move along the selected shelf. With no row selected yet
    /// the first press selects one, so the cursor appears where the reader is
    /// looking rather than nowhere.
    fn move_card(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        let Some(shelf) = self.selected_shelf(cx) else {
            return self.move_row(CursorMove::Down, cx);
        };
        shelf.update(cx, |shelf, cx| shelf.move_cursor(movement, cx))
    }

    fn selected_shelf(&self, cx: &gpui::App) -> Option<Entity<Paginator>> {
        let index = self.paginator.read(cx).cursor()?;
        self.rows.get(index).map(|entry| entry.shelf.clone())
    }

    /// A card knows what focused looks like; the page knows where the cursor is.
    /// Only the selected row has one, so a shelf the reader has left keeps its
    /// page but not its highlight.
    fn sync_cursor(&self, cx: &mut Context<Self>) {
        let selected = self.paginator.read(cx).cursor();
        for (index, entry) in self.rows.iter().enumerate() {
            let cursor = (selected == Some(index)).then(|| entry.shelf.read(cx).cursor()).flatten();
            for (card_index, card) in entry.cards.iter().enumerate() {
                card.update(cx, |card, cx| card.set_focused(cursor == Some(card_index), cx));
            }
        }
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        // Nothing to show is either a library that is still arriving or one that
        // holds no books. Saying nothing at all leaves a blank page in both.
        if self.rows.is_empty() && self.error.is_none() {
            return if self.ui.updates().read(cx).scanning() {
                components::browser_loading_message("Scanning library…", theme).into_any_element()
            } else if self.loaded && !self.ui.updates().read(cx).loading() {
                library_empty_state(theme)
            } else {
                components::browser_loading_message(S::LOADING_MESSAGE, theme).into_any_element()
            };
        }
        let body = components::shelf_page().child(self.paginator.clone()).into_any_element();
        if let Some(error) = self.error.clone() {
            let page = cx.entity();
            let retry = components::topbar_action_button(format!("{}-retry", S::ID), "Retry", IconName::Redo2, theme).on_click(move |_, _, cx| page.update(cx, |page, cx| page.refresh(cx)));
            components::browser_error_banner(error, retry, body).into_any_element()
        } else {
            body
        }
    }
}

/// A shelf's own controls, which step it sideways. They act on the shelf entity
/// directly: a row is drawn inside the outer paginator's render, where the page
/// itself is not reachable.
fn shelf_controls(id: &SharedString, shelf: &Entity<Paginator>, theme: components::BrowserTheme, cx: &gpui::App) -> gpui::Div {
    let state = shelf.read(cx);
    let (has_previous, has_next, can_previous, can_next) = (state.has_previous(), state.has_next(), state.can_previous(), state.can_next());
    let previous = has_previous.then(|| {
        let shelf = shelf.clone();
        components::outlined_icon_button(format!("shelf-{id}-previous"), "Previous", IconName::ChevronLeft, theme)
            .disabled(!can_previous)
            .on_click(move |_, _, cx| {
                shelf.update(cx, |shelf, cx| shelf.previous(cx));
            })
            .into_any_element()
    });
    let next = has_next.then(|| {
        let shelf = shelf.clone();
        components::outlined_icon_button(format!("shelf-{id}-next"), "Next", IconName::ChevronRight, theme)
            .disabled(!can_next)
            .on_click(move |_, _, cx| {
                shelf.update(cx, |shelf, cx| shelf.next(cx));
            })
            .into_any_element()
    });
    components::paginator_controls(previous, next)
}

impl<S: ShelfSource> Render for ShelfPage<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.loaded && self.error.is_none() && S::ID == "home" {
            let library_id = *self.ui.backend().id();
            if crate::services::library_add_trace::mark(library_id, "home_content_render") {
                window.on_next_frame(move |_, _| {
                    crate::services::library_add_trace::mark(library_id, "home_frame_completed");
                });
            }
        }
        let body = self.render_body(cx);
        components::full_size_column().track_focus(&self.focus).on_key_down(cx.listener(Self::navigate)).child(components::remaining_space_column().child(body))
    }
}

impl<S: ShelfSource> Focusable for ShelfPage<S> {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}
