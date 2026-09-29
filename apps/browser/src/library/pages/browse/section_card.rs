//! One card of a sectioned listing. Child-entry columns and book covers share
//! one horizontal paginator, with entries first. The card owns the keyboard
//! within either kind of item and releases it at the outer edge.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, IntoElement, MouseButton, Render, SharedString, Subscription, WeakEntity, Window, div};
use gpui_component::IconName;
use library_model::BrowseRow;
use sync_common::DirId;
use ui_components as components;

use super::folder_actions::FolderTargets;
use super::layout::{DragPreviewItem, folder_drag_source, folder_drop_target};
use super::listing::BrowseSection;
use super::page::BrowsePage;
use crate::library::widgets::{BookCardView, CursorMove, Paginator, PaginatorAxis, PaginatorChild, PaginatorGroup, browse_section_item_policy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SectionRail {
    Subjects,
    Books,
}

pub(super) struct SectionCard {
    page: WeakEntity<BrowsePage>,
    id: String,
    child_icon: components::NavigationIcon,
    band_index: usize,
    /// The folder this card shows, on a folder page. Its subfolders and books
    /// are dragged out of it, and subfolders take drops.
    folder: Option<DirId>,
    subjects: Vec<BrowseRow>,
    subject_cursor: Option<usize>,
    flow: Entity<Paginator>,
    pager: Entity<SectionPager>,
    focused_subject: Rc<Cell<Option<usize>>>,
    book_entities: Vec<Entity<BookCardView>>,
    /// The part holding the keyboard, if the card has it at all.
    rail: Option<SectionRail>,
}

impl SectionCard {
    pub(super) fn new(page: WeakEntity<BrowsePage>, id: String, child_icon: components::NavigationIcon, section: &BrowseSection, cx: &mut Context<Self>) -> Self {
        let folder = (child_icon == components::NavigationIcon::Folder).then(|| DirId::parse_str(&section.parent.id).ok()).flatten();
        let color_key = if folder.is_some() { &section.parent.name } else { &section.parent.id };
        let band_index = components::section_card_band_index(child_icon, color_key);
        let focused_subject = Rc::new(Cell::new(None));
        let groups = mixed_groups(&id, page.clone(), section, folder, child_icon, focused_subject.clone());
        let flow = cx.new(|_| Paginator::new(format!("{id}-contents"), groups).flow_axis(PaginatorAxis::Horizontal).max_rows(1).horizontal_only());
        let card = cx.entity().downgrade();
        let pager = cx.new(|cx| SectionPager::new(id.clone(), card, flow.clone(), cx));
        Self { page, id, child_icon, band_index, folder, subjects: section.children.clone(), subject_cursor: None, flow, pager, focused_subject, book_entities: section.books.clone(), rail: None }
    }

    /// Installs a refreshed section in place, keeping each part's position.
    pub(super) fn set_section(&mut self, section: &BrowseSection, cx: &mut Context<Self>) {
        let color_key = if self.folder.is_some() { &section.parent.name } else { &section.parent.id };
        self.band_index = components::section_card_band_index(self.child_icon, color_key);
        let groups = mixed_groups(&self.id, self.page.clone(), section, self.folder, self.child_icon, self.focused_subject.clone());
        self.flow.update(cx, |paginator, cx| paginator.refresh_groups(groups, cx));
        self.subjects = section.children.clone();
        self.book_entities = section.books.clone();
        self.subject_cursor = self.subject_cursor.map(|cursor| cursor.min(self.subjects.len().saturating_sub(1)));
        if self.rail.is_some_and(|rail| !self.has_items(rail)) {
            self.rail = None;
        }
        self.sync(cx);
    }

    pub(super) fn has_subjects(&self) -> bool {
        !self.subjects.is_empty()
    }

    pub(super) fn has_books(&self) -> bool {
        !self.book_entities.is_empty()
    }

    pub(super) fn pager(&self) -> Entity<SectionPager> {
        self.pager.clone()
    }

    pub(super) fn band_index(&self) -> usize {
        self.band_index
    }

    pub(super) fn direct_book_count(&self) -> usize {
        self.book_entities.len()
    }

    fn subject_columns(&self) -> usize {
        self.subjects.len().div_ceil(components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN)
    }

    pub(super) fn is_focused(&self) -> bool {
        self.rail.is_some()
    }

    fn has_items(&self, rail: SectionRail) -> bool {
        match rail {
            SectionRail::Subjects => self.has_subjects(),
            SectionRail::Books => self.has_books(),
        }
    }

    /// Takes the keyboard from a move arriving at the card. Moving left enters
    /// the covers, since that is the side it came in from; anything else
    /// enters the subjects. Arriving from the right or below lands on the last
    /// item.
    pub(super) fn enter(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        let rail = if movement == CursorMove::Left && self.has_books() || !self.has_subjects() { SectionRail::Books } else { SectionRail::Subjects };
        self.focus(rail, matches!(movement, CursorMove::Left | CursorMove::Up), cx)
    }

    pub(super) fn leave(&mut self, cx: &mut Context<Self>) {
        if self.rail.take().is_some() {
            self.sync(cx);
        }
    }

    fn focus(&mut self, rail: SectionRail, last: bool, cx: &mut Context<Self>) -> bool {
        if !self.has_items(rail) {
            return false;
        }
        self.rail = Some(rail);
        match rail {
            SectionRail::Subjects => {
                let last_subject = self.subjects.len() - 1;
                self.subject_cursor = Some(if last { last_subject } else { self.subject_cursor.unwrap_or(0).min(last_subject) });
                let column = self.subject_cursor.expect("subject focus has a cursor") / components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN;
                self.flow.update(cx, |paginator, cx| paginator.restore_cursor(column, column, cx));
            }
            SectionRail::Books => {
                let first = self.subject_columns();
                let target = if last { first + self.book_entities.len() - 1 } else { first };
                self.flow.update(cx, |paginator, cx| paginator.restore_cursor(target, target, cx));
            }
        }
        self.sync(cx);
        true
    }

    /// Child entries fill columns from top to bottom. Horizontal movement
    /// crosses columns before it reaches the book strip.
    fn move_subject_cursor(&mut self, movement: CursorMove) -> bool {
        let (count, rows) = (self.subjects.len(), components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN);
        let Some(current) = self.subject_cursor else { return false };
        let (column, row) = (current / rows, current % rows);
        let target = match movement {
            CursorMove::Up => (row > 0).then(|| current - 1),
            CursorMove::Down => (row + 1 < rows && current + 1 < count).then(|| current + 1),
            CursorMove::Left => column.checked_sub(1).map(|previous| previous * rows + row),
            CursorMove::Right => {
                let next_start = (column + 1) * rows;
                (next_start < count).then(|| (next_start + row).min(count - 1))
            }
        };
        let Some(target) = target else { return false };
        self.subject_cursor = Some(target);
        true
    }

    /// Moves within the focused part. Returns false when the move runs off the
    /// card, for the page to continue it in the outer listing.
    pub(super) fn move_cursor(&mut self, movement: CursorMove, cx: &mut Context<Self>) -> bool {
        let Some(rail) = self.rail else { return false };
        let moved = match rail {
            SectionRail::Subjects => {
                let previous_column = self.subject_cursor.map(|cursor| cursor / components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN);
                let moved = self.move_subject_cursor(movement);
                let next_column = self.subject_cursor.map(|cursor| cursor / components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN);
                if moved && previous_column != next_column {
                    let column = next_column.expect("moving a subject keeps a cursor");
                    self.flow.update(cx, |paginator, cx| paginator.restore_cursor(column, column, cx));
                }
                moved
            }
            SectionRail::Books => {
                let paginator = self.flow.clone();
                let moved = paginator.update(cx, |paginator, cx| paginator.move_cursor(movement, cx));
                // A page still turning swallows the key rather than leaving
                // the card from a position the reader cannot see yet.
                if !moved && !paginator.read(cx).is_idle() {
                    return true;
                }
                if moved && paginator.read(cx).cursor().is_some_and(|cursor| cursor < self.subject_columns()) {
                    self.rail = Some(SectionRail::Subjects);
                    self.subject_cursor = Some(self.subjects.len() - 1);
                }
                moved
            }
        };
        if moved {
            self.sync(cx);
            return true;
        }
        // The covers sit to the right of the subjects, so a horizontal edge
        // crosses to the other part before it leaves the card.
        match (movement, rail) {
            (CursorMove::Right, SectionRail::Subjects) if self.has_books() => self.focus(SectionRail::Books, false, cx),
            (CursorMove::Left, SectionRail::Books) if self.has_subjects() => self.focus(SectionRail::Subjects, true, cx),
            _ => false,
        }
    }

    /// Tab walks from the subjects to the covers and Shift-Tab back. Past
    /// either end the card lets go of the keyboard and returns false, leaving
    /// the key to the ordinary focus chain.
    pub(super) fn tab(&mut self, shift: bool, cx: &mut Context<Self>) -> bool {
        let next = match (self.rail, shift) {
            (Some(SectionRail::Subjects), false) if self.has_books() => Some(SectionRail::Books),
            (Some(SectionRail::Books), true) if self.has_subjects() => Some(SectionRail::Subjects),
            _ => None,
        };
        match next {
            Some(rail) => self.focus(rail, shift, cx),
            None => {
                self.leave(cx);
                false
            }
        }
    }

    /// Turns the shared strip through entry columns and then book covers.
    pub(super) fn turn_page(&mut self, next: bool, cx: &mut Context<Self>) -> bool {
        let moved = self.flow.update(cx, |paginator, cx| if next { paginator.next_with_axis(PaginatorAxis::Horizontal, cx) } else { paginator.previous_with_axis(PaginatorAxis::Horizontal, cx) });
        if !moved {
            return false;
        }
        let anchor = self.flow.read(cx).child_anchor();
        self.flow.update(cx, |paginator, cx| paginator.focus_child_on_current_page(anchor, cx));
        if self.rail.is_some() {
            if anchor < self.subject_columns() {
                self.rail = Some(SectionRail::Subjects);
                self.subject_cursor = Some((anchor * components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN).min(self.subjects.len() - 1));
            } else {
                self.rail = Some(SectionRail::Books);
            }
        }
        self.sync(cx);
        true
    }

    /// What Enter would run: the focused item, if it is on screen. Handed out
    /// rather than run, because it navigates the page that is asking.
    pub(super) fn activation(&self, cx: &App) -> Option<Rc<dyn Fn(&mut Window, &mut App)>> {
        match self.rail? {
            SectionRail::Subjects => {
                let location = self.subjects.get(self.subject_cursor?)?.id.clone();
                let page = self.page.clone();
                Some(Rc::new(move |window, cx| {
                    let _ = page.update(cx, |page, cx| page.open_child(location.clone(), window, cx));
                }))
            }
            SectionRail::Books => {
                let paginator = self.flow.read(cx);
                paginator.child_bounds(paginator.cursor()?)?;
                paginator.cursor_activation()
            }
        }
    }

    /// The subjects draw their own cursor; book cards are entities and are
    /// told.
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.focused_subject.set((self.rail == Some(SectionRail::Subjects)).then_some(self.subject_cursor).flatten());
        let book_cursor = (self.rail == Some(SectionRail::Books)).then(|| self.flow.read(cx).cursor()).flatten().and_then(|cursor| cursor.checked_sub(self.subject_columns()));
        for (index, book) in self.book_entities.iter().enumerate() {
            book.update(cx, |book, cx| book.set_focused(book_cursor == Some(index), cx));
        }
        self.flow.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn render_contents(&self) -> AnyElement {
        self.flow.clone().into_any_element()
    }
}

pub(super) struct SectionPager {
    id: String,
    card: WeakEntity<SectionCard>,
    flow: Entity<Paginator>,
    _flow_subscription: Subscription,
}

impl SectionPager {
    fn new(id: String, card: WeakEntity<SectionCard>, flow: Entity<Paginator>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&flow, |_, _, cx| cx.notify());
        Self { id, card, flow, _flow_subscription: subscription }
    }
}

impl Render for SectionPager {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let flow = self.flow.read(cx);
        let previous = flow.has_previous();
        let next = flow.has_next();
        let arrow = |id: &str, label: &str, icon: IconName, enabled: bool, forward: bool| {
            let card = self.card.clone();
            components::section_card_cover_arrow(format!("{}-{id}", self.id), label.to_owned(), icon, enabled)
                .opacity(if enabled { 1.0 } else { 0.35 })
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    if enabled {
                        let _ = card.update(cx, |card, cx| card.turn_page(forward, cx));
                    }
                })
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(gpui::px(4.0))
            .text_size(gpui::rems(0.875))
            .text_color(gpui::rgb(0xf2efe8))
            .child(arrow("previous-contents", "Previous entries or books", IconName::ArrowLeft, previous, false))
            .child(arrow("next-contents", "Next entries or books", IconName::ArrowRight, next, true))
    }
}

impl Render for SectionCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_contents()
    }
}

/// Child-entry columns come first in the same horizontal paginator as the
/// covers. A later page contains only covers once every entry has passed.
fn mixed_groups(id: &str, page: WeakEntity<BrowsePage>, section: &BrowseSection, folder: Option<DirId>, child_icon: components::NavigationIcon, focused_subject: Rc<Cell<Option<usize>>>) -> Vec<PaginatorGroup> {
    let mut children = Vec::new();
    let subject_columns = section.children.len().div_ceil(components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN);
    for (column_index, column) in section.children.chunks(components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN).enumerate() {
        let column = column.to_vec();
        let before_direct_books = column_index + 1 == subject_columns && !section.books.is_empty();
        let page = page.clone();
        let focus = focused_subject.clone();
        let id = id.to_owned();
        children.push(PaginatorChild::new(move |_, _, cx| {
            let theme = components::browser_theme(cx);
            let rows = column.iter().enumerate().map(|(row_index, subject)| {
                let index = column_index * components::SECTION_CARD_SUBJECT_ROWS_PER_COLUMN + row_index;
                let location = subject.id.clone();
                let open_page = page.clone();
                let menu_page = page.clone();
                let mobile_menu_page = page.clone();
                let menu_location = location.clone();
                let mobile_menu_location = location.clone();
                let mut row = components::section_card_subject(format!("{id}-subject-{index}"), child_icon, subject.name.clone(), subject.book_count, focus.get() == Some(index), theme).on_click(move |_, window, cx| {
                    let _ = open_page.update(cx, |page, cx| page.open_child(location.clone(), window, cx));
                }).on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    if !components::uses_mobile_navigation(window) {
                        let _ = menu_page.update(cx, |page, cx| page.open_section_child_menu(&menu_location, event.position, window, cx));
                    }
                    cx.stop_propagation();
                }).on_aux_click(move |event, window, cx| {
                    if event.is_secondary() {
                        if components::uses_mobile_navigation(window) {
                            let _ = mobile_menu_page.update(cx, |page, cx| page.open_section_child_menu(&mobile_menu_location, event.position(), window, cx));
                        }
                        cx.stop_propagation();
                    }
                });
                if let Some(folder) = folder
                    && let Ok(subfolder) = DirId::parse_str(&subject.id)
                {
                    let targets = FolderTargets { folders: vec![subfolder], folder_parents: Default::default(), books: Vec::new(), source: folder };
                    let preview = DragPreviewItem::Folder { icon: components::NavigationIcon::Folder, label: SharedString::from(subject.name.clone()), path: None };
                    row = folder_drop_target(folder_drag_source(row, targets, vec![preview], false), subfolder, SharedString::from(subject.name.clone()), true, page.clone(), theme);
                }
                row.into_any_element()
            }).collect();
            components::section_card_subject_rows(rows)
                .when(before_direct_books, |rows| rows.pr(gpui::px(components::SECTION_CARD_COLUMN_GAP)))
                .into_any_element()
        }));
    }
    for (index, book) in section.books.iter().enumerate() {
        let rendered = book.clone();
        let activated = book.clone();
        let prefetched = book.clone();
        children.push(PaginatorChild::new(move |_, _, cx| {
            // Identified because a drag source has to be a stateful element,
            // and each by its place: a drag's state is kept per id, so covers
            // sharing one would hand a press on one cover to another.
            let frame = div().id(("book", index)).w_full().h_full().min_w_0().child(rendered.clone());
            match folder {
                Some(folder) => {
                    let targets = FolderTargets { folders: Vec::new(), folder_parents: Default::default(), books: vec![rendered.read(cx).content_hash()], source: folder };
                    folder_drag_source(frame, targets, vec![DragPreviewItem::Book(rendered.clone())], false).into_any_element()
                }
                None => frame.into_any_element(),
            }
        })
            .with_activate(move |window, cx| BookCardView::activate(&activated, window, cx))
            .with_prepare(move |window, cx| BookCardView::prefetch_for_window(&prefetched, window, cx)));
    }
    vec![PaginatorGroup::new(browse_section_item_policy(), children)]
}
