//! Browser shell and route coordinator.
//!
//! This shell owns the current route. Library sessions own library lifecycle
//! and per-library page state. Settings belongs to this shell.
//! The sidebar draws whichever route it is handed.

use ui_components::user_facing_error;

use crate::services::notify_error;
use crate::{NavigationSession, PaneRequest};
use gpui::prelude::*;
use gpui::{App, AppContext, Context, Entity, EntityId, IntoElement, Render, Subscription, Window};
use library_model::BrowseLocation;
#[cfg(not(target_arch = "wasm32"))]
use sync_common::ROOT_DIR_ID;
use sync_common::{DirId, LibraryId};
use ui_components as components;

use super::sidebar::{Sidebar, SplitPane};
use super::{GlobalRoute, Libraries, LibrarySession, Navigate, Route};
use crate::library::OpenBook;
use crate::services::AppServices;
use crate::stores::Preferences;
use crate::universal::settings::SettingsPage;
use crate::universal::sync::SyncPage;

/// Only the latest destination waits for library discovery or selection.
enum PendingNavigation {
    Restore(NavigationSession),
    Location { library_id: LibraryId, location: BrowseLocation, label: String },
}

impl PendingNavigation {
    fn library_id(&self) -> Option<LibraryId> {
        match self {
            Self::Restore(saved) => saved.library_id,
            Self::Location { library_id, .. } => Some(*library_id),
        }
    }
}

pub struct BrowserShell {
    services: AppServices,
    #[cfg(not(target_arch = "wasm32"))]
    open_book: OpenBook,
    route: Route,
    /// The browse view the phone's Library tab returns to: the last one shown.
    library_view: super::LibraryRoute,
    content_footer: Option<gpui::AnyView>,
    reader_surface: Option<gpui::AnyView>,
    split_close_enabled: bool,
    pending_navigation: Option<PendingNavigation>,
    libraries: Entity<Libraries>,
    sync_page: Entity<SyncPage>,
    settings: Entity<SettingsPage>,
    navigation: Entity<Sidebar>,
    observed_session: Option<(EntityId, [Subscription; 2])>,
    _library_subscription: Subscription,
    _navigation_subscription: Subscription,
}

impl gpui::EventEmitter<PaneRequest> for BrowserShell {}

impl BrowserShell {
    pub(crate) fn set_now_playing(&self, book: Option<crate::NowPlayingBook>, cx: &mut App) {
        self.libraries.update(cx, |libraries, cx| libraries.set_now_playing(book, cx));
    }

    pub(crate) fn set_reader_surface(&mut self, surface: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        if self.reader_surface.as_ref().map(|view| view.entity_id()) != surface.as_ref().map(|view| view.entity_id()) {
            self.reader_surface = surface;
            cx.notify();
        }
    }

    pub(crate) fn set_split_close_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.split_close_enabled != enabled {
            self.split_close_enabled = enabled;
            cx.notify();
        }
    }

    fn close_reader_surface(&mut self, cx: &mut Context<Self>) {
        if self.reader_surface.take().is_some() {
            cx.emit(PaneRequest::Browse);
            cx.notify();
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn apply_sync_event(&mut self, event: crate::SyncEvent, cx: &mut Context<Self>) {
        self.sync_page.update(cx, |page, cx| page.apply_event(event, cx));
    }
    pub(crate) fn set_content_footer(&mut self, footer: Option<gpui::AnyView>, cx: &mut Context<Self>) {
        if self.content_footer.as_ref().map(|view| view.entity_id()) == footer.as_ref().map(|view| view.entity_id()) {
            return;
        }
        self.content_footer = footer;
        cx.notify();
    }

    pub(crate) fn shown_book_detail(&self, cx: &App) -> Option<library_model::BookLocator> {
        if !matches!(self.route, Route::Library(_)) {
            return None;
        }
        self.libraries.read(cx).selected_session()?.read(cx).shown_book_detail(cx)
    }

    pub(crate) fn open_book_detail(&mut self, library_id: LibraryId, content_hash: sync_common::BookId, window: &mut Window, cx: &mut Context<Self>) {
        self.libraries.update(cx, |libraries, cx| libraries.select_library(library_id, cx));
        let Some(session) = self.libraries.read(cx).selected_session() else { return };
        session.update(cx, |session, cx| session.open_book_detail(content_hash, window, cx));
    }

    /// Builds the browser around the restored session and starts background subscriptions.
    pub fn new(services: AppServices, mut saved: NavigationSession, sync_state: Entity<crate::SyncState>, open_book: OpenBook, preferences: Entity<Preferences>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let libraries = services.startup.libraries.clone();
        saved.reconcile(&libraries.iter().map(|entry| *entry.library_id()).collect::<Vec<_>>());
        let active_library_id = saved.library_id;
        let shell = cx.entity().downgrade();
        let navigate_callback: Navigate = std::rc::Rc::new(move |route, window, cx| {
            let _ = shell.update(cx, |shell, cx| shell.select_destination(route, window, cx));
        });
        let libraries = cx.new(|cx| Libraries::new(services.backend.clone(), open_book.clone(), libraries, active_library_id, cx));
        let session = libraries.read(cx).selected_session();
        if let Some(session) = &session
            && let Route::Library(route) = saved.route
        {
            session.update(cx, |session, cx| session.restore(route, saved.pages, window, cx));
        }
        let observed_session = session.as_ref().map(|session| Self::observe_session(session, cx));
        let route = if matches!(saved.route, Route::Global(_)) {
            saved.route
        } else if let Some(session) = session {
            session.update(cx, |session, cx| session.focus_page(window, cx));
            Route::Library(session.read(cx).route())
        } else {
            Route::Library(super::LibraryRoute::Home)
        };
        let navigation = cx.new(|cx| Sidebar::new(libraries.clone(), navigate_callback, cx));
        // Sidebar elements are rendered by this shell rather than as an entity
        // child. Forward its state changes so menus and dialogs are repainted.
        let navigation_subscription = cx.observe(&navigation, |_, _, cx| cx.notify());
        cx.subscribe(&navigation, |_, _, request: &PaneRequest, cx| cx.emit(request.clone())).detach();
        cx.subscribe(&navigation, |shell, _, _: &SplitPane, cx| {
            let session = shell.navigation_session(cx);
            cx.emit(PaneRequest::Open { session, side: components::SplitSide::Right });
        })
        .detach();
        let library_subscription = cx.observe_in(&libraries, window, |shell, libraries, window, cx| {
            shell.open_pending_navigation(window, cx);
            let session = libraries.read(cx).selected_session();
            let session_id = session.as_ref().map(Entity::entity_id);
            let selection_changed = shell.observed_session.as_ref().map(|(id, _)| *id) != session_id;
            if selection_changed {
                shell.close_reader_surface(cx);
                shell.observed_session = session.as_ref().map(|session| Self::observe_session(session, cx));
            }
            if matches!(shell.route, Route::Library(_))
                && let Some(session) = session
            {
                shell.route = Route::Library(session.read(cx).route());
                // Background list refreshes must not take focus from an open
                // dialog, menu, or search field in the same library.
                if selection_changed {
                    session.update(cx, |session, cx| session.focus_page(window, cx));
                }
            }
            cx.notify();
        });
        // Sync state belongs to the application shell, not to Settings. The
        // page stays alive across navigation and Settings only renders it.
        let sync_page = cx.new(|cx| SyncPage::new(services.clone(), sync_state, libraries.clone(), window, cx));
        let settings = cx.new(|cx| SettingsPage::new(services.updates.clone(), sync_page.clone(), preferences, cx));
        Self {
            services,
            #[cfg(not(target_arch = "wasm32"))]
            open_book,
            route,
            library_view: super::LibraryRoute::Books,
            content_footer: None,
            reader_surface: None,
            split_close_enabled: false,
            pending_navigation: None,
            libraries,
            sync_page,
            settings,
            navigation,
            observed_session,
            _library_subscription: library_subscription,
            _navigation_subscription: navigation_subscription,
        }
    }

    /// Identity and subscriptions change together, so a cached session stops
    /// driving this shell as soon as another library is selected.
    fn observe_session(session: &Entity<LibrarySession>, cx: &mut Context<Self>) -> (EntityId, [Subscription; 2]) {
        let subscriptions = [
            cx.observe(session, |shell, session, cx| {
                if matches!(shell.route, Route::Library(_)) {
                    shell.route = Route::Library(session.read(cx).route());
                }
                cx.notify();
            }),
            cx.subscribe(session, |_, _, request: &PaneRequest, cx| cx.emit(request.clone())),
        ];
        (session.entity_id(), subscriptions)
    }

    /// A value snapshot assembled from the components that own navigation.
    pub(crate) fn navigation_session(&self, cx: &App) -> NavigationSession {
        let library_id = self.libraries.read(cx).selected_id();
        let pages = if matches!(self.route, Route::Library(_)) { self.libraries.read(cx).selected_session().map(|session| session.read(cx).saved_pages(cx)).unwrap_or_default() } else { Vec::new() };
        NavigationSession { library_id, route: self.route, pages, ..Default::default() }
    }

    fn empty_libraries(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = components::browser_theme(cx);
        if self.libraries.read(cx).creating_libraries() {
            return components::library_section_content().child(components::browser_loading_message("Adding library…", theme)).into_any_element();
        }
        let actions = self.navigation.read(cx).render_add_library_control(self.navigation.clone(), components::primary_action_button("add-first-library", "Add library", gpui_component::IconName::ChevronDown, false), false);
        components::library_section_content().child(components::browser_actionable_empty("No libraries yet", actions, theme).size_full()).into_any_element()
    }

    pub(crate) fn restore_navigation(&mut self, saved: NavigationSession, window: &mut Window, cx: &mut Context<Self>) {
        self.close_reader_surface(cx);
        self.request_navigation(PendingNavigation::Restore(saved), window, cx);
    }

    fn request_navigation(&mut self, request: PendingNavigation, window: &mut Window, cx: &mut Context<Self>) {
        let library_id = request.library_id();
        self.pending_navigation = Some(request);
        if let Some(id) = library_id {
            self.libraries.update(cx, |libraries, cx| libraries.select_library(id, cx));
        }
        self.open_pending_navigation(window, cx);
    }

    fn open_pending_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let libraries = self.libraries.read(cx);
        let session = libraries.selected_session();
        let Some(request) = self.pending_navigation.take_if(|request| request.library_id() == libraries.selected_id() && (session.is_some() || matches!(request, PendingNavigation::Restore(_)))) else { return };
        match request {
            PendingNavigation::Restore(saved) => {
                self.route = saved.route;
                if let (Some(session), Route::Library(route)) = (session, saved.route) {
                    session.update(cx, |session, cx| session.restore(route, saved.pages, window, cx));
                }
            }
            PendingNavigation::Location { location, label, .. } => {
                if let Some(session) = session {
                    let route = session.update(cx, |session, cx| {
                        match location {
                            BrowseLocation::Folder(id) => session.open_folder(id, label, window, cx),
                            _ => session.open_subject(location, label, window, cx),
                        }
                        session.route()
                    });
                    self.route = Route::Library(route);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn open_folder(&mut self, library_id: LibraryId, directory_id: DirId, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.request_navigation(PendingNavigation::Location { library_id, location: BrowseLocation::Folder(directory_id), label: title }, window, cx);
    }

    pub(crate) fn open_subject(&mut self, library_id: LibraryId, location: library_model::BrowseLocation, label: String, window: &mut Window, cx: &mut Context<Self>) {
        self.request_navigation(PendingNavigation::Location { library_id, location, label }, window, cx);
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
                let _progress = root.update_in(cx, |_, window, cx| crate::services::ImportProgressToast::start(window, cx)).ok();
                let hash = library.import_stored_book(ROOT_DIR_ID, name, path.clone()).await?;
                Ok::<_, app::BackendError>(library_model::BookLocator::new(*library.id(), hash))
            }
            .await;
            let _ = std::fs::remove_file(&path);
            let _ = root.update_in(cx, |_, window, cx| match result {
                Ok(locator) => open_book(locator, title, None, window, cx),
                Err(error) => {
                    log::warn!("Could not import downloaded book: {error}");
                    notify_error("incoming-book-import-error", crate::services::book_transfer_error_message(error.failure()), window, cx);
                }
            });
        })
        .detach();
    }

    /// Reports a failure that occurred before an incoming file reached the library.
    pub fn incoming_import_failed(&mut self, error: app::BackendError, window: &mut Window, cx: &mut Context<Self>) {
        notify_error("incoming-book-stage-error", format!("Could not prepare this book for import. {}", user_facing_error(&error, "Please try again.")), window, cx);
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
        if matches!(self.route, Route::Library(_))
            && let Some(session) = self.libraries.read(cx).selected_session()
        {
            session.update(cx, |session, cx| session.focus_page(window, cx));
        }
    }

    /// Selecting the active destination returns it to its root. Switching
    /// destinations keeps the cached page, as ordinary navigation does.
    fn select_destination(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        self.close_reader_surface(cx);
        if self.route != route {
            self.navigate(route, window, cx);
            return;
        }
        match route {
            Route::Library(_) => {
                if let Some(session) = self.libraries.read(cx).selected_session() {
                    session.update(cx, |session, cx| session.open_root(window, cx));
                }
            }
            Route::Global(GlobalRoute::Settings) => {
                self.settings.update(cx, |page, cx| page.open_root(window, cx));
            }
        }
        cx.notify();
    }

    pub(crate) fn navigate(&mut self, route: Route, window: &mut Window, cx: &mut Context<Self>) {
        self.close_reader_surface(cx);
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
            Route::Global(GlobalRoute::Settings) => {
                // Refresh local storage usage when returning after imports.
                self.sync_page.update(cx, |page, cx| page.schedule_refresh(cx));
            }
        }
        cx.notify();
    }

    /// Leaves a global page by revealing the selected library's last page.
    fn return_to_library_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.libraries.read(cx).selected_session();
        self.route = if let Some(session) = session {
            session.update(cx, |session, cx| session.focus_page(window, cx));
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

    /// Resolve native Back against visible state even when a toolbar, a removed
    /// control, or no element holds focus. Only Home may fall through to Android.
    pub(crate) fn navigate_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.dismiss(window, cx) {
            return true;
        }
        if let Some(session) = self.libraries.read(cx).selected_session()
            && session.update(cx, |session, cx| session.dismiss_page(cx))
        {
            return true;
        }
        if self.route != Route::Library(super::LibraryRoute::Home) {
            self.navigate(super::LibraryRoute::Home.into(), window, cx);
            return true;
        }
        false
    }
}

impl Render for BrowserShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let route = self.route;
        let content = if let Some(reader) = &self.reader_surface {
            reader.clone().into_any_element()
        } else {
            match route {
                Route::Library(_) => match self.libraries.read(cx).selected_session() {
                    Some(session) => session.into_any_element(),
                    None => self.empty_libraries(cx),
                },
                Route::Global(GlobalRoute::Settings) => components::library_section_content().child(self.settings.clone()).into_any_element(),
            }
        };
        let compact_navigation = components::uses_mobile_navigation(window);
        let mobile_heading = components::uses_mobile_navigation(window).then(|| route).and_then(|route| match route {
            Route::Global(super::GlobalRoute::Settings) => {
                let theme = components::browser_theme(cx);
                Some(components::mobile_navigation_bar("Settings", super::mobile_back(true), theme))
            }
            // Browse pages, Home among them, own their current title.
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
            .child(gpui::div().relative().flex_1().min_h_0().w_full().flex().flex_col().child(content).when(!self.split_close_enabled, |content| content.children(gpui_component::Root::render_notification_layer(window, cx))));
        let safe_area = window.insets().effective();
        let sidebar = self.navigation.clone();
        // The route reaches the shell by several paths (the rail, the session's
        // own navigation, restored state), so the view is noted where they all
        // end up: here.
        if let Route::Library(view) = route
            && super::LIBRARY_VIEWS.contains(&view)
        {
            self.library_view = view;
        }
        let navigation = sidebar.read(cx).render_navigation(sidebar.clone(), route, self.library_view, compact_navigation, self.split_close_enabled, safe_area, window, cx);
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
            components::browser_body().when(self.split_close_enabled, |body| body.gap(gpui::px(components::SPACE_SM))).pt(safe_area.top).pl(safe_area.left).pr(safe_area.right).pb(bottom).child(navigation).child(content).into_any_element()
        };

        // On compact windows the bottom nav bar already reserves `safe_area.bottom`
        // beneath itself, and the dock sits above that bar; on wide windows there
        // is no such bar, so the dock clears the OS gesture/button bar itself.
        let outer_footer = (!compact_navigation).then(footer).flatten().map(|footer| footer.pb(safe_area.bottom).pl(safe_area.left).pr(safe_area.right));
        gpui::div().size_full().relative().flex().flex_col().child(page).children(outer_footer).children(sidebar.read(cx).library_switcher_overlay(sidebar.clone(), window, cx)).into_any_element()
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use crate::{AppStartup, LibraryRoute, SyncState};
    use gpui::TestAppContext;
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    async fn only_the_selected_session_drives_routes_and_pane_requests(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = app::AppClient::start_native(launch).unwrap();
        let first = *backend.create_library_with_asset_storage("First".into(), true).await.unwrap().library_id();
        let second = *backend.create_library_with_asset_storage("Second".into(), true).await.unwrap().library_id();
        let startup = AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status };
        let services = AppServices::new(backend, startup);
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::init_page_scrolling(cx);
        });
        let sync = cx.new(|_| SyncState::new(&services));
        let window = cx.add_window(|_, _| gpui::Empty);
        let shell = window
            .update(cx, |_, window, cx| {
                let preferences = Preferences::shared(&services, cx);
                let saved = NavigationSession { library_id: Some(first), ..Default::default() };
                cx.new(|cx| BrowserShell::new(services, saved, sync, Rc::new(|_, _, _, _, _| {}), preferences, window, cx))
            })
            .unwrap();
        let libraries = shell.read_with(cx, |shell, _| shell.libraries.clone());
        let original = libraries.read_with(cx, |libraries, _| libraries.selected_session().unwrap());
        let requests = Rc::new(Cell::new(0));
        let received = requests.clone();
        let _subscription = cx.update(|cx| cx.subscribe(&shell, move |_, _: &PaneRequest, _| received.set(received.get() + 1)));
        original.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        assert_eq!(requests.get(), 1, "startup observes the restored session");

        libraries.update(cx, |libraries, cx| libraries.select_library(second, cx));
        cx.run_until_parked();
        let selected = libraries.read_with(cx, |libraries, _| libraries.selected_session().unwrap());
        original.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        selected.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        assert_eq!(requests.get(), 2, "a cached session must stop forwarding pane requests");
        window
            .update(cx, |_, window, cx| {
                selected.update(cx, |session, cx| session.navigate(LibraryRoute::Authors, window, cx));
                original.update(cx, |session, cx| session.navigate(LibraryRoute::Catalog, window, cx));
            })
            .unwrap();
        assert_eq!(shell.read_with(cx, |shell, _| shell.route), Route::Library(LibraryRoute::Authors));
        window
            .update(cx, |_, window, cx| {
                shell.update(cx, |shell, cx| shell.navigate(GlobalRoute::Settings.into(), window, cx));
                selected.update(cx, |session, cx| session.navigate(LibraryRoute::Folders, window, cx));
            })
            .unwrap();
        assert_eq!(shell.read_with(cx, |shell, _| shell.route), GlobalRoute::Settings.into());

        libraries.update(cx, |libraries, cx| libraries.select_library(first, cx));
        cx.run_until_parked();
        window.update(cx, |_, window, cx| shell.update(cx, |shell, cx| shell.return_to_library_page(window, cx))).unwrap();
        assert_eq!(shell.read_with(cx, |shell, _| shell.route), Route::Library(LibraryRoute::Catalog));
        original.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        selected.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        assert_eq!(requests.get(), 3, "returning to a cached session reconnects it exactly once");

        for id in [first, second] {
            libraries.update(cx, |libraries, cx| libraries.delete_library(id, cx));
            cx.condition(&libraries, |libraries, _| !libraries.contains(&id)).await;
        }
        cx.run_until_parked();
        original.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        selected.update(cx, |_, cx| cx.emit(PaneRequest::Close));
        assert_eq!(requests.get(), 3, "removing every library disconnects retained sessions");
        assert!(shell.read_with(cx, |shell, _| shell.observed_session.is_none()));
    }
}
