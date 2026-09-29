//! Browser shell and route coordinator.
//!
//! This shell owns the current route. Library sessions own library lifecycle
//! and per-library page state, and global pages own Settings.
//! sidebar draws whichever route it is handed.

use app_preferences::LibraryView;
use gpui::prelude::*;
use gpui::{App, AppContext, Context, Entity, IntoElement, Render, Subscription, Window};
use sync_common::LibraryId;
#[cfg(not(target_arch = "wasm32"))]
use sync_common::ROOT_DIR_ID;
use ui_components as components;

use super::sidebar::Sidebar;
use super::{GlobalPages, Libraries, LibrarySummary, Navigate, Route};
use crate::library::{BookDetailClosed, OpenBook};
use crate::services::AppServices;
use crate::stores::Preferences;
use crate::universal::sync::SyncPage;

pub struct BrowserShell {
    #[cfg(not(target_arch = "wasm32"))]
    services: AppServices,
    #[cfg(not(target_arch = "wasm32"))]
    open_book: OpenBook,
    route: Route,
    content_footer: Option<gpui::AnyView>,
    pending_folder: Option<(LibraryId, sync_common::DirId, String)>,
    pending_subject: Option<(LibraryId, String, String)>,
    libraries: Entity<Libraries>,
    global_pages: GlobalPages,
    navigation: Entity<Sidebar>,
    _library_subscription: Subscription,
    _navigation_subscription: Subscription,
}

impl BrowserShell {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn apply_sync_event(&mut self, event: crate::SyncEvent, cx: &mut Context<Self>) {
        self.global_pages.apply_sync_event(event, cx);
    }
    pub(crate) fn set_content_footer(&mut self, footer: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        if self.content_footer.as_ref().map(|view| view.entity_id()) == footer.as_ref().map(|view| view.entity_id()) {
            return;
        }
        self.content_footer = footer;
        cx.notify();
    }

    pub(crate) fn open_book_detail(&mut self, library_id: LibraryId, content_hash: sync_common::ContentHash, window: &mut Window, cx: &mut Context<Self>) {
        self.libraries.update(cx, |libraries, cx| libraries.select_library(library_id, cx));
        let Some(session) = self.libraries.read(cx).selected_session() else { return };
        session.update(cx, |session, cx| session.open_book_detail(content_hash, window, cx));
    }

    pub(crate) fn clear_cached_book_details(&mut self, cx: &mut Context<Self>) {
        self.libraries.update(cx, |libraries, cx| libraries.clear_cached_book_details(cx));
    }
    pub(crate) fn set_library_view(&mut self, view: LibraryView, cx: &mut Context<Self>) {
        self.libraries.update(cx, |libraries, cx| libraries.set_library_view(view, cx));
    }
    /// Builds the browser around the restored session and starts background subscriptions.
    pub fn new(services: AppServices, sync_state: Entity<crate::SyncState>, open_book: OpenBook, book_detail_closed: BookDetailClosed, preferences: Entity<Preferences>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let libraries = services.startup.libraries.iter().map(|entry| LibrarySummary::from_entry(&entry)).collect::<Vec<_>>();
        let active_library_id = libraries.first().map(|entry| *entry.library_id());
        let shell = cx.entity().downgrade();
        let navigate_callback: Navigate = std::rc::Rc::new(move |route, window, cx| {
            let _ = shell.update(cx, |shell, cx| shell.navigate(route, window, cx));
        });
        let libraries = cx.new(|cx| Libraries::new(services.backend.clone(), open_book.clone(), book_detail_closed, libraries, active_library_id, navigate_callback.clone(), cx));
        let session = libraries.read(cx).selected_session();
        let route = if let Some(session) = session {
            session.read(cx).focus_handle(cx).focus(window, cx);
            Route::Library(session.read(cx).route())
        } else {
            Route::Library(super::LibraryRoute::Home)
        };
        let navigation = cx.new(|cx| Sidebar::new(libraries.clone(), navigate_callback, cx));
        // Sidebar elements are rendered by this shell rather than as an entity
        // child. Forward its state changes so menus and dialogs are repainted.
        let navigation_subscription = cx.observe(&navigation, |_, _, cx| cx.notify());
        let library_subscription = cx.observe_in(&libraries, window, |shell, libraries, window, cx| {
            shell.open_pending_folder(window, cx);
            shell.open_pending_subject(window, cx);
            let session = libraries.read(cx).selected_session();
            if matches!(shell.route, Route::Library(_)) {
                if let Some(session) = session {
                    shell.route = Route::Library(session.read(cx).route());
                    session.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
            cx.notify();
        });
        // Sync state belongs to the application shell, not to Settings. The
        // page stays alive across navigation and Settings only renders it.
        let sync_page = cx.new(|cx| SyncPage::new(services.clone(), sync_state, libraries.clone(), window, cx));
        let global_pages = GlobalPages::new(services.updates.clone(), preferences, libraries.clone(), sync_page);
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            services,
            #[cfg(not(target_arch = "wasm32"))]
            open_book,
            route,
            content_footer: None,
            pending_folder: None,
            pending_subject: None,
            libraries,
            global_pages,
            navigation,
            _library_subscription: library_subscription,
            _navigation_subscription: navigation_subscription,
        }
    }

    fn empty_libraries(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = components::browser_theme(cx);
        if self.libraries.read(cx).creating_libraries() {
            return components::library_section_content().child(components::browser_loading_message("Adding library…", theme)).into_any_element();
        }
        let navigation = self.navigation.clone();
        let create = self.navigation.clone();
        let mut actions = gpui::div().flex().flex_wrap().justify_center().gap(gpui::px(components::SPACE_SM));
        #[cfg(not(target_arch = "arm"))]
        {
            actions = actions
                .child(components::primary_action_button("add-first-library", "Add library", gpui_component::IconName::Plus, false).on_click(move |_, _, cx| navigation.update(cx, |navigation, cx| navigation.choose_library_folders(cx))));
        }
        let actions = actions.child(
            components::secondary_action_button("create-first-library", "Create library", gpui_component::IconName::Plus, false)
                .on_click(move |_, window, cx| create.update(cx, |navigation, cx| navigation.open_create_library_dialog(window, cx))),
        );
        components::library_section_content().child(components::browser_actionable_empty_without_description("No libraries yet", actions, theme).size_full()).into_any_element()
    }

    pub(crate) fn open_folder(&mut self, library_id: LibraryId, directory_id: sync_common::DirId, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_folder = Some((library_id, directory_id, title));
        self.libraries.update(cx, |libraries, cx| libraries.select_library(library_id, cx));
        self.open_pending_folder(window, cx);
    }

    fn open_pending_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((library_id, _, _)) = &self.pending_folder else { return };
        let libraries = self.libraries.read(cx);
        if libraries.selected_id() != Some(*library_id) {
            return;
        }
        let Some(session) = libraries.selected_session() else { return };
        let (_, directory_id, title) = self.pending_folder.take().unwrap();
        self.route = Route::Library(super::LibraryRoute::Folders);
        session.update(cx, |session, cx| session.open_folder(directory_id, title, window, cx));
        cx.notify();
    }

    pub(crate) fn open_subject(&mut self, library_id: LibraryId, location: String, label: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_subject = Some((library_id, location, label));
        self.libraries.update(cx, |libraries, cx| libraries.select_library(library_id, cx));
        self.open_pending_subject(window, cx);
    }

    fn open_pending_subject(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((library_id, _, _)) = &self.pending_subject else { return };
        let libraries = self.libraries.read(cx);
        if libraries.selected_id() != Some(*library_id) {
            return;
        }
        let Some(session) = libraries.selected_session() else { return };
        let (_, location, label) = self.pending_subject.take().unwrap();
        self.route = Route::Library(super::LibraryRoute::Subjects);
        session.update(cx, |session, cx| session.open_subject(location, label, window, cx));
        cx.notify();
    }

    /// Imports an in-memory file into the active library and opens it for reading.
    #[cfg(not(target_arch = "wasm32"))]
    /// Imports a staged native file into the active library and opens it for reading.
    pub fn import_and_open_path(&mut self, name: String, path: std::path::PathBuf, _window: &mut Window, cx: &mut Context<Self>) {
        let library_id = self.libraries.read(cx).selected_id();
        let backend = self.services.backend.clone();
        let title = name.rsplit_once('.').map(|(title, _)| title).filter(|title| !title.is_empty()).unwrap_or(&name).to_owned();
        let open_book = self.open_book.clone();
        cx.spawn(async move |root, cx| {
            let result = async {
                let library = backend.library_for_import(library_id).await?;
                let _progress = root.update_in(cx, |_, window, cx| crate::services::ImportProgressToast::start(library.clone(), window, cx)).ok();
                let hash = library.import_owned_book(ROOT_DIR_ID, name, path.clone()).await?;
                Ok::<_, String>(library_model::BookLocator::new(*library.id(), hash))
            }
            .await;
            let _ = std::fs::remove_file(&path);
            let _ = root.update_in(cx, |_, window, cx| match result {
                Ok(locator) => open_book(locator, title, None, window, cx),
                Err(error) => crate::services::notify_error("incoming-book-import-error", format!("Could not import downloaded book: {error}"), window, cx),
            });
        })
        .detach();
    }

    /// Reports a failure that occurred before an incoming file reached the library.
    pub fn incoming_import_failed(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        crate::services::notify_error("incoming-book-stage-error", format!("Could not stage incoming book: {error}"), window, cx);
    }

    /// Replaces the current page with the view represented by `route`.
    /// Moves to the neighbouring rail destination. Clamped rather than
    /// wrapping: the rail is a visible list, and jumping from its end back to
    /// its start reads as a mis-press rather than navigation.
    pub(crate) fn step_destination(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let routes = super::sidebar::RAIL_ROUTES;
        let Some(current) = routes.iter().position(|route| *route == self.route) else {
            return;
        };
        let Some(target) = current.checked_add_signed(delta).and_then(|index| routes.get(index)) else {
            return;
        };
        self.navigate(*target, window, cx);
    }

    /// Focuses whichever page is on screen.
    ///
    /// Page keys — including the `pageup`/`pagedown` that Android volume buttons
    /// dispatch — are handled by a listener on the *page's* focus handle, so
    /// focusing the shell or the root leaves the grid unreachable from the
    /// keyboard even though the browser is visible.
    pub(crate) fn focus_active_page(&self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Library(_)) {
            if let Some(session) = self.libraries.read(cx).selected_session() {
                session.read(cx).focus_handle(cx).focus(window, cx);
            }
        }
    }

    pub(crate) fn navigate(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        if self.route == route {
            return;
        }
        self.route = route;
        match route {
            Route::Library(route) => {
                if let Some(session) = self.libraries.read(cx).selected_session() {
                    session.update(cx, |session, cx| session.navigate(route, window, cx));
                }
            }
            Route::Global(route) => self.global_pages.prepare(route, cx),
        }
        cx.notify();
    }

    /// Leaves a global page by revealing the selected library's last page.
    fn return_to_library_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.libraries.read(cx).selected_session();
        self.route = if let Some(session) = session {
            session.read(cx).focus_handle(cx).focus(window, cx);
            Route::Library(session.read(cx).route())
        } else {
            Route::Library(super::LibraryRoute::Home)
        };
        cx.notify();
    }

    /// Reveals the library switcher, for the pages that can be navigated out of
    /// at their top edge.
    pub(crate) fn open_library_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation.update(cx, |navigation, cx| navigation.open_library_switcher(window, cx));
    }

    pub(crate) fn contains_library(&self, library_id: &LibraryId, cx: &App) -> bool {
        self.libraries.read(cx).contains(library_id)
    }

    /// Closes shell overlays or returns a universal page to the library.
    pub(crate) fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.navigation.read(cx).is_library_switcher_open() {
            self.navigation.update(cx, |navigation, cx| navigation.close_library_switcher(window, cx));
            return true;
        }
        if matches!(self.route, Route::Global(_)) {
            self.return_to_library_page(window, cx);
            return true;
        }
        false
    }
}

impl Render for BrowserShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = self.route;
        let content = match route {
            Route::Library(_) => match self.libraries.read(cx).selected_session() {
                Some(session) => session.into_any_element(),
                None => self.empty_libraries(cx),
            },
            Route::Global(global) => components::library_section_content().child(self.global_pages.page(global, cx)).into_any_element(),
        };
        let compact_navigation = components::WindowWidthClass::for_window(window).is_compact();
        let mobile_heading = components::uses_mobile_navigation(window).then(|| route).and_then(|route| match route {
            // Home is the root of the mobile tab bar: there is nothing to go
            // back to, so its heading never has a back button.
            Route::Library(super::LibraryRoute::Home) => {
                let theme = components::browser_theme(cx);
                Some(components::mobile_navigation_bar("Home", None, theme))
            }
            Route::Global(super::GlobalRoute::Settings) => {
                let theme = components::browser_theme(cx);
                // The OS back button/gesture already does this on native
                // mobile; only compact-width web needs the on-screen one.
                let back = (!components::has_native_back_button()).then(|| components::mobile_back_button(theme).on_click(|_, window, cx| window.dispatch_action(Box::new(crate::DismissSettings), cx)));
                Some(components::mobile_navigation_bar("Settings", back, theme))
            }
            // Browse pages own their current folder or subject title.
            _ => None,
        });
        let content = gpui::div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .children(mobile_heading)
            .child(gpui::div().relative().flex_1().min_h_0().w_full().flex().flex_col().child(content).children(gpui_component::Root::render_notification_layer(window, cx)));
        let safe_area = window.insets().safe_area;
        let sidebar = self.navigation.clone();
        let navigation = sidebar.read(cx).render_navigation(sidebar.clone(), route, compact_navigation, safe_area, cx);
        // A plain flex sibling rather than an absolute overlay, so the dock
        // takes its own space instead of painting over other content. On
        // compact windows `page` is a column, so the dock goes between
        // `content` and `navigation` to sit above the bottom nav bar rather
        // than below it; on wide windows `page` is sidebar-beside-content
        // row, so the dock instead goes below the whole row, at the outer level.
        let footer = || self.content_footer.clone().map(|footer| gpui::div().w_full().flex_none().child(footer));
        let page = if compact_navigation {
            gpui::div().flex_1().min_h_0().min_w_0().flex().flex_col().pt(safe_area.top).child(content).children(footer()).child(navigation).into_any_element()
        } else {
            // A wide window may still be a phone in landscape, where the OS
            // button bar sits beside the page or along its bottom edge. The
            // dock clears the bottom edge when there is one.
            let bottom = if self.content_footer.is_some() { gpui::px(0.0) } else { safe_area.bottom };
            components::browser_body().pt(safe_area.top).pl(safe_area.left).pr(safe_area.right).pb(bottom).child(navigation).child(content).into_any_element()
        };

        // On compact windows the bottom nav bar already reserves `safe_area.bottom`
        // beneath itself, and the dock sits above that bar; on wide windows there
        // is no such bar, so the dock clears the OS gesture/button bar itself.
        let outer_footer = (!compact_navigation).then(footer).flatten().map(|footer| footer.pb(safe_area.bottom).pl(safe_area.left).pr(safe_area.right));
        gpui::div().size_full().relative().flex().flex_col().child(page).children(outer_footer).children(sidebar.read(cx).library_switcher_overlay(sidebar.clone(), window, cx)).into_any_element()
    }
}
