//! Persistent pages owned by one library session.

use std::collections::HashMap;
use std::rc::Rc;

use app::AppClient;
use app_preferences::LibraryView;
use gpui::prelude::*;
use gpui::{AnyView, App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription, Window, div, px};
use sync_common::{LibraryId, ROOT_DIR_ID};

use super::LibraryRoute;
use crate::library::BrowseCrumb;
use crate::library::{AuthorsPage, BookDetailClosed, BrowseKind, BrowsePage, BrowseSplitPlane, HomePage, LibraryContext, LibraryUpdateBridge, OpenBook, OpenTrash, TrashPage};

struct CachedPage {
    view: AnyView,
    focus: FocusHandle,
}

pub(super) struct LibrarySession {
    library: LibraryContext,
    route: LibraryRoute,
    pages: HashMap<LibraryRoute, CachedPage>,
    browse_planes: HashMap<LibraryRoute, Entity<BrowseSplitPlane>>,
    _library_updates_subscription: Subscription,
}

fn create_page<V: Focusable + Render>(build: impl FnOnce(&mut Context<V>) -> V, cx: &mut Context<LibrarySession>) -> CachedPage {
    let page = cx.new(build);
    let focus = page.read(cx).focus_handle(cx);
    CachedPage { view: page.into(), focus }
}

impl LibrarySession {
    pub(super) fn new(app_backend: Rc<AppClient>, open_book: OpenBook, book_detail_closed: BookDetailClosed, library_id: LibraryId, cx: &mut Context<Self>) -> Self {
        let library_backend = Rc::new(app_backend.library(library_id));
        let updates = LibraryUpdateBridge::create(app_backend, library_id, cx);
        let library = LibraryContext::new(library_backend, open_book, book_detail_closed, updates);
        let library_updates = library.updates();
        let library_updates_subscription = cx.observe(&library_updates, |_, _, cx| cx.notify());
        let mut session = Self { library, route: LibraryRoute::Home, pages: HashMap::new(), browse_planes: HashMap::new(), _library_updates_subscription: library_updates_subscription };
        let page = session.create_route_page(LibraryRoute::Home, cx);
        session.pages.insert(LibraryRoute::Home, page);
        session
    }

    pub(super) fn set_importing(&mut self, importing: bool, cx: &mut Context<Self>) {
        self.library.updates().update(cx, |updates, cx| updates.set_importing(importing, cx));
    }

    pub(super) fn set_library_view(&mut self, view: LibraryView, cx: &mut Context<Self>) {
        if LibrarySession::is_browse_route(self.route) {
            if let Some(plane) = self.browse_planes.get(&self.route) {
                plane.update(cx, |plane, cx| plane.set_library_view(view == LibraryView::Detailed, cx));
            }
        }
    }

    fn create_route_page(&self, route: LibraryRoute, cx: &mut Context<Self>) -> CachedPage {
        let library = self.library.clone();
        let session = cx.entity().downgrade();
        let open_trash: OpenTrash = Rc::new(move |_, cx| {
            let _ = session.update_in(cx, |session, window, cx| session.navigate(LibraryRoute::Trash, window, cx));
        });
        match route {
            LibraryRoute::Home => create_page(|cx| HomePage::new(library, cx), cx),
            LibraryRoute::Authors => create_page(|cx| AuthorsPage::new(library, cx), cx),
            LibraryRoute::Folders => create_page(|cx| BrowsePage::new(BrowseKind::Folder, library, open_trash, cx), cx),
            LibraryRoute::Subjects => create_page(|cx| BrowsePage::new(BrowseKind::Subject, library, open_trash, cx), cx),
            LibraryRoute::Trash => create_page(|cx| TrashPage::new(library, cx), cx),
        }
    }

    fn page(&self) -> &CachedPage {
        self.pages.get(&self.route).expect("the active library page must be cached")
    }

    fn is_browse_route(route: LibraryRoute) -> bool {
        matches!(route, LibraryRoute::Folders | LibraryRoute::Subjects)
    }

    fn browse_plane(&mut self, route: LibraryRoute, cx: &mut Context<Self>) -> Entity<BrowseSplitPlane> {
        if let Some(plane) = self.browse_planes.get(&route) {
            return plane.clone();
        }
        let kind = match route {
            LibraryRoute::Folders => BrowseKind::Folder,
            LibraryRoute::Subjects => BrowseKind::Subject,
            _ => unreachable!("only browse routes have browse planes"),
        };
        let library = self.library.clone();
        let session = cx.entity().downgrade();
        let open_trash: OpenTrash = Rc::new(move |_, cx| {
            let _ = session.update_in(cx, |session, window, cx| session.navigate(LibraryRoute::Trash, window, cx));
        });
        let plane = cx.new(|cx| BrowseSplitPlane::new(kind, library, open_trash.clone(), cx));
        self.browse_planes.insert(route, plane.clone());
        plane
    }

    pub(super) fn route(&self) -> LibraryRoute {
        self.route
    }

    pub(super) fn open_folder(&mut self, directory_id: sync_common::DirId, title: String, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(LibraryRoute::Folders, window, cx);
        self.browse_plane(LibraryRoute::Folders, cx).update(cx, |plane, cx| plane.open_folder(directory_id, title, cx));
    }

    pub(super) fn open_subject(&mut self, location: String, label: String, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(LibraryRoute::Subjects, window, cx);
        self.browse_plane(LibraryRoute::Subjects, cx).update(cx, |plane, cx| plane.open_subject(location, label, cx));
    }

    pub(super) fn open_book_detail(&mut self, content_hash: sync_common::ContentHash, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(LibraryRoute::Folders, window, cx);
        let backend = self.library.backend().clone();
        let plane = self.browse_plane(LibraryRoute::Folders, cx);
        cx.spawn(async move |session, cx| {
            let result = backend.book_detail(content_hash).await;
            cx.defer_update(session, move |session, cx| {
                if let Ok(detail) = result {
                    let path = detail.path.into_iter().map(|segment| BrowseCrumb { location: segment.id, label: segment.name }).collect();
                    plane.update(cx, |plane, cx| plane.open_book_detail(detail.book, path, cx));
                }
            });
        })
        .detach();
    }

    pub(super) fn clear_cached_book_detail(&mut self, cx: &mut Context<Self>) {
        for plane in self.browse_planes.values() {
            plane.update(cx, |plane, cx| plane.clear_cached_book_detail(cx));
        }
    }

    pub(super) fn focus_handle(&self, cx: &App) -> FocusHandle {
        if Self::is_browse_route(self.route) { self.browse_planes.get(&self.route).expect("browse plane must exist before it is focused").read(cx).focus_handle() } else { self.page().focus.clone() }
    }

    pub(super) fn navigate(&mut self, route: LibraryRoute, window: &mut Window, cx: &mut Context<Self>) {
        if Self::is_browse_route(route) {
            self.browse_plane(route, cx);
        } else if !self.pages.contains_key(&route) {
            let page = self.create_route_page(route, cx);
            self.pages.insert(route, page);
        }
        self.route = route;
        self.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn choose_book(&mut self, cx: &mut Context<Self>) {
        self.library.choose_and_import_book(ROOT_DIR_ID, cx);
    }
}

impl Render for LibrarySession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let library_id = *self.library.backend().id();
        if crate::services::library_add_trace::mark(library_id, "selected_view_render") {
            window.on_next_frame(move |_, _| {
                crate::services::library_add_trace::mark(library_id, "selected_frame_completed");
            });
        }
        let updates = self.library.updates();
        let updates = updates.read(cx);
        let uses_contents = matches!(self.route, LibraryRoute::Home | LibraryRoute::Authors | LibraryRoute::Folders | LibraryRoute::Subjects);
        if !(uses_contents && self.route != LibraryRoute::Folders && !updates.loading() && updates.book_count() == Some(0) && !updates.scanning()) {
            let view = if Self::is_browse_route(self.route) { self.browse_planes.get(&self.route).expect("browse plane must exist before render").clone().into_any_element() } else { self.page().view.clone().into_any_element() };
            return ui_components::library_section_content().child(view).into_any_element();
        }

        let theme = ui_components::browser_theme(cx);
        let session = cx.entity();
        let actions = div().flex().items_center().gap(px(ui_components::SPACE_SM)).child(ui_components::primary_action_button("empty-library-add-book", "Add book", gpui_component::IconName::Plus, false).on_click(move |_, _, cx| {
            session.update(cx, |session, cx| session.choose_book(cx));
        }));
        let heading = ui_components::uses_mobile_navigation(window)
            .then(|| match self.route {
                LibraryRoute::Authors => Some("Authors"),
                LibraryRoute::Folders => Some("Folders"),
                LibraryRoute::Subjects => Some("Subjects"),
                _ => None,
            })
            .flatten()
            .map(|title| {
                let back = (!ui_components::has_native_back_button()).then(|| ui_components::mobile_back_button(theme).on_click(|_, window, cx| window.dispatch_action(Box::new(crate::DismissSettings), cx)));
                ui_components::mobile_navigation_bar(title, back, theme)
            });
        ui_components::library_section_content()
            .children(heading)
            .child(ui_components::remaining_space_column().child(ui_components::browser_actionable_empty("No books in this library", "Add a book to get started.", actions, theme).size_full()))
            .into_any_element()
    }
}
