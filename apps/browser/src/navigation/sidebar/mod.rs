//! Responsive navigation sidebar and compact navigation bar.
//!
//! Renders whichever route the shell says is current; it owns no route of its
//! own, only the open/closed state of its own overlays.

mod add_library;
#[cfg(feature = "google-drive")]
mod cloud_library;
#[cfg(not(feature = "google-drive"))]
#[path = "cloud_library_disabled.rs"]
mod cloud_library;
mod switcher_overlay;

use gpui::prelude::*;
use gpui::{Context, Edges, Entity, FocusHandle, Pixels, SharedString, Window};
use gpui_component::IconName;
use ui_components as components;

use super::{GlobalRoute, Libraries, LibraryRoute, Navigate, Route};

/// The rail's destinations, in the order it draws them. The keyboard walks this
/// same list, so a shortcut can never land somewhere other than the neighbour
/// the reader can see.
pub(crate) const RAIL_ROUTES: [Route; 7] = [
    Route::Library(LibraryRoute::Home),
    Route::Library(LibraryRoute::Books),
    Route::Library(LibraryRoute::Folders),
    Route::Library(LibraryRoute::Subjects),
    Route::Library(LibraryRoute::Authors),
    Route::Library(LibraryRoute::Catalog),
    Route::Global(GlobalRoute::Settings),
];

/// Where the keyboard is inside the open library switcher.
///
/// A grid rather than a list: rows are the add button and then one per library,
/// and a library row has two cells. Tab order alone could not express this —
/// `down` has to skip a row's menu button and land on the next library.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SwitcherCursor {
    Add,
    Library { index: usize, menu: bool },
}

pub(crate) struct Sidebar {
    libraries: Entity<Libraries>,
    switcher_open: bool,
    library_menu: Option<(sync_common::LibraryId, Entity<gpui_component::menu::PopupMenu>, gpui::Subscription)>,
    library_name_dialog: Option<switcher_overlay::LibraryNameDialog>,
    cloud_library_dialog: Option<cloud_library::CloudLibraryDialog>,
    add_library_choices: Option<add_library::AddLibraryChoices>,
    navigate: Navigate,
    pub(super) switcher_cursor: SwitcherCursor,
    pub(super) switcher_focus: FocusHandle,
}

impl gpui::EventEmitter<crate::PaneRequest> for Sidebar {}

pub(super) struct SplitPane;

impl gpui::EventEmitter<SplitPane> for Sidebar {}

impl Sidebar {
    pub(crate) fn new(libraries: Entity<Libraries>, navigate: Navigate, cx: &mut Context<Self>) -> Self {
        Self { libraries, library_menu: None, library_name_dialog: None, cloud_library_dialog: None, add_library_choices: None, switcher_open: false, navigate, switcher_cursor: SwitcherCursor::Add, switcher_focus: cx.focus_handle() }
    }

    /// Draws the rail or the bottom bar, whichever the window width calls for.
    /// `library_view` is the Library tab's view: the one last shown, which the
    /// tab returns to.
    pub(super) fn render_navigation(&self, emitter: Entity<Self>, route: Route, library_view: LibraryRoute, compact: bool, close_pane: bool, safe_area: Edges<Pixels>, window: &Window, cx: &gpui::App) -> gpui::AnyElement {
        if compact { self.render_bottom_navigation(emitter, route, library_view, safe_area, cx) } else { self.render_sidebar(emitter, route, close_pane, window, cx) }
    }

    /// The compact tab bar: the rail's destinations. On a phone the library's
    /// browse views share one Library tab instead, switched from the menu in
    /// their heading (`library_view_heading`).
    fn render_bottom_navigation(&self, emitter: Entity<Self>, route: Route, library_view: LibraryRoute, safe_area: Edges<Pixels>, cx: &gpui::App) -> gpui::AnyElement {
        let theme = components::browser_theme(cx);
        let in_library_view = matches!(route, Route::Library(view) if super::LIBRARY_VIEWS.contains(&view));
        let tabs = if components::touch_ui() { vec![Route::Library(LibraryRoute::Home), Route::Library(library_view), Route::Library(LibraryRoute::Catalog), Route::Global(GlobalRoute::Settings)] } else { RAIL_ROUTES.to_vec() };
        let mut navigation = components::bottom_navigation(theme);
        for target in tabs {
            let (id, icon) = target.presentation();
            let library_tab = components::touch_ui() && target == Route::Library(library_view);
            let (id, label, selected) = if library_tab { ("library", "Library", in_library_view) } else { (id, id, route == target) };
            let select = emitter.clone();
            navigation = navigation.child(components::bottom_navigation_item(format!("bottom-{id}"), components::navigation_icon(icon), label, selected, theme).on_click(move |_, window, cx| {
                select.update(cx, |navigation, cx| (navigation.navigate)(target, window, cx));
            }));
        }
        let library_name = self.active_library_name(cx);
        let navigation = navigation.child(components::bottom_navigation_item("bottom-libraries", components::navigation_icon(components::NavigationIcon::Books), library_name, false, theme).on_click(move |_, window, cx| {
            emitter.update(cx, |navigation, cx| navigation.open_library_switcher(window, cx));
        }));
        gpui::div().w_full().flex_none().flex().flex_col().pl(safe_area.left).pr(safe_area.right).bg(theme.page_bg).child(navigation).child(gpui::div().w_full().h(safe_area.bottom).flex_none()).into_any_element()
    }

    fn render_sidebar(&self, emitter: Entity<Self>, route: Route, close_pane: bool, window: &Window, cx: &gpui::App) -> gpui::AnyElement {
        let theme = components::browser_theme(cx);
        // The switcher and destinations share the same square tile geometry.
        let destination = |target: Route| {
            let (id, icon) = target.presentation();
            let label = SharedString::from(id);
            let select = emitter.clone();
            components::nav_rail_item(id, components::navigation_icon(icon), label, route == target, theme).on_click(move |_, window, cx| {
                select.update(cx, |navigation, cx| {
                    (navigation.navigate)(target, window, cx);
                });
            })
        };
        let mut destinations = components::nav_rail_destinations();
        for target in RAIL_ROUTES.into_iter().filter(|route| !matches!(route, Route::Global(GlobalRoute::Settings))) {
            destinations = destinations.child(destination(target));
        }
        let split_available = crate::split_panes_available(window);
        if split_available {
            let split = emitter.clone();
            destinations = destinations.child(components::nav_rail_item("split-pane", IconName::PanelLeft, "Split", false, theme).tooltip("Open split pane").on_click(move |_, _, cx| split.update(cx, |_, cx| cx.emit(SplitPane))));
        }
        let library_name = self.active_library_name(cx);
        if close_pane {
            let close = emitter.clone();
            destinations = destinations.child(components::nav_rail_item("close-pane", IconName::Close, "Close", false, theme).on_click(move |_, _, cx| close.update(cx, |_, cx| cx.emit(crate::PaneRequest::Close))));
        }
        let labels = RAIL_ROUTES.into_iter().map(|route| route.presentation().0).chain(std::iter::once(library_name.as_ref())).chain(split_available.then_some("Split")).chain(close_pane.then_some("Close"));
        let width = components::navigation_rail_width(labels, window, cx);
        let sidebar = components::sidebar(theme).w(width).flex_1().child(self.render_library_switcher_trigger(emitter.clone(), cx)).child(destinations);
        gpui::div().w(width).h_full().min_h_0().flex_none().flex().flex_col().bg(theme.page_bg).child(sidebar).child(destination(Route::Global(GlobalRoute::Settings))).into_any_element()
    }
}
