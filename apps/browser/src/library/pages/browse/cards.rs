//! The cards view of a browse page: the navigator's listing as chips, section
//! cards and book cards in one paginated grid. What a card asks for — to open
//! a child, a menu, a drop — goes to the page through [`CardEvents`], since
//! only the page knows what its kind does with it.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, Context, Entity, IntoElement, Pixels, Point, Render, Subscription, Window};
use library_model::BrowseLocation;
use sync_common::DirId;

use super::drag_drop::FolderDrop;
use super::listing::{BrowseItem, resolve_items};
use super::navigator::{BrowseNavigator, Loaded};
use super::page::BrowseSplitSide;
use super::section_card::SectionCard;
use crate::library::widgets::{CursorMove, Paginator, PaginatorAxis, PaginatorGroup, PaginatorPosition, activate_paginator};

/// What a card asks the page to do.
pub(super) enum CardEvent {
    OpenChild(BrowseLocation),
    SplitChild(BrowseLocation, BrowseSplitSide),
    /// A menu over these items, at a window position.
    ItemsMenu(Vec<BrowseItem>, Point<Pixels>),
    /// A menu over open space, at a window position.
    SpaceMenu(Point<Pixels>),
    Download(BrowseLocation),
    /// Files to import or library items to place into this folder.
    DropOnto(DirId, FolderDrop),
}

pub(super) type CardEvents = Rc<dyn Fn(CardEvent, &mut Window, &mut App)>;

pub(super) struct CardView {
    navigator: Entity<BrowseNavigator>,
    events: CardEvents,
    paginator: Entity<Paginator>,
    /// The cards of a sectioned listing, by the id of the chip each belongs to.
    /// Kept across refreshes so a card keeps its strip's position.
    section_cards: HashMap<BrowseLocation, Entity<SectionCard>>,
    selected_section: Option<BrowseLocation>,
    _section_subscriptions: Vec<Subscription>,
    positions: HashMap<BrowseLocation, PaginatorPosition>,
    _subscriptions: [Subscription; 3],
}

impl CardView {
    /// `selectable` lets a rubber band pick up several items, for the commands
    /// that act on many at once.
    pub(super) fn new(navigator: Entity<BrowseNavigator>, selectable: bool, events: CardEvents, cx: &mut Context<Self>) -> Self {
        let prefix = navigator.read(cx).kind.list_prefix();
        let menu_navigator = navigator.downgrade();
        let menu_events = events.clone();
        let paginator = cx.new(|cx| {
            let paginator = Paginator::new(format!("{prefix}-browse-paginator"), []).flow_axis(PaginatorAxis::Vertical);
            if !selectable {
                return paginator;
            }
            let this = cx.entity().downgrade();
            paginator.selectable().on_context_menu(move |id, position, window, cx| {
                let event = match id {
                    Some(id) => {
                        let (Some(navigator), Some(paginator)) = (menu_navigator.upgrade(), this.upgrade()) else { return };
                        let listing = &navigator.read(cx).listing;
                        // Control-click on a child splits instead, from its own handler.
                        if window.modifiers().control && crate::split_panes_available(window) && listing.item(&id).is_some_and(|item| item.child().is_some()) {
                            return;
                        }
                        CardEvent::ItemsMenu(resolve_items(&paginator.read(cx).target_ids(&id), &listing.items), position)
                    }
                    None => CardEvent::SpaceMenu(position),
                };
                menu_events(event, window, cx);
            })
        });
        let mut previous_selection = Vec::new();
        let subscriptions = [
            cx.subscribe(&navigator, |cards, _, loaded: &Loaded, cx| cards.install(loaded, cx)),
            // A feed lists its next page once the grid reaches its end.
            cx.observe(&paginator, move |cards, paginator, cx| {
                let flow = paginator.read(cx);
                let selected = flow.selection().iter().copied().collect::<Vec<_>>();
                if previous_selection != selected {
                    previous_selection = selected;
                    if !previous_selection.is_empty() {
                        cards.selected_section = None;
                        cards.clear_section_selections(None, cx);
                    }
                    cx.notify();
                }
                let flow = paginator.read(cx);
                let range = flow.visible_child_range().map(|range| range.start..flow.adjacent_child_range(true).map_or(range.end, |next| next.end.max(range.end)));
                cards.load_visible_page(range, cx);
                if paginator.read(cx).on_last_page() {
                    cards.navigator.update(cx, |navigator, cx| navigator.load_more(cx));
                }
            }),
            cx.observe_global::<crate::stores::CoverTextSetting>(|cards, cx| cards.refresh_layout(cx)),
        ];
        Self { navigator, events, paginator, section_cards: HashMap::new(), selected_section: None, _section_subscriptions: Vec::new(), positions: HashMap::new(), _subscriptions: subscriptions }
    }

    fn install(&mut self, loaded: &Loaded, cx: &mut Context<Self>) {
        if let Some(left) = loaded.left {
            self.positions.insert(left, self.paginator.read(cx).position());
        }
        let groups = self.groups(cx);
        let location = self.navigator.read(cx).destination.location;
        let restored = loaded.left.and_then(|_| self.positions.get(&location).cloned());
        self.paginator.update(cx, |paginator, cx| paginator.install_groups(groups, loaded.refresh, restored.as_ref(), cx));
        cx.notify();
    }

    fn load_visible_page(&self, range: Option<Range<usize>>, cx: &mut Context<Self>) {
        let Some(range) = range else { return };
        let navigator = self.navigator.read(cx);
        if navigator.listing.page.is_none() {
            return;
        }
        let compact = navigator.listing.compact_slot_count();
        self.navigator.update(cx, |navigator, cx| navigator.load_page_range(range, compact, cx));
    }

    /// Rebuilds row heights when the cover text setting changes.
    ///
    /// The cards themselves pick the change up on their own — see
    /// `BookCardView`'s own global observer — but the row height the
    /// paginator reserves for them is decided here, and has to be recomputed
    /// alongside them or the grid keeps the old gap under a card that no
    /// longer has anything in it.
    fn refresh_layout(&mut self, cx: &mut Context<Self>) {
        let groups = self.groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.refresh_groups(groups, cx));
        cx.notify();
    }

    fn groups(&mut self, cx: &mut Context<Self>) -> Vec<PaginatorGroup> {
        let navigator = self.navigator.read(cx);
        let library_id = navigator.library_id();
        let (kind, sections, children, books, items) = (navigator.kind, navigator.listing.sections.clone(), navigator.listing.children.clone(), navigator.listing.books.clone(), navigator.listing.items.clone());
        // Only a folder listing can be dragged: its children are directories,
        // and its books sit in a folder a move can name as their source.
        let source = navigator.destination.location.folder();
        let revision = navigator.listing.revision;
        let mut cards = HashMap::new();
        for section in &sections {
            let id = section.parent.id;
            let card = match self.section_cards.remove(&id) {
                Some(card) => {
                    card.update(cx, |card, cx| card.set_section(section, revision, cx));
                    card
                }
                None => {
                    let events = self.events.clone();
                    let navigator = self.navigator.clone();
                    cx.new(|cx| SectionCard::new(events, library_id, format!("{}-section-{id}", kind.list_prefix()), kind, section, navigator, revision, cx))
                }
            };
            cards.insert(id, card);
        }
        self.section_cards = cards;
        self._section_subscriptions = self
            .section_cards
            .iter()
            .map(|(&id, card)| {
                let mut previous = card.read(cx).selected_ids(cx);
                cx.observe(card, move |cards, card, cx| {
                    let selected = card.read(cx).selected_ids(cx);
                    if previous == selected {
                        return;
                    }
                    previous = selected;
                    if !previous.is_empty() {
                        cards.selected_section = Some(id);
                        cards.paginator.update(cx, |flow, cx| flow.clear_selection(cx));
                        cards.clear_section_selections(Some(id), cx);
                    }
                    cx.notify();
                })
            })
            .collect();
        let navigator = self.navigator.read(cx);
        let page = navigator.listing.page.clone();
        let book_indices = navigator.listing.book_indices.clone();
        let heading_rows = navigator.listing.heading_rows.clone();
        super::layout::paginator_groups(self.events.clone(), library_id, children, kind, books, source, &self.section_cards, items, page.as_ref(), &book_indices, &heading_rows, cx)
    }

    pub(super) fn is_empty(&self, cx: &App) -> bool {
        self.paginator.read(cx).is_empty()
    }

    /// Moves, activates or pages with a plain key. Returns whether it was used.
    pub(super) fn handle_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // Arrows move the cursor; the page turns as a consequence of the cursor
        // walking off it. PageUp and PageDown remain the explicit whole-page
        // jump, and are the only way to page without moving the cursor.
        if let Some(movement) = CursorMove::from_key(key) {
            return self.paginator.update(cx, |paginator, cx| paginator.move_cursor(movement, cx));
        }
        match key {
            "enter" => {
                activate_paginator(&self.paginator, window, cx);
                true
            }
            "pageup" | "pagedown" => self.paginator.update(cx, |paginator, cx| if key == "pagedown" { paginator.next_with_axis(PaginatorAxis::Vertical, cx) } else { paginator.previous_with_axis(PaginatorAxis::Vertical, cx) }),
            _ => false,
        }
    }

    pub(super) fn toggle_selection(&mut self, movement: CursorMove, cx: &mut Context<Self>) {
        self.paginator.update(cx, |paginator, cx| paginator.move_and_toggle_selection(movement, cx));
    }

    /// The selection, or the item under the keyboard cursor, with its menu
    /// anchored below the first target rather than under the pointer.
    pub(super) fn cursor_menu(&self, cx: &App) -> Option<CardEvent> {
        let paginator = self.paginator.read(cx);
        let cursor = paginator.context_menu_index()?;
        // A keyboard selection can span pages; anchor to the visible cursor
        // when its first selected item is on another page.
        let bounds = paginator.child_bounds(cursor).or_else(|| paginator.cursor().and_then(|index| paginator.child_bounds(index)))?;
        let id = paginator.child_id(cursor)?;
        let items = resolve_items(&paginator.target_ids(id), &self.navigator.read(cx).listing.items);
        Some(CardEvent::ItemsMenu(items, gpui::point(bounds.left(), bounds.bottom())))
    }

    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let outer = self.paginator.update(cx, |paginator, cx| paginator.clear_selection(cx));
        let inner = self.clear_section_selections(None, cx);
        self.selected_section = None;
        outer || inner
    }

    fn clear_section_selections(&self, keep: Option<BrowseLocation>, cx: &mut Context<Self>) -> bool {
        let mut cleared = false;
        for (&id, card) in &self.section_cards {
            if Some(id) != keep {
                cleared |= card.update(cx, |card, cx| card.clear_selection(cx));
            }
        }
        cleared
    }

    pub(super) fn selected_items(&self, cx: &App) -> (Option<DirId>, Vec<BrowseItem>) {
        if let Some(card) = self.selected_section.and_then(|id| self.section_cards.get(&id)) {
            return card.read(cx).selected_items(cx);
        }
        let flow = self.paginator.read(cx);
        let ids = flow.selection().iter().filter_map(|&index| flow.child_id(index).cloned()).collect::<Vec<_>>();
        let navigator = self.navigator.read(cx);
        (navigator.destination.location.folder(), resolve_items(&ids, &navigator.listing.items))
    }
}

impl Render for CardView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.paginator.clone()
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::super::listing::{BrowseKind, BrowseListing};
    use super::*;
    use crate::navigation::LibrarySession;
    use gpui::{TestAppContext, VisualTestContext, px, size};
    use std::cell::Cell;

    #[gpui::test]
    async fn crossing_compact_breakpoints_does_not_rebuild_the_card_groups(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        let listing = BrowseListing::new(library.clone(), BrowseKind::Folder, Rc::new(|_, _, _| {}), false);
        let navigator = cx.new(|cx| BrowseNavigator::new(library, BrowseKind::Folder, listing, cx));
        let cards = cx.new(|cx| CardView::new(navigator.clone(), true, Rc::new(|_, _, _| {}), cx));
        let window = cx.add_window(|window, cx| gpui_component::Root::new(cards.clone(), window, cx));
        cx.condition(&navigator, |navigator, _| navigator.listing.revision > 0).await;
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_resize(size(px(1200.0), px(800.0)));
        visual.run_until_parked();
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _subscription = visual.update(|_, cx| cx.observe(&cards, move |_, _| observed.set(observed.get() + 1)));
        for width in [390.0, 1200.0, 390.0, 1200.0] {
            visual.simulate_resize(size(px(width), px(800.0)));
            visual.run_until_parked();
            assert_eq!(notifications.get(), 0, "resizing to {width}px must not invalidate the listing's groups");
        }
    }
}
