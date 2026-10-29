//! Shared reader widgets used by EPUB and PDF readers.

use std::{collections::HashMap, rc::Rc};

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Window, div, px};
use gpui_component::list::ListItem;
use gpui_component::tree::{TreeState, tree};
use skrift_column_paginator::SearchOptions;
use library_backend::Annotation;
use ui_components as components;

use crate::settings::ReaderSettings;
pub(crate) use crate::settings::SidebarTab;
use crate::shell::search::highlighted_text;

/// Shared sidebar tab-selection state.
///
/// The sidebar itself is always visible on desktop while chrome is shown, so
/// `active` is normally `Some`. It only goes `None` on the mobile bottom-bar
/// layout, where tapping the already-open tab's icon closes its sheet while
/// the bar itself stays put. Visibility is restored from preferences; mobile
/// and Kobo readers default to a closed sidebar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SidebarState {
    active: Option<SidebarTab>,
    last_tab: SidebarTab,
}

impl SidebarState {
    pub(crate) fn from_preferences(cx: &App) -> Self {
        let preferences = ReaderSettings::preferences(cx);
        let tab = preferences.active_sidebar_tab;
        Self { active: preferences.sidebar_open.then_some(tab), last_tab: tab }
    }

    #[cfg(test)]
    pub(crate) fn active(self) -> Option<SidebarTab> {
        self.active
    }

    /// The tab that should actually be drawn: falls back to Notes when the
    /// stored/selected tab is Contents but no outline exists.
    pub(crate) fn effective(self, contents_available: bool) -> Option<SidebarTab> {
        match self.active {
            Some(SidebarTab::Contents) if !contents_available => Some(SidebarTab::Notes),
            other => other,
        }
    }

    /// [`Self::effective`], defaulting to a sensible tab instead of `None`.
    ///
    /// A persistent desktop split always shows some tab — there is no closed
    /// state to fall back to once the toolbar's toggle button is gone — so
    /// this is what the split render path reaches for instead of `effective`.
    pub(crate) fn effective_or_default(self, contents_available: bool) -> SidebarTab {
        match self.active.unwrap_or(self.last_tab) {
            SidebarTab::Contents if !contents_available => SidebarTab::Notes,
            tab => tab,
        }
    }

    pub(crate) fn shows(self, tab: SidebarTab) -> bool {
        self.active == Some(tab)
    }

    pub(crate) fn is_visible(self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn show(&mut self, tab: SidebarTab) -> bool {
        if self.active == Some(tab) {
            return false;
        }
        self.active = Some(tab);
        self.last_tab = tab;
        true
    }

    /// Reopens the last selected tab and saves its visibility.
    pub(crate) fn reopen(&mut self, cx: &mut App) -> bool {
        let state_changed = self.show(self.last_tab);
        let preference_changed = ReaderSettings::update(cx, |preferences| preferences.sidebar_open = true);
        state_changed || preference_changed
    }

    pub(crate) fn close(&mut self, cx: &mut App) -> bool {
        let state_changed = self.active.is_some();
        self.active = None;
        let preference_changed = ReaderSettings::update(cx, |preferences| preferences.sidebar_open = false);
        state_changed || preference_changed
    }
}

/// Selects a sidebar tab and persists it as the tab to reopen next session.
pub(crate) fn select_tab<C: 'static>(sidebar: &mut SidebarState, tab: SidebarTab, cx: &mut Context<C>) -> bool {
    let state_changed = sidebar.show(tab);
    let preference_changed = ReaderSettings::update(cx, |preferences| {
        preferences.active_sidebar_tab = tab;
        preferences.sidebar_open = true;
    });
    state_changed || preference_changed
}

/// The phone bottom bar's self-toggling policy: tapping the action whose sheet
/// is open closes it; tapping any other opens its sheet. Contents falls back
/// to Notes when there is no outline, the same fallback
/// `SidebarState::effective` applies on desktop.
pub(crate) fn toggle_tab<C: 'static>(sidebar: &mut SidebarState, tab: SidebarTab, contents_available: bool, cx: &mut Context<C>) -> bool {
    let tab = if tab == SidebarTab::Contents && !contents_available { SidebarTab::Notes } else { tab };
    if sidebar.shows(tab) { sidebar.close(cx) } else { select_tab(sidebar, tab, cx) }
}

/// The panels in rail order, without Contents for a book with no outline —
/// it is omitted rather than shown disabled, as a PDF without bookmarks never
/// had contents to show.
pub(crate) fn sidebar_tabs(contents_available: bool) -> &'static [SidebarTab] {
    if contents_available { &[SidebarTab::Contents, SidebarTab::Notes, SidebarTab::Search, SidebarTab::Settings] } else { &[SidebarTab::Notes, SidebarTab::Search, SidebarTab::Settings] }
}

/// Cycles only through panels available in this book, wrapping at either end.
pub(crate) fn adjacent_sidebar_tab(active: SidebarTab, contents_available: bool, direction: i32) -> SidebarTab {
    adjacent_available_tab(active, sidebar_tabs(contents_available), direction)
}

pub(crate) fn adjacent_available_tab(active: SidebarTab, tabs: &[SidebarTab], direction: i32) -> SidebarTab {
    if tabs.is_empty() {
        return active;
    }
    let index = tabs.iter().position(|tab| *tab == active).unwrap_or(0) as i32;
    tabs[(index + direction).rem_euclid(tabs.len() as i32) as usize]
}

pub(crate) trait AnnotationSidebarTarget: Sized + 'static {
    fn select_sidebar_annotation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>);
    fn go_to_annotation(&mut self, annotation: &Annotation, cx: &mut Context<Self>);
    fn delete_sidebar_annotation(&mut self, id: String, cx: &mut Context<Self>);
}

/// Compact descriptions shared by the flat PDF list and EPUB chapter tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AnnotationRow {
    Heading(String),
    Card { index: usize, depth: usize, id: SharedString, modified_at: u64 },
    Empty(&'static str),
}

impl AnnotationRow {
    pub(crate) fn card(index: usize, depth: usize, annotation: &Annotation) -> Self {
        Self::Card { index, depth, id: annotation.id.clone().into(), modified_at: annotation.modified_at }
    }

    pub(crate) fn annotation_id(&self) -> Option<&str> {
        match self {
            Self::Card { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// Group indices without copying quotes, notes or annotation anchors.
pub(crate) fn reader_state_rows<H>(annotations: &[Annotation], no_annotations: &'static str, group_for_annotation: H) -> Vec<AnnotationRow>
where
    H: Fn(&Annotation) -> (String, String),
{
    if annotations.is_empty() {
        return vec![AnnotationRow::Empty(no_annotations)];
    }
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut group_indices = HashMap::<String, usize>::new();
    for (index, annotation) in annotations.iter().enumerate() {
        let (key, title) = group_for_annotation(annotation);
        let group_index = *group_indices.entry(key).or_insert_with(|| {
            groups.push((title, Vec::new()));
            groups.len() - 1
        });
        groups[group_index].1.push(index);
    }
    let mut rows = Vec::new();
    for (title, indices) in groups {
        rows.push(AnnotationRow::Heading(title));
        for index in indices {
            rows.push(AnnotationRow::card(index, 0, &annotations[index]));
        }
    }
    rows
}

pub(crate) fn render_annotation_row<C: AnnotationSidebarTarget>(target: &Entity<C>, row: &AnnotationRow, annotations: &[Annotation], selected_id: Option<&str>, query: &str, theme: components::BrowserTheme) -> AnyElement {
    match row {
        AnnotationRow::Heading(title) => components::reader_annotation_group_heading(title.clone(), theme).into_any_element(),
        AnnotationRow::Empty(message) => components::reader_state_note(*message, theme).p(px(12.0)).into_any_element(),
        AnnotationRow::Card { index, depth, id, .. } => {
            let Some(annotation) = annotations.get(*index).filter(|annotation| annotation.id == id.as_str()) else { return div().into_any_element() };
            div()
                .w_full()
                .min_w_0()
                .flex_none()
                .pl(px(components::CONTENTS_ROW_INDENT * (*depth).min(3) as f32))
                .child(reader_annotation_card(target, *index, annotation, selected_id == Some(annotation.id.as_str()), query, theme))
                .into_any_element()
        }
    }
}

/// One annotation, shared by flat lists and the filtered TOC tree.
pub(crate) fn reader_annotation_card<C: AnnotationSidebarTarget>(target: &Entity<C>, index: usize, annotation: &Annotation, selected: bool, query: &str, theme: components::BrowserTheme) -> AnyElement {
    let note = annotation.note.clone();
    let options = SearchOptions { match_diacritics: true, ..Default::default() };
    let editable = !annotation.is_external_pdf_annotation();
    let select_target = target.clone();
    let select_id = annotation.id.clone();
    let go_target = target.clone();
    let go_annotation = annotation.clone();
    let keyboard_target = target.clone();
    let keyboard_annotation = annotation.clone();
    let actions = if editable {
        let delete_target = target.clone();
        let delete_id = annotation.id.clone();
        Some(components::delete_icon_button(("reader-state-annotation-delete", index), "Delete annotation", theme).debug_selector(move || format!("reader-annotation-delete-{index}")).on_click(move |_, _, cx| {
            cx.stop_propagation();
            delete_target.update(cx, |this, cx| this.delete_sidebar_annotation(delete_id.clone(), cx));
        }))
    } else {
        None
    };
    components::reader_state_card(("reader-state-annotation", index), selected, theme)
        .debug_selector(move || format!("reader-annotation-card-{index}"))
        .focusable()
        .tab_stop(true)
        .on_key_down(move |event, window, cx| {
            if event.keystroke.modifiers == gpui::Modifiers::default() && matches!(event.keystroke.key.as_str(), "enter" | "space") {
                keyboard_target.update(cx, |this, cx| {
                    this.select_sidebar_annotation(keyboard_annotation.id.clone(), window, cx);
                    this.go_to_annotation(&keyboard_annotation, cx);
                });
                cx.stop_propagation();
            }
        })
        .on_click(move |_, window, cx| {
            let id = select_id.clone();
            select_target.update(cx, |this, cx| this.select_sidebar_annotation(id, window, cx));
            // Clicking the annotation goes to its passage; there is no Go button.
            let annotation = go_annotation.clone();
            go_target.update(cx, |this, cx| this.go_to_annotation(&annotation, cx));
        })
        .child(components::reader_annotation_color_bar(annotation.color))
        .when(!annotation.exact_text.is_empty(), |card| card.child(components::reader_state_quote(highlighted_text(format!("“{}”", annotation.exact_text), query, options), theme)))
        .when(!note.is_empty(), |card| card.child(components::reader_state_note(highlighted_text(note, query, options), theme).text_color(theme.text)))
        .children(actions.map(|actions| {
            components::action_row().id(("reader-state-annotation-actions", index)).justify_end().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(|_, _, cx| cx.stop_propagation()).child(actions)
        }))
        .into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TocActivation {
    Pointer,
    Keyboard,
}

pub(crate) type TocReadReset = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The table-of-contents tree. Keyboard activation keeps focus in the tree so
/// the reader can continue navigating entries with the arrow keys.
pub(crate) fn reader_toc_tree<C, F>(
    toc_tree: &Entity<TreeState>, target: &Entity<C>, active_id: Option<&str>, read_ids: std::collections::HashSet<String>, reset_read: Option<TocReadReset>, keyboard_visible: bool, theme: components::BrowserTheme, cx: &gpui::App,
    on_navigate: F,
) -> AnyElement
where
    C: 'static,
    F: Fn(&str, TocActivation, &mut Window, &mut C, &mut Context<C>) + 'static,
{
    let target = target.clone();
    let on_navigate = Rc::new(on_navigate);
    let active_id = active_id.map(str::to_owned);
    // `selected` is the entry the reader is inside: the renderer resolves the
    // position to a link and the sidebar selects it. It used to be discarded,
    // so the list gave no sign of where you were and the only indication was
    // the chapter name in the toolbar.
    // Every entry is numbered in reading order, those inside a closed part
    // counted too, so a number never changes when a part is opened. The caret
    // column is kept only in a contents that has parts at all.
    let (numbers, number_digits, nested) = {
        fn descendants(item: &gpui_component::tree::TreeItem) -> usize {
            item.children.iter().map(|child| 1 + descendants(child)).sum()
        }
        let entries = toc_tree.read(cx).entries();
        let mut next = 0;
        let numbers = entries
            .iter()
            .map(|entry| {
                next += 1;
                let number = next;
                if entry.is_folder() && !entry.is_expanded() {
                    next += descendants(entry.item());
                }
                number
            })
            .collect::<Vec<_>>();
        (numbers, components::contents_digits(next), entries.iter().any(|entry| entry.is_folder()))
    };
    let keyboard_tree = toc_tree.clone();
    let keyboard_target = target.clone();
    let keyboard_navigate = on_navigate.clone();
    gpui::div()
        .size_full()
        .on_key_down(move |event, window, cx| {
            if event.keystroke.key == "enter" && !event.keystroke.modifiers.control && !event.keystroke.modifiers.alt && !event.keystroke.modifiers.platform {
                let id = keyboard_tree.read(cx).selected_item().filter(|item| !item.is_disabled()).map(|item| item.id.to_string());
                if let Some(id) = id {
                    keyboard_target.update(cx, |this, cx| keyboard_navigate(&id, TocActivation::Keyboard, window, this, cx));
                    keyboard_tree.update(cx, |tree, cx| tree.focus(window, cx));
                    cx.stop_propagation();
                }
            }
        })
        .child(
            tree(toc_tree, move |index, entry, selected, _window, _cx| {
                let item = entry.item();
                let id = item.id.to_string();
                let navigation_target = target.clone();
                let on_navigate = on_navigate.clone();
                let marked = active_id.as_deref() == Some(item.id.as_ref());
                let row = components::ContentsRow {
                    depth: entry.depth(),
                    has_children: entry.is_folder(),
                    expanded: entry.is_expanded(),
                    marked,
                    read: read_ids.contains(item.id.as_ref()),
                    focused: keyboard_visible && selected,
                    number: numbers.get(index).copied(),
                    number_digits,
                };
                let reset = reset_read.clone().map(|reset| {
                    let id = id.clone();
                    Rc::new(move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| reset(&id, window, cx)) as components::ContentsReadReset
                });
                // The tree toggles an entry when its row is clicked, so navigation
                // hangs on the title, which stops the click reaching the row.
                let title = components::contents_title(("reader-toc-title", index), item.label.clone()).debug_selector(move || format!("reader-toc-title-{index}")).on_click(move |_, window, cx| {
                    let id = id.clone();
                    navigation_target.update(cx, |this, cx| on_navigate(&id, TocActivation::Pointer, window, this, cx));
                });
                // The tree insists on a `ListItem`, which is where its own click and
                // keyboard handling attach; the row inside it is the shared one, so a
                // sidebar entry and a chapter in the player are the same drawing.
                // Rows are content-sized, so a two-line title keeps the row's own
                // padding above and below it.
                ListItem::new(("reader-toc-item", index)).w_full().min_w_0().p_0().child(
                    components::contents_row(("reader-toc-row", index), row, nested.then(|| components::contents_disclosure(("reader-toc-disclosure", index), row, theme)), title, None, reset, theme)
                        .debug_selector(move || format!("reader-toc-row-{index}"))
                        .min_h_0()
                        .line_height_auto(),
                )
            })
            .pr(gpui::px(components::READER_SCROLLBAR_GUTTER_PX)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::prelude::*;
    use gpui::{Context, Entity, Render, ScrollStrategy, TestAppContext, Window, div, px};
    use gpui_component::tree::{TreeItem, TreeState};

    use crate::settings::{ReaderPreferences, ReaderSettings};

    use super::{SidebarState, SidebarTab, adjacent_sidebar_tab, reader_toc_tree};

    struct TocSpacingTest {
        tree: Entity<TreeState>,
    }

    impl Render for TocSpacingTest {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().debug_selector(|| "test-toc-viewport".into()).w(px(280.0)).h(px(240.0)).child(reader_toc_tree(&self.tree, &cx.entity(), None, Default::default(), None, true, ui_components::browser_theme(cx), cx, |_, _, _, _, _| {}))
        }
    }

    #[gpui::test]
    fn wrapped_toc_titles_keep_the_same_vertical_padding(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|_, cx| TocSpacingTest {
            tree: cx.new(|cx| TreeState::new(cx).items(vec![TreeItem::new("short", "Short title"), TreeItem::new("wrapped", "A chapter title long enough to wrap onto a second line in the sidebar"), TreeItem::new("last", "Next chapter")])),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let short_row = cx.debug_bounds("reader-toc-row-0").unwrap();
        let short_title = cx.debug_bounds("reader-toc-title-0").unwrap();
        let wrapped_row = cx.debug_bounds("reader-toc-row-1").unwrap();
        let wrapped_title = cx.debug_bounds("reader-toc-title-1").unwrap();
        let last_row = cx.debug_bounds("reader-toc-row-2").unwrap();
        assert!(wrapped_title.size.height > short_title.size.height);
        assert!(wrapped_row.size.height > short_row.size.height);
        let top_difference = (short_title.top() - short_row.top()) - (wrapped_title.top() - wrapped_row.top());
        let bottom_difference = (short_row.bottom() - short_title.bottom()) - (wrapped_row.bottom() - wrapped_title.bottom());
        assert!(f32::from(top_difference).abs() < 0.01);
        assert!(f32::from(bottom_difference).abs() < 0.01);
        assert!(wrapped_row.top() >= short_row.bottom());
        assert!(last_row.top() >= wrapped_row.bottom());
    }

    #[gpui::test]
    fn mixed_height_toc_rows_scroll_to_the_requested_entry(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (view, cx) = cx.add_window_view(|_, cx| TocSpacingTest {
            tree: cx.new(|cx| {
                TreeState::new(cx).items(
                    (0..60)
                        .map(|index| {
                            let title = if index % 2 == 0 { "Short title" } else { "A chapter title long enough to wrap onto a second line in the sidebar" };
                            TreeItem::new(format!("entry-{index}"), title)
                        })
                        .collect::<Vec<_>>(),
                )
            }),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        view.update(cx, |view, cx| {
            view.tree.update(cx, |tree, cx| {
                tree.reveal_item(&"entry-31".into(), ScrollStrategy::Center, cx);
                cx.notify();
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let viewport = cx.debug_bounds("test-toc-viewport").unwrap();
        let target = cx.debug_bounds("reader-toc-row-31").unwrap();
        assert!(target.top() >= viewport.top());
        assert!(target.bottom() <= viewport.bottom());
        assert!(f32::from(target.center().y - viewport.center().y).abs() < 1.0);
        view.update(cx, |view, cx| {
            view.tree.update(cx, |tree, cx| {
                tree.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(cx.debug_bounds("reader-toc-row-0").unwrap().top(), viewport.top());
    }

    #[test]
    fn tab_navigation_wraps_and_skips_unavailable_contents() {
        let last = SidebarTab::Settings;
        assert_eq!(adjacent_sidebar_tab(last, true, 1), SidebarTab::Contents);
        assert_eq!(adjacent_sidebar_tab(SidebarTab::Contents, true, -1), last);
        assert_eq!(adjacent_sidebar_tab(last, false, 1), SidebarTab::Notes);
        assert_eq!(adjacent_sidebar_tab(SidebarTab::Notes, false, -1), last);
        assert_eq!(adjacent_sidebar_tab(SidebarTab::Notes, true, 1), SidebarTab::Search);
    }

    #[gpui::test]
    fn closing_and_reopening_restores_saved_visibility_and_tab(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for tab in [SidebarTab::Contents, SidebarTab::Notes, SidebarTab::Search, SidebarTab::Settings] {
                let preferences = ReaderPreferences { active_sidebar_tab: tab, sidebar_open: true, ..Default::default() };
                let saved = Rc::new(RefCell::new(preferences.clone()));
                let save_target = saved.clone();
                let save: crate::settings::SaveReaderSettings = Rc::new(move |preferences, _, _| *save_target.borrow_mut() = preferences);
                cx.set_global(ReaderSettings { preferences, save: save.clone() });
                let mut sidebar = SidebarState::from_preferences(cx);
                assert!(sidebar.close(cx));
                assert_eq!(sidebar.active(), None);
                assert_eq!(sidebar.effective_or_default(true), tab);
                assert!(!saved.borrow().sidebar_open);

                // Reinstall saved preferences as on application startup, then
                // construct a fresh reader's sidebar.
                cx.set_global(ReaderSettings { preferences: saved.borrow().clone(), save: save.clone() });
                let mut sidebar = SidebarState::from_preferences(cx);
                assert_eq!(sidebar.active(), None);
                assert!(sidebar.reopen(cx));
                assert!(saved.borrow().sidebar_open);
                assert_eq!(SidebarState::from_preferences(cx).active(), Some(tab));
                assert!(!sidebar.reopen(cx));
            }
        });
    }

    #[gpui::test]
    fn missing_contents_falls_back_without_forgetting_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(ReaderSettings { preferences: ReaderPreferences::default(), save: Rc::new(|_, _, _| {}) });
            let mut sidebar = SidebarState::default();
            sidebar.show(SidebarTab::Contents);
            sidebar.close(cx);
            assert_eq!(sidebar.effective_or_default(false), SidebarTab::Notes);
            sidebar.reopen(cx);
            assert_eq!(sidebar.effective(false), Some(SidebarTab::Notes));
            assert_eq!(sidebar.effective(true), Some(SidebarTab::Contents));
        });
    }
}
