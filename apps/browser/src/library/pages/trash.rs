//! Library Trash. Deleted items remain recoverable here until explicitly purged.
//!
//! One list, newest first, folders and books together and grouped by how long
//! ago they were deleted: the question Trash is opened with is "what did I
//! just delete?". Items are selected to restore or delete several at once.

use ui_components::user_facing_error;

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Weak};

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, IntoElement, Render, RenderImage, SharedString, Stateful, Subscription, Task, Window, canvas, div, list, px};
use gpui_component::IconName;
use gpui_component::WindowExt as _;
use gpui_component::button::ButtonVariant;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::scroll::{Scrollbar, ScrollbarAxis};
use gpui_image_eviction::track_image;
use library_backend::{LibraryClient, ThumbnailResolution};
use library_model::{CardKey, LibraryTrashView, TrashedBook, TrashedFolder};
use sync_common::{BookId, DirId};
use ui_components as components;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::library::pages::with_retry_banner;
use crate::library::services::{LibraryContentsChanged, LibraryContext};
use crate::library::widgets::{CoverVisibility, CoversEvicted, generated_cover_variant, loaded_cover_from_jpeg};

mod list;
#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests;
use self::list::{TrashList, TrashRow};

const MAX_COVER_REQUESTS: usize = 4;
const DAY_MS: i64 = 86_400_000;

/// The groups the list is split into, each holding what was deleted less
/// than its age ago and not already in an earlier group. Ages rather than
/// calendar days: the page has no time zone to draw midnight with.
const GROUPS: [(&str, i64); 3] = [("Past day", DAY_MS), ("Past week", 7 * DAY_MS), ("Earlier", i64::MAX)];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum TrashKey {
    Folder(DirId),
    Book(BookId),
}

impl std::fmt::Display for TrashKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Folder(id) => write!(formatter, "folder-{id}"),
            Self::Book(content_hash) => write!(formatter, "book-{content_hash}"),
        }
    }
}

#[derive(Clone, Copy)]
enum TrashItem<'a> {
    Folder(&'a TrashedFolder),
    Book(&'a TrashedBook),
}

impl TrashItem<'_> {
    fn key(&self) -> TrashKey {
        match self {
            Self::Folder(folder) => TrashKey::Folder(folder.id),
            Self::Book(book) => TrashKey::Book(book.content_hash),
        }
    }

    fn deleted_at(&self) -> i64 {
        match self {
            Self::Folder(folder) => folder.deleted_at,
            Self::Book(book) => book.deleted_at,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Folder(folder) => &folder.name,
            Self::Book(book) => &book.title,
        }
    }

    /// Whether a lowercased search matches the name, or a book's author.
    fn matches(&self, query: &str) -> bool {
        let found = |text: &str| text.to_lowercase().contains(query);
        match self {
            Self::Folder(folder) => found(&folder.name),
            Self::Book(book) => found(&book.title) || found(&book.author),
        }
    }

    fn description(&self, now: i64) -> String {
        let ago = deleted_ago(self.deleted_at(), now);
        match self {
            Self::Folder(folder) => match &folder.parent_name {
                Some(parent) => format!("{parent} · {ago}"),
                None => ago,
            },
            Self::Book(book) => [book.author.as_str(), book.format.to_uppercase().as_str(), ago.as_str()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · "),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// How long ago, in the coarsest unit that still tells items apart.
fn deleted_ago(deleted_at: i64, now: i64) -> String {
    let minutes = (now - deleted_at).max(0) / 60_000;
    let days = minutes / (24 * 60);
    match minutes {
        0 => "deleted just now".to_owned(),
        1..60 => format!("deleted {minutes} min ago"),
        60..1440 => format!("deleted {} h ago", minutes / 60),
        _ if days == 1 => "deleted a day ago".to_owned(),
        _ if days < 60 => format!("deleted {days} days ago"),
        _ if days < 730 => format!("deleted {} months ago", days / 30),
        _ => format!("deleted {} years ago", days / 365),
    }
}

fn group_of(deleted_at: i64, now: i64) -> usize {
    let age = now - deleted_at;
    GROUPS.iter().position(|(_, limit)| age < *limit).unwrap_or(GROUPS.len() - 1)
}

fn count_label(items: impl IntoIterator<Item = TrashKey>) -> String {
    let (folders, books) = items.into_iter().fold((0, 0), |(folders, books), key| match key {
        TrashKey::Folder(_) => (folders + 1, books),
        TrashKey::Book(_) => (folders, books + 1),
    });
    let plural = |count: usize, one: &str, many: &str| format!("{count} {}", if count == 1 { one } else { many });
    match (folders, books) {
        (0, books) => plural(books, "book", "books"),
        (folders, 0) => plural(folders, "folder", "folders"),
        (folders, books) => format!("{} and {}", plural(folders, "folder", "folders"), plural(books, "book", "books")),
    }
}

pub(crate) struct TrashPage {
    ui: LibraryContext,
    contents: Option<LibraryTrashView>,
    error: Option<SharedString>,
    /// The load in flight. Replacing it drops the one before, which cancels it.
    request: Option<Task<()>>,
    selection: HashSet<TrashKey>,
    /// Lookups are weak; the shared image cache and visible frames own pixels.
    covers: HashMap<BookId, Option<Weak<RenderImage>>>,
    cover_visibility: CoverVisibility,
    cover_requests: HashMap<BookId, (u64, Task<()>)>,
    wanted_covers: HashSet<BookId>,
    cover_request_serial: u64,
    list: TrashList,
    /// Created on first render, since an input needs a window.
    search: Option<(Entity<InputState>, Subscription)>,
    focus: FocusHandle,
    _subscriptions: [Subscription; 3],
}

impl TrashPage {
    pub(crate) fn new(ui: LibraryContext, cx: &mut Context<Self>) -> Self {
        let subscriptions = [
            cx.subscribe(&ui.updates(), |page, _, _: &LibraryContentsChanged, cx| page.refresh(cx)),
            cx.observe_global::<CoversEvicted>(|page: &mut Self, cx| {
                if page.cover_visibility.is_visible() {
                    return;
                }
                page.reset_covers();
                cx.notify();
            }),
            cx.observe_global::<crate::stores::GeneratedCoversOnly>(|page: &mut Self, cx| {
                page.reset_covers();
                cx.notify();
            }),
        ];
        let mut page = Self {
            ui,
            contents: None,
            error: None,
            request: None,
            selection: HashSet::new(),
            covers: HashMap::new(),
            cover_visibility: CoverVisibility::default(),
            cover_requests: HashMap::new(),
            wanted_covers: HashSet::new(),
            cover_request_serial: 0,
            list: TrashList::default(),
            search: None,
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            _subscriptions: subscriptions,
        };
        page.refresh(cx);
        page
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let library = self.ui.backend().clone();
        let task = cx.spawn(async move |page, cx| {
            let result = library.trash().await;
            cx.wait_for_update_slot().await;
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(contents) => {
                        let present: HashSet<TrashKey> = contents.folders.iter().map(|folder| TrashKey::Folder(folder.id)).chain(contents.books.iter().map(|book| TrashKey::Book(book.content_hash))).collect();
                        page.selection.retain(|key| present.contains(key));
                        page.contents = Some(contents);
                        page.list.invalidate();
                        page.error = None;
                        // Cancel obsolete work even while cached offscreen.
                        // The next visible frame requests its own cover set.
                        page.reset_covers();
                    }
                    Err(error) => page.error = Some(user_facing_error(&error, "Could not load Trash. Please try again.").into()),
                }
                cx.notify();
            });
        });
        self.request = Some(task);
    }

    fn reset_covers(&mut self) {
        self.cover_requests.clear();
        self.covers.clear();
        self.wanted_covers.clear();
    }

    /// Called once after the visible list has painted. Layout callbacks only
    /// collect IDs, so they never update an entity while it is being rendered.
    fn request_visible_covers(&mut self, wanted: HashSet<BookId>, cx: &mut Context<Self>) {
        self.wanted_covers = wanted;
        self.covers.retain(|id, _| self.wanted_covers.contains(id));
        self.cover_requests.retain(|id, _| self.wanted_covers.contains(id));
        self.load_covers(cx);
    }

    fn load_covers(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<_> = self
            .wanted_covers
            .iter()
            .copied()
            .filter(|id| {
                !self.cover_requests.contains_key(id)
                    && match self.covers.get(id) {
                        Some(Some(image)) => image.strong_count() == 0,
                        Some(None) => false,
                        None => true,
                    }
            })
            .collect();
        if crate::stores::generated_covers_only(cx) {
            if !missing.is_empty() {
                self.covers.extend(missing.into_iter().map(|id| (id, None)));
                cx.notify();
            }
            return;
        }
        for content_hash in missing.into_iter().take(MAX_COVER_REQUESTS.saturating_sub(self.cover_requests.len())) {
            let library = self.ui.backend().clone();
            let decoder = cx.background_executor().clone();
            self.cover_request_serial = self.cover_request_serial.wrapping_add(1);
            let serial = self.cover_request_serial;
            let task = cx.spawn(async move |page, cx| {
                let cover = match library.thumbnail(content_hash, ThumbnailResolution::Browse).await {
                    Ok(Some(bytes)) => decoder.spawn(async move { loaded_cover_from_jpeg(bytes) }).await,
                    Ok(None) => None,
                    Err(error) => {
                        log::warn!("failed to load the thumbnail for trashed book {content_hash}: {error}");
                        None
                    }
                };
                cx.defer_update(page, move |page, cx| {
                    if !page.cover_requests.get(&content_hash).is_some_and(|(current, _)| *current == serial) {
                        return;
                    }
                    page.cover_requests.remove(&content_hash);
                    if page.wanted_covers.contains(&content_hash) {
                        let image = cover.map(|cover| {
                            track_image(&cover, cx);
                            Arc::downgrade(&cover)
                        });
                        page.covers.insert(content_hash, image);
                        cx.notify();
                    }
                    page.load_covers(cx);
                });
            });
            self.cover_requests.insert(content_hash, (serial, task));
        }
    }

    fn search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> &Entity<InputState> {
        let (search, _) = self.search.get_or_insert_with(|| {
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search Trash"));
            let subscription = cx.subscribe(&search, |page, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    page.list.invalidate();
                    cx.notify();
                }
            });
            (search, subscription)
        });
        search
    }

    /// Runs one Trash action. Success needs no reload here: the library
    /// reports its contents changed, and that refreshes this page.
    fn run<F: Future<Output = Result<(), library_backend::BackendError>> + 'static>(&mut self, operation: impl FnOnce(LibraryClient) -> F, action: &'static str, cx: &mut Context<Self>) {
        let operation = operation(self.ui.backend().clone());
        cx.spawn(async move |page, cx| {
            if let Err(error) = operation.await {
                cx.defer_update(page, move |page, cx| {
                    page.error = Some(format!("Could not {action}. {}", user_facing_error(&error, "Please try again.")).into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn restore(&mut self, items: Vec<TrashKey>, cx: &mut Context<Self>) {
        for item in items {
            self.selection.remove(&item);
            match item {
                TrashKey::Book(id) => self.run(move |library| async move { library.restore_book(id).await }, "restore book", cx),
                TrashKey::Folder(id) => self.run(move |library| async move { library.restore_directory(id, None).await.map(drop) }, "restore folder", cx),
            }
        }
        cx.notify();
    }

    fn purge(&mut self, items: Vec<TrashKey>, cx: &mut Context<Self>) {
        for item in items {
            self.selection.remove(&item);
            match item {
                TrashKey::Book(id) => self.run(move |library| async move { library.purge_book(id).await }, "delete book permanently", cx),
                TrashKey::Folder(id) => self.run(move |library| async move { library.purge_directory(id).await }, "delete folder permanently", cx),
            }
        }
        cx.notify();
    }

    fn empty(&mut self, cx: &mut Context<Self>) {
        self.selection.clear();
        self.run(|library| async move { library.empty_trash().await }, "empty Trash", cx);
        cx.notify();
    }

    /// Asks before deleting for good. `everything` empties Trash in one
    /// command rather than purging the items one by one.
    fn confirm_purge(&mut self, items: Vec<TrashKey>, everything: bool, window: &mut Window, cx: &mut Context<Self>) {
        if items.is_empty() {
            return;
        }
        let title: SharedString = match items.as_slice() {
            [item] => match self.item(*item) {
                Some(item) => format!("Delete “{}” permanently?", item.name()).into(),
                None => return,
            },
            _ => format!("Delete {} permanently?", count_label(items.iter().copied())).into(),
        };
        let page = cx.weak_entity();
        let items: Rc<[TrashKey]> = items.into();
        window.open_alert_dialog(cx, move |dialog, window, _| {
            let page = page.clone();
            let items = items.clone();
            components::confirmation_dialog(dialog, window).title(title.clone()).button_props(DialogButtonProps::default().ok_text("Delete permanently").ok_variant(ButtonVariant::Danger).cancel_text("Cancel").show_cancel(true)).on_ok(
                move |_, _, cx| {
                    let items = items.clone();
                    let _ = page.update(cx, |page, cx| if everything { page.empty(cx) } else { page.purge(items.to_vec(), cx) });
                    true
                },
            )
        });
    }

    fn item(&self, key: TrashKey) -> Option<TrashItem<'_>> {
        let contents = self.contents.as_ref()?;
        match key {
            TrashKey::Folder(id) => contents.folders.iter().find(|folder| folder.id == id).map(TrashItem::Folder),
            TrashKey::Book(id) => contents.books.iter().find(|book| book.content_hash == id).map(TrashItem::Book),
        }
    }

    fn ordered_keys(&self, selected_only: bool) -> Vec<TrashKey> {
        let Some(contents) = &self.contents else { return Vec::new() };
        let mut items: Vec<_> = contents
            .folders
            .iter()
            .map(|folder| (folder.deleted_at, TrashKey::Folder(folder.id)))
            .chain(contents.books.iter().map(|book| (book.deleted_at, TrashKey::Book(book.content_hash))))
            .filter(|(_, key)| !selected_only || self.selection.contains(key))
            .collect();
        items.sort_by_key(|(deleted, _)| Reverse(*deleted));
        items.into_iter().map(|(_, key)| key).collect()
    }

    fn all_keys(&self) -> Vec<TrashKey> {
        self.ordered_keys(false)
    }

    fn selected_keys(&self) -> Vec<TrashKey> {
        self.ordered_keys(true)
    }

    fn group_keys(&self, group: usize) -> Vec<TrashKey> {
        let Some(contents) = &self.contents else { return Vec::new() };
        self.list.rows.iter().skip_while(|row| **row != TrashRow::Group(group)).skip(1).take_while(|row| !matches!(row, TrashRow::Group(_))).filter_map(|row| row.item(contents).map(|item| item.key())).collect()
    }

    fn render_cover(&self, content_hash: BookId, theme: components::BrowserTheme) -> AnyElement {
        match self.covers.get(&content_hash) {
            Some(Some(cover)) => match cover.upgrade() {
                Some(image) => components::book_cover( components::BOOK_COVER_ASPECT_RATIO).child(components::book_cover_image(format!("trash-cover-{content_hash}"), image, 1.0, false, theme)).into_any_element(),
                None => components::book_cover( components::BOOK_COVER_ASPECT_RATIO).into_any_element(),
            },
            Some(None) => components::book_thumbnail_generated(generated_cover_variant(CardKey::from(content_hash))).into_any_element(),
            None => components::book_cover( components::BOOK_COVER_ASPECT_RATIO).into_any_element(),
        }
    }

    fn render_item(&self, item: TrashItem<'_>, now: i64, theme: components::BrowserTheme, target: &Entity<Self>) -> Stateful<Div> {
        let key = item.key();
        let selected = self.selection.contains(&key);
        let page = target.downgrade();
        let checkbox = components::selection_checkbox(format!("trash-select-{key}"), selected, theme).debug_selector(move || format!("trash-select-{key}")).on_click({
            let page = page.clone();
            move |_, _, cx| {
                let _ = page.update(cx, |page, cx| {
                    if !page.selection.remove(&key) {
                        page.selection.insert(key);
                    }
                    cx.notify();
                });
            }
        });
        let leading = match item {
            TrashItem::Folder(_) => components::trash_folder_thumbnail(theme),
            TrashItem::Book(book) => components::trash_book_thumbnail(self.render_cover(book.content_hash, theme)),
        };
        let restore = components::topbar_action_button(format!("trash-restore-{key}"), "Restore", IconName::Undo2, theme).debug_selector(move || format!("trash-restore-{key}")).on_click({
            let page = page.clone();
            move |_, _, cx| {
                let _ = page.update(cx, |page, cx| page.restore(vec![key], cx));
            }
        });
        let delete = components::delete_icon_button(format!("trash-delete-{key}"), "Delete permanently", theme).on_click(move |_, window, cx| {
            let _ = page.update(cx, |page, cx| page.confirm_purge(vec![key], false, window, cx));
        });
        components::trash_row(checkbox, leading, item.name().to_owned(), item.description(now), components::row(components::SPACE_SM).child(restore).child(delete), selected, theme)
            .id(format!("trash-row-{key}"))
            .debug_selector(move || format!("trash-row-{key}"))
    }

    fn render_group(&self, index: usize, theme: components::BrowserTheme, target: &Entity<Self>) -> Div {
        let all_selected = self.group_keys(index).iter().all(|key| self.selection.contains(key));
        let page = target.downgrade();
        let toggle = components::browser_settings_header_button(format!("trash-select-group-{index}"), if all_selected { "Clear selection" } else { "Select all" }, false, theme)
            .debug_selector(move || format!("trash-select-group-{index}"))
            .on_click(move |_, _, cx| {
                let _ = page.update(cx, |page, cx| {
                    for key in page.group_keys(index) {
                        if all_selected {
                            page.selection.remove(&key);
                        } else {
                            page.selection.insert(key);
                        }
                    }
                    cx.notify();
                });
            });
        components::browser_settings_fieldset_with_actions(GROUPS[index].0, toggle, theme).pt(px(if self.list.rows.first() == Some(&TrashRow::Group(index)) { 0.0 } else { components::SPACE_XL }))
    }

    fn render_selection_bar(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> Option<Div> {
        if self.selection.is_empty() {
            return None;
        }
        let page = cx.weak_entity();
        let clear = components::link_button("trash-clear-selection", "Clear", false).on_click({
            let page = page.clone();
            move |_, _, cx| {
                let _ = page.update(cx, |page, cx| {
                    page.selection.clear();
                    cx.notify();
                });
            }
        });
        let restore = components::topbar_action_button("trash-restore-selection", "Restore", IconName::Undo2, theme).on_click({
            let page = page.clone();
            move |_, _, cx| {
                let _ = page.update(cx, |page, cx| page.restore(page.selected_keys(), cx));
            }
        });
        let delete = components::topbar_danger_button("trash-delete-selection", "Delete permanently", IconName::Delete, theme).on_click(move |_, window, cx| {
            let _ = page.update(cx, |page, cx| page.confirm_purge(page.selected_keys(), false, window, cx));
        });
        Some(components::selection_bar(format!("{} selected", count_label(self.selection.iter().copied())), components::row(components::SPACE_SM).child(clear).child(restore).child(delete), theme))
    }

    fn render_list(&self, theme: components::BrowserTheme, now: i64, target: &Entity<Self>, window: &mut Window, cx: &mut App) -> AnyElement {
        let mounted_owner = target.downgrade();
        window.use_keyed_state(("trash-cover-work", target.entity_id()), cx, move |_, cx| {
            cx.on_release(move |_: &mut (), cx| {
                let _ = mounted_owner.update(cx, |page, _| {
                    if !page.cover_visibility.is_visible() {
                        page.reset_covers();
                    }
                });
            })
            .detach();
        });
        let owner = target.downgrade();
        let needed = Rc::new(RefCell::new(HashSet::new()));
        let row_needs = needed.clone();
        let rows = list(self.list.state.clone(), move |index, _, cx| {
            let Some(owner) = owner.upgrade() else { return div().into_any_element() };
            let page = owner.read(cx);
            match page.list.rows.get(index) {
                Some(TrashRow::Group(group)) => page.render_group(*group, theme, &owner).into_any_element(),
                Some(row) => match page.contents.as_ref().and_then(|contents| row.item(contents)) {
                    Some(item) => {
                        if let TrashKey::Book(id) = item.key() {
                            row_needs.borrow_mut().insert(id);
                        }
                        page.render_item(item, now, theme, &owner).into_any_element()
                    }
                    None => div().into_any_element(),
                },
                None => div().into_any_element(),
            }
        })
        .size_full()
        .py(px(components::SPACE_LG))
        .pr(px(components::SPACE_MD));
        let owner = target.downgrade();
        let state = self.list.state.clone();
        let previous_width = state.viewport_bounds().size.width;
        div()
            .relative()
            .size_full()
            .min_h_0()
            .max_w(px(640.0))
            .px(px(components::SPACE_MD))
            .child(rows)
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| {
                        if state.viewport_bounds().size.width != previous_width {
                            state.clone().with_uniform_item_height(px(80.0));
                            window.refresh();
                        }
                        let wanted = std::mem::take(&mut *needed.borrow_mut());
                        let owner = owner.clone();
                        cx.defer(move |cx| {
                            let _ = owner.update(cx, |page, cx| page.request_visible_covers(wanted, cx));
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(Scrollbar::new(&self.list.state).axis(ScrollbarAxis::Vertical))
            .into_any_element()
    }
}

impl Render for TrashPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.cover_visibility.track(window);
        let theme = components::browser_theme(cx);
        let now = now_ms();
        if let Some(contents) = &self.contents {
            let query = self.search.as_ref().map(|(search, _)| search.read(cx).value()).unwrap_or_default();
            self.list.update(contents, &query, now);
        }
        self.list.sync_typography(window);
        let has_items = self.contents.as_ref().is_some_and(|contents| !contents.books.is_empty() || !contents.folders.is_empty());
        let page = cx.weak_entity();
        let heading = if components::uses_mobile_navigation(window) {
            components::mobile_navigation_heading("Trash", crate::navigation::mobile_back(true), theme)
        } else {
            components::browser_topbar_left().child(div().text_xl().child("Trash"))
        };
        let search = has_items.then(|| components::browser_search_control(self.search_input(window, cx)));
        let empty_trash = has_items.then(|| {
            components::topbar_danger_button("empty-trash", "Empty Trash", IconName::Delete, theme).on_click(move |_, window, cx| {
                let _ = page.update(cx, |page, cx| page.confirm_purge(page.all_keys(), true, window, cx));
            })
        });
        let header = components::topbar(theme).child(heading).children(search).child(components::browser_topbar_right().children(empty_trash));
        let body = match self.contents.as_ref() {
            None => components::browser_settings_page().child(components::browser_loading_message("Loading Trash…", theme)).into_any_element(),
            Some(_) if !has_items => components::browser_settings_page().child(components::browser_actionable_empty("Trash is empty", div(), theme)).into_any_element(),
            Some(_) if self.list.rows.is_empty() => components::browser_settings_page().child(components::browser_settings_content().child(components::browser_inline_note("No matches", theme))).into_any_element(),
            Some(_) => div().size_full().min_h_0().flex().justify_center().child(self.render_list(theme, now, &cx.entity(), window, cx)).into_any_element(),
        };
        let page = cx.weak_entity();
        let body = with_retry_banner(body, self.error.clone(), "retry-section", theme, move |_, _, cx| {
            let _ = page.update(cx, |page, cx| page.refresh(cx));
        });
        components::library_section_content().track_focus(&self.focus).child(header).child(body).children(self.render_selection_bar(theme, cx)).into_any_element()
    }
}

impl Focusable for TrashPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
