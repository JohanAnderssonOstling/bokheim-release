//! The author index: every credited name in the library, filed under its
//! initial, each with a few of its covers beside it.
//!
//! Most authors in a library have one or two books. A full-width shelf per name
//! spent a screen on two or three of them and left most of every row empty, so
//! the index is a list of names instead, and an author's books open as ordinary
//! cards under their entry when it is chosen.
//!
//! The list is paged rather than scrolled, like every other page of the
//! library: one group per heading, and the open author's cards as a group of
//! their own directly after the entry that opened them.

use std::collections::HashMap;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent, Render, SharedString, Subscription, WeakEntity, Window, div};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::{Disableable, IconName};
use library_model::{AuthorSort, AuthorSummary};
use sync_common::ContentHash;
use ui_components as components;

use crate::library::pages::browse::controls::SearchField;
use crate::library::pages::library_empty_state;
use crate::library::services::{LatestRequest, LibraryContentsChanged, LibraryContext, LibraryScanChanged};
use crate::library::widgets::{BookCardView, CursorMove, Paginator, PaginatorAxis, PaginatorChild, PaginatorGroup, author_index_entry_policy, author_index_letter_sizing, book_card_policy};

/// The orders the reader may choose between, the first the default.
const SORTS: [AuthorSort; 2] = [AuthorSort::Name, AuthorSort::BookCount];

fn sort_label(sort: AuthorSort) -> &'static str {
    match sort {
        AuthorSort::Name => "Name",
        AuthorSort::BookCount => "Most books",
    }
}

fn names_label(count: usize) -> String {
    if count == 1 { "1 name".to_owned() } else { format!("{count} names") }
}

fn books_label(count: usize) -> String {
    if count == 1 { "1 book".to_owned() } else { format!("{count} books") }
}

/// What an author is filed under: their initial when the index is in name
/// order, and how many books they have when it is in count order — the heading
/// states whatever the order is changing on.
fn heading(author: &Author, sort: AuthorSort) -> SharedString {
    match sort {
        AuthorSort::Name => author.name.chars().next().filter(|initial| initial.is_alphabetic()).map_or_else(|| "#".to_owned(), |initial| initial.to_uppercase().collect()).into(),
        AuthorSort::BookCount if author.book_count >= 5 => "5 or more books".into(),
        AuthorSort::BookCount => books_label(author.book_count).into(),
    }
}

/// One name as the page holds it.
struct Author {
    id: SharedString,
    name: SharedString,
    /// Every book credited to the name. `books` is capped by the query, so this
    /// is the count to state.
    book_count: usize,
    /// The count and the subjects, as the line under the name reads.
    detail: SharedString,
    books: Vec<library_model::BookCardRow>,
    /// The first few books as cover-only cards, drawn beside the name.
    covers: Vec<Entity<BookCardView>>,
}

pub(crate) struct AuthorsPage {
    ui: LibraryContext,
    /// Every author the library returned, in the order it returned them. The
    /// search narrows what is drawn, not this.
    authors: Vec<Author>,
    sort: AuthorSort,
    search: SharedString,
    search_field: Option<SearchField>,
    search_open: bool,
    _search_subscription: Option<Subscription>,
    /// The author whose books are open under their entry, with those books as
    /// ordinary cards.
    open: Option<(SharedString, Vec<Entity<BookCardView>>)>,
    /// Each heading and the child it starts at, as last laid out: the rail
    /// jumps by these, and the open cards are found by `open_first_child`.
    sections: Vec<(SharedString, usize)>,
    open_first_child: Option<usize>,
    error: Option<SharedString>,
    refresh: LatestRequest,
    /// Whether the current load has resolved. No authors means "still loading"
    /// before the first result and "nothing to show" after it.
    loaded: bool,
    paginator: Entity<Paginator>,
    focus: FocusHandle,
    _updates: Vec<Subscription>,
}

impl AuthorsPage {
    pub(crate) fn new(ui: LibraryContext, cx: &mut Context<Self>) -> Self {
        let paginator = cx.new(|_| Paginator::new("authors", []));
        let updates = ui.updates();
        let update_subscriptions = vec![
            cx.subscribe(&updates, |page, _, _: &LibraryContentsChanged, cx| page.refresh(cx)),
            cx.subscribe(&updates, |_, _, _: &LibraryScanChanged, cx| cx.notify()),
            // The open cards reserve a title block or not with this setting, so
            // their group's row height has to be worked out again.
            cx.observe_global::<crate::stores::CoverTextSetting>(|page, cx| page.relayout(cx)),
        ];
        let mut page = Self {
            ui,
            authors: Vec::new(),
            sort: SORTS[0],
            search: SharedString::default(),
            search_field: None,
            search_open: false,
            _search_subscription: None,
            open: None,
            sections: Vec::new(),
            open_first_child: None,
            error: None,
            refresh: LatestRequest::default(),
            loaded: false,
            paginator,
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            _updates: update_subscriptions,
        };
        page.refresh(cx);
        page
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let generation = self.refresh.begin();
        let library = self.ui.backend().clone();
        let sort = self.sort;
        let task = cx.spawn(async move |page, cx| {
            let result = library.authors(sort).await;
            cx.defer_update(page, move |page, cx| {
                if !page.refresh.is_current(generation) {
                    return;
                }
                // Either outcome settles whether the page is still waiting: an
                // error surfaces its own banner and must not leave the body
                // claiming to be loading forever.
                page.loaded = true;
                match result {
                    Ok(authors) => {
                        page.error = None;
                        page.install(authors, cx);
                    }
                    Err(error) => {
                        log::error!("failed to load authors: {error}");
                        page.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        });
        self.refresh.install(task);
    }

    /// Replaces the authors and lays them out again.
    ///
    /// A reload is not a new page. A cover card fetched and faded in its cover
    /// once; rebuilding it for every library update would refetch every visible
    /// thumbnail and replay every fade. So a cover that still stands for the
    /// same book under the same name keeps its card, and the page stays where
    /// the reader had turned to.
    fn install(&mut self, authors: Vec<AuthorSummary>, cx: &mut Context<Self>) {
        let mut reusable = HashMap::<(SharedString, ContentHash), Entity<BookCardView>>::new();
        for author in self.authors.drain(..) {
            for card in author.covers {
                reusable.insert((author.id.clone(), card.read(cx).content_hash()), card);
            }
        }
        self.authors = authors
            .into_iter()
            .map(|author| {
                let id = SharedString::from(author.id.to_string());
                let covers = author
                    .books
                    .iter()
                    .take(components::AUTHOR_INDEX_COVERS)
                    .map(|book| match reusable.remove(&(id.clone(), book.content_hash)) {
                        Some(card) => {
                            card.update(cx, |card, _| card.replace_row(book.clone(), None));
                            card
                        }
                        None => {
                            let card = BookCardView::create(book.clone(), self.ui.clone(), cx);
                            card.update(cx, |card, cx| card.set_cover_only(true, cx));
                            card
                        }
                    })
                    .collect();
                // A leaf means little alone — "20th century" — so a subject is
                // stated with the parent it was reached through, as its chip is.
                let subjects = author.subjects.iter().map(|subject| match &subject.parent {
                    Some(parent) => format!("{parent} \u{203a} {}", subject.label),
                    None => subject.label.clone(),
                });
                let detail = std::iter::once(books_label(author.book_count)).chain(subjects).collect::<Vec<_>>().join(" · ");
                Author { id, name: author.name.as_str().to_owned().into(), book_count: author.book_count, detail: detail.into(), books: author.books, covers }
            })
            .collect();
        // An author who is no longer here has no books to hold open.
        if let Some((id, _)) = &self.open
            && !self.authors.iter().any(|author| &author.id == id)
        {
            self.open = None;
        }
        self.relayout(cx);
    }

    /// Lays the authors out again where the reader is. Opening an author or a
    /// library update changes what the page holds, not where the reader is in it.
    fn relayout(&mut self, cx: &mut Context<Self>) {
        let groups = self.groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.refresh_groups(groups, cx));
        self.sync_cursor(cx);
        cx.notify();
    }

    /// Lays the authors out from the first page: the search or the order has
    /// changed, so the page the reader was on no longer means anything.
    fn reset(&mut self, cx: &mut Context<Self>) {
        let groups = self.groups(cx);
        self.paginator.update(cx, |paginator, cx| paginator.set_groups(groups, cx));
        self.sync_cursor(cx);
        cx.notify();
    }

    /// Whether an author survives the search: on their own name, or on one of
    /// their books'. An empty query keeps everyone.
    fn matches(&self, author: &Author) -> bool {
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let matches = |text: &str| text.to_lowercase().contains(&query);
        matches(&author.name) || author.books.iter().any(|book| matches(&book.title))
    }

    /// One group per heading, split after the open author so that their cards
    /// follow their entry directly, and the rest of the heading continues below.
    fn groups(&mut self, cx: &mut Context<Self>) -> Vec<PaginatorGroup> {
        let page = cx.entity().downgrade();
        let cover_text = crate::stores::current_cover_text(cx);
        let authors = self.authors.iter().filter(|author| self.matches(author)).collect::<Vec<_>>();
        let mut groups = Vec::new();
        let mut sections = Vec::new();
        let mut open_first_child = None;
        let mut child = 0;
        let mut start = 0;
        while start < authors.len() {
            let title = heading(authors[start], self.sort);
            let end = start + authors[start..].iter().take_while(|author| heading(author, self.sort) == title).count();
            sections.push((title.clone(), child));
            let mut header = Some((title, names_label(end - start)));
            let mut entries = Vec::new();
            for author in &authors[start..end] {
                entries.push(entry_child(author, child, self.open.as_ref().is_some_and(|(id, _)| id == &author.id), page.clone()));
                child += 1;
                if let Some((id, cards)) = &self.open
                    && id == &author.id
                {
                    groups.push(entry_group(std::mem::take(&mut entries), header.take()));
                    groups.push(PaginatorGroup::new(book_card_policy(false, cover_text), cards.iter().map(card_child)));
                    open_first_child = Some(child);
                    child += cards.len();
                }
            }
            if !entries.is_empty() {
                groups.push(entry_group(entries, header));
            }
            start = end;
        }
        self.sections = sections;
        self.open_first_child = open_first_child;
        groups
    }

    /// Opens an author's books under their entry, or closes them if they are
    /// the ones already open. One author at a time: the index is for finding a
    /// name, and a page of open shelves is what it replaced.
    ///
    /// The cursor lands on the entry that was chosen. Closing an author's cards
    /// moves every child after them up by as many, so the page start moves with
    /// them — it stays on what the reader was looking at rather than on an index
    /// that now names something further down.
    fn toggle(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let closed = self.open.take().zip(self.open_first_child).map(|((open, cards), first)| (open, first, cards.len()));
        if closed.as_ref().is_none_or(|(open, _, _)| open != &id)
            && let Some(author) = self.authors.iter().find(|author| author.id == id)
        {
            let cards = author.books.iter().map(|book| BookCardView::create(book.clone(), self.ui.clone(), cx)).collect();
            self.open = Some((id, cards));
        }
        let groups = self.groups(cx);
        // An entry is the child just before its cards, and nothing before an
        // author's own cards moves when they close.
        let entry = self.open_first_child.or(closed.as_ref().map(|(_, first, _)| *first)).map(|first| first - 1);
        self.paginator.update(cx, |paginator, cx| {
            let anchor = paginator.child_anchor();
            let anchor = match closed {
                Some((_, first, count)) if anchor >= first + count => anchor - count,
                Some((_, first, _)) if anchor >= first => first - 1,
                _ => anchor,
            };
            paginator.refresh_groups(groups, cx);
            if let Some(entry) = entry {
                paginator.restore_cursor(entry, anchor, cx);
            }
        });
        self.sync_cursor(cx);
        cx.notify();
    }

    /// Turns the list to the page a heading starts on, with the cursor on its
    /// first name.
    fn jump(&mut self, child: usize, cx: &mut Context<Self>) {
        self.paginator.update(cx, |paginator, cx| paginator.restore_cursor(child, child, cx));
        self.sync_cursor(cx);
        cx.notify();
    }

    /// An entry reads the cursor from the paginator as it is drawn; a card is
    /// its own entity and has to be told.
    fn sync_cursor(&self, cx: &mut Context<Self>) {
        let (Some((_, cards)), Some(first)) = (&self.open, self.open_first_child) else { return };
        let cursor = self.paginator.read(cx).cursor();
        for (index, card) in cards.iter().enumerate() {
            card.update(cx, |card, cx| card.set_focused(cursor == Some(first + index), cx));
        }
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let Some(search_field) = &self.search_field else { return };
        let query = SharedString::from(search_field.query(cx));
        if self.search == query {
            return;
        }
        self.search = query;
        // The authors are already here, so the list narrows on the keystroke
        // rather than after a debounce that would only wait for itself.
        self.reset(cx);
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_open {
            self.search_open = false;
            self.focus.focus(window, cx);
        } else {
            if self.search_field.is_none() {
                let (search_field, subscription) = SearchField::new(Some("Search authors or books"), |page: &mut Self, cx| page.search_changed(cx), window, cx);
                self.search_field = Some(search_field);
                self._search_subscription = Some(subscription);
            }
            self.search_open = true;
            if let Some(search) = &self.search_field {
                search.focus(window, cx);
            }
        }
        cx.notify();
    }

    /// Choosing an order is a new question for the library, not a narrowing of
    /// the answer it already gave, so this reloads where the search does not.
    fn select_sort(&mut self, sort: AuthorSort, cx: &mut Context<Self>) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        self.open = None;
        self.authors.clear();
        self.reset(cx);
        self.refresh(cx);
    }

    /// The arrows walk the entries and any open cards in reading order, turning
    /// the page at its edges; Enter opens what the cursor is on.
    fn navigate(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers != gpui::Modifiers::default() {
            return;
        }
        // Escape leaves the search before anything else reads the key, and while
        // the field has focus the arrows belong to the text, not to the list.
        if self.search_open && event.keystroke.key == "escape" {
            self.toggle_search(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.search_field.as_ref().is_some_and(|search| search.is_focused(window, cx)) {
            return;
        }
        let paginator = self.paginator.clone();
        let step = |movement, cx: &mut Context<Self>| paginator.update(cx, |paginator, cx| paginator.move_cursor(movement, cx));
        let handled = match event.keystroke.key.as_str() {
            // Above the first name is the library the page belongs to, so the
            // cursor leaves upwards into the switcher rather than stopping.
            "up" => {
                step(CursorMove::Up, cx) || {
                    window.dispatch_action(Box::new(crate::OpenLibrarySwitcher), cx);
                    true
                }
            }
            "down" => step(CursorMove::Down, cx),
            "left" => step(CursorMove::Left, cx),
            "right" => step(CursorMove::Right, cx),
            "pageup" => self.paginator.update(cx, |paginator, cx| paginator.previous(cx)),
            "pagedown" => self.paginator.update(cx, |paginator, cx| paginator.next(cx)),
            // Activating navigates or relays the page, so it runs once the
            // paginator is no longer borrowed.
            "enter" => self.paginator.read(cx).cursor_activation().is_some_and(|activate| {
                window.defer(cx, move |window, cx| activate(window, cx));
                true
            }),
            "escape" => self.open.as_ref().map(|(id, _)| id.clone()).is_some_and(|id| {
                self.toggle(id, cx);
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

    fn render_sort_control(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.sort;
        let page = cx.entity();
        // An accent edge marks a non-default order as applied, matching how the
        // browse page marks its own filters. It is not the selected treatment:
        // this is a dropdown trigger, not a thing chosen in the toolbar.
        components::outlined_icon_button("authors-sort", "Sort", IconName::Settings2, theme)
            .when(selected != SORTS[0], |button| button.border_color(theme.text_accent).text_color(theme.text_accent))
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                let mut menu = menu.item(components::menu_section("Sort"));
                for sort in SORTS {
                    let page = page.clone();
                    menu = menu.item(components::menu_item(sort_label(sort), move |_, _, cx| page.update(cx, |page, cx| page.select_sort(sort, cx))).checked(sort == selected));
                }
                menu
            })
            .into_any_element()
    }

    fn render_topbar(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let paginator = self.paginator.read(cx);
        let (has_previous, has_next, can_previous, can_next) = (paginator.has_previous(), paginator.has_next(), paginator.can_previous(), paginator.can_next());
        let previous = has_previous.then(|| {
            components::outlined_icon_button("authors-previous", "Previous", IconName::ChevronUp, theme)
                .disabled(!can_previous)
                .on_click(cx.listener(|page, _, _, cx| {
                    page.paginator.update(cx, |paginator, cx| paginator.previous_with_axis(PaginatorAxis::Vertical, cx));
                }))
                .into_any_element()
        });
        let next = has_next.then(|| {
            components::outlined_icon_button("authors-next", "Next", IconName::ChevronDown, theme)
                .disabled(!can_next)
                .on_click(cx.listener(|page, _, _, cx| {
                    page.paginator.update(cx, |paginator, cx| paginator.next_with_axis(PaginatorAxis::Vertical, cx));
                }))
                .into_any_element()
        });
        let search_label = if self.search_open { "Hide search" } else { "Search authors or books" };
        let shown = self.authors.iter().filter(|author| self.matches(author)).count();
        let controls = components::browser_topbar_right()
            .when(self.search_open, |controls| controls.flex_1())
            .children(self.search_open.then(|| div().flex_1().min_w(gpui::rems(8.0)).child(self.search_field.as_ref().expect("search field exists while the search is open").render())))
            .child(
                components::outlined_icon_button("authors-search", search_label, IconName::Search, theme)
                    .when(self.search_open || !self.search.is_empty(), |button| button.bg(theme.accent).text_color(theme.accent_text))
                    .on_click(cx.listener(|page, _, window, cx| page.toggle_search(window, cx))),
            )
            .child(self.render_sort_control(theme, cx))
            .child(components::paginator_controls(previous, next));
        components::browser_topbar(theme)
            .child(if components::uses_mobile_navigation(window) {
                let back = (!components::has_native_back_button()).then(|| components::mobile_back_button(theme).on_click(|_, window, cx| window.dispatch_action(Box::new(crate::DismissSettings), cx)));
                components::mobile_navigation_heading("Authors", back)
            } else {
                components::browser_topbar_left().child(components::browser_page_title("Authors", names_label(shown), theme))
            })
            .child(controls)
            .into_any_element()
    }

    /// The letters beside the list. Only in name order — a count heading is not
    /// something to jump to — and not on a phone, where the list needs the width.
    fn render_rail(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.sort != AuthorSort::Name || components::uses_mobile_navigation(window) || self.sections.len() < 2 {
            return None;
        }
        let theme = components::browser_theme(cx);
        Some(
            components::author_index_rail()
                .children(self.sections.iter().map(|(letter, child)| {
                    let child = *child;
                    components::author_index_rail_letter(SharedString::from(format!("authors-rail-{letter}")), letter.clone(), theme).on_click(cx.listener(move |page, _, _, cx| page.jump(child, cx)))
                }))
                .into_any_element(),
        )
    }

    fn render_body(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        // Nothing to show is either a library that is still arriving or one that
        // holds no books. Saying nothing at all leaves a blank page in both.
        if self.authors.is_empty() && self.error.is_none() {
            return if self.ui.updates().read(cx).scanning() {
                components::browser_loading_message("Scanning library…", theme).into_any_element()
            } else if self.loaded && !self.ui.updates().read(cx).loading() {
                library_empty_state(theme)
            } else {
                components::browser_loading_message("Loading authors…", theme).into_any_element()
            };
        }
        let body = components::author_index_body().child(components::author_index_list().child(self.paginator.clone())).children(self.render_rail(window, cx)).into_any_element();
        if let Some(error) = self.error.clone() {
            let page = cx.entity();
            let retry = components::topbar_action_button("authors-retry", "Retry", IconName::Redo2, theme).on_click(move |_, _, cx| page.update(cx, |page, cx| page.refresh(cx)));
            components::browser_error_banner(error, retry, body).into_any_element()
        } else {
            body
        }
    }
}

/// One heading's entries, under the heading when they start it.
fn entry_group(entries: Vec<PaginatorChild>, header: Option<(SharedString, String)>) -> PaginatorGroup {
    let group = PaginatorGroup::new(author_index_entry_policy(), entries);
    let Some((title, count)) = header else { return group };
    group.with_header(author_index_letter_sizing(), move |_, cx| components::author_index_letter(title.clone(), count.clone(), components::browser_theme(cx)).into_any_element())
}

/// An author's entry. It holds the page weakly: the page owns the paginator
/// that owns this, and a strong handle back would keep all three alive.
fn entry_child(author: &Author, child: usize, open: bool, page: WeakEntity<AuthorsPage>) -> PaginatorChild {
    let id = author.id.clone();
    let name = author.name.clone();
    let detail = author.detail.clone();
    let covers = author.covers.clone();
    let prefetched = author.covers.clone();
    let more = author.book_count.saturating_sub(covers.len());
    let toggle = move |id: SharedString, page: &WeakEntity<AuthorsPage>, cx: &mut gpui::App| {
        let _ = page.update(cx, |page, cx| page.toggle(id, cx));
    };
    let activate_id = id.clone();
    let activate_page = page.clone();
    PaginatorChild::new(move |state, _, cx| {
        let theme = components::browser_theme(cx);
        let click_id = id.clone();
        let click_page = page.clone();
        components::author_index_entry(SharedString::from(format!("author-{id}")), open || state.cursor == Some(child), theme)
            .on_click(move |_, _, cx| toggle(click_id.clone(), &click_page, cx))
            .child(components::author_index_name(name.clone(), detail.clone(), theme))
            .child(components::author_index_covers().children(covers.iter().map(|cover| components::author_index_cover().child(cover.clone()))).when(more > 0, |covers| covers.child(components::author_index_more(format!("+{more}"), theme))))
            .into_any_element()
    })
    .with_activate(move |_, cx| toggle(activate_id.clone(), &activate_page, cx))
    .with_prepare(move |window, cx| {
        for cover in &prefetched {
            BookCardView::prefetch_for_window(cover, window, cx);
        }
    })
}

/// One of the open author's books, as the ordinary card it is everywhere else.
fn card_child(card: &Entity<BookCardView>) -> PaginatorChild {
    let card = card.clone();
    let prefetched = card.clone();
    let opened = card.clone();
    PaginatorChild::new(move |_, _, _| div().w_full().min_w_0().child(card.clone()).into_any_element())
        .with_activate(move |window, cx| BookCardView::activate(&opened, window, cx))
        .with_prepare(move |window, cx| BookCardView::prefetch_for_window(&prefetched, window, cx))
}

impl Render for AuthorsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let topbar = self.render_topbar(window, cx);
        let body = self.render_body(window, cx);
        components::full_size_column().track_focus(&self.focus).on_key_down(cx.listener(Self::navigate)).child(topbar).child(components::remaining_space_column().child(body))
    }
}

impl Focusable for AuthorsPage {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}
