//! Independent application panes, composed in one native window.

use std::path::PathBuf;
use std::rc::Rc;

use app::AppClient;
use browser_ui::{AppServices, NavigationSession, PaneRequest, SaveNavigationSession, WindowSession};
use gpui::prelude::*;
use gpui::{App, AppContext, Context, Entity, FocusHandle, IntoElement, Render, SharedString, Subscription, Window, div, px};
use gpui_image_eviction::trim_unused_images_after_transition;
use library_model::BookLocator;
use ui_components::SplitSide;
use uuid::Uuid;

use super::{ApplicationPane, playback::PlaybackController};

struct Pane {
    id: Uuid,
    view: Entity<ApplicationPane>,
    last_focus: Option<FocusHandle>,
    _subscriptions: Vec<Subscription>,
}

/// A window contains complete application panes rather than copies of one
/// library view. Only preferences and backend services are shared.
pub struct ApplicationRoot {
    services: AppServices,
    backend: AppClient,
    title: SharedString,
    playback_restore_path: Option<PathBuf>,
    #[cfg(feature = "kobo")]
    kobo_auto_rotate: bool,
    panes: Vec<Pane>,
    active: usize,
    save: SaveNavigationSession,
    last_saved: Option<NavigationSession>,
    native_back_overlay: Option<super::NativeBackOverlay>,
}

impl ApplicationRoot {
    pub fn new(
        services: AppServices, session: WindowSession, backend: AppClient, application_title: impl Into<SharedString>, playback_restore_path: Option<PathBuf>, #[cfg(feature = "kobo")] kobo_auto_rotate: bool, window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut initial = session.initial;
        let active = initial.active_pane;
        let mut saved = std::mem::take(&mut initial.panes);
        if saved.is_empty() {
            // Upgrade the former same-library, same-view splits.
            let pages = std::mem::take(&mut initial.pages);
            if pages.len() > 1 {
                for (index, page) in pages.into_iter().enumerate() {
                    let mut pane = initial.clone();
                    pane.pages = vec![page];
                    if index > 0 {
                        pane.reader = None;
                        pane.pane_id = None;
                    }
                    saved.push(pane);
                }
            } else {
                initial.pages = pages;
                saved.push(initial);
            }
        }
        let mut root = Self {
            services,
            backend,
            title: application_title.into(),
            playback_restore_path,
            #[cfg(feature = "kobo")]
            kobo_auto_rotate,
            panes: Vec::new(),
            active: 0,
            save: session.save,
            last_saved: None,
            native_back_overlay: None,
        };
        for (index, saved) in saved.into_iter().enumerate() {
            root.add_pane(index, saved, root.services.clone(), window, cx);
        }
        root.active = active.min(root.panes.len().saturating_sub(1));
        root.refresh_split_controls(cx);
        root.save_navigation(cx);
        root
    }

    fn add_pane(&mut self, index: usize, mut saved: NavigationSession, services: AppServices, window: &mut Window, cx: &mut Context<Self>) {
        let legacy = saved.pane_id.is_none();
        let id = saved.pane_id.unwrap_or_else(Uuid::new_v4);
        saved.pane_id = Some(id);
        saved.panes.clear();
        saved.active_pane = 0;
        let controller = if browser_ui::native_windows_available() {
            let path = self.playback_restore_path.as_ref().map(|base| base.with_extension(format!("pane-{id}.json")));
            if legacy && index == 0 && !PlaybackController::has_controllers(cx) {
                if let (Some(base), Some(path)) = (&self.playback_restore_path, &path) {
                    if base.is_file() && !path.exists() {
                        if let Err(error) = std::fs::copy(base, path) {
                            log::warn!("could not migrate pane playback restoration: {error}");
                        }
                    }
                }
            }
            Some(PlaybackController::independent(self.backend.clone(), path, cx))
        } else {
            None
        };
        let owner = cx.weak_entity();
        let save = Rc::new(move |_: NavigationSession, cx: &mut App| {
            let owner = owner.clone();
            // A child saves during construction; observe it after insertion.
            cx.defer(move |cx| {
                let _ = owner.update(cx, |root, cx| {
                    root.save_navigation(cx);
                    cx.notify();
                });
            });
        });
        let view = cx.new(|cx| {
            ApplicationPane::new_with_playback(
                services,
                WindowSession { initial: saved, save },
                self.backend.clone(),
                self.title.clone(),
                self.playback_restore_path.clone(),
                controller,
                #[cfg(feature = "kobo")]
                self.kobo_auto_rotate,
                window,
                cx,
            )
        });
        let focus = view.read(cx).opening_focus.clone();
        let subscriptions = vec![
            cx.observe(&view, |root, _, cx| {
                root.save_navigation(cx);
                cx.notify();
            }),
            cx.subscribe_in(&view, window, |root, source, request: &PaneRequest, window, cx| root.handle_request(source, request.clone(), window, cx)),
            cx.on_focus_in(&focus, window, move |root, _, cx| {
                if let Some(index) = root.panes.iter().position(|pane| pane.id == id) {
                    if root.active != index {
                        root.active = index;
                        root.save_navigation(cx);
                        cx.notify();
                    }
                }
            }),
        ];
        self.panes.insert(index.min(self.panes.len()), Pane { id, view, last_focus: None, _subscriptions: subscriptions });
    }

    fn handle_request(&mut self, source: &Entity<ApplicationPane>, request: PaneRequest, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.panes.iter().position(|pane| pane.view == *source) else { return };
        match request {
            PaneRequest::Close => self.close_pane(index, window, cx),
            PaneRequest::Browse => source.update(cx, |pane, cx| pane.return_to_library(window, cx)),
            PaneRequest::Open { session, side } => {
                if !browser_ui::native_windows_available() || ui_components::WindowWidthClass::for_window(window).is_compact() {
                    return;
                }
                let adjacent = match side {
                    SplitSide::Left => index.checked_sub(1),
                    SplitSide::Right => (index + 1 < self.panes.len()).then_some(index + 1),
                };
                if let Some(adjacent) = adjacent {
                    self.active = adjacent;
                    self.panes[adjacent].view.update(cx, |pane, cx| {
                        pane.return_to_library(window, cx);
                        pane.browser.update(cx, |browser, cx| browser.restore_navigation(session, window, cx));
                    });
                    self.save_navigation(cx);
                    return;
                }
                let source = self.panes[index].id;
                let services = self.services.clone();
                cx.spawn(async move |root, cx| {
                    let services = services.for_new_window().await;
                    let _ = root.update_in(cx, |root, window, cx| {
                        let Some(index) = root.panes.iter().position(|pane| pane.id == source) else { return };
                        let services = match services {
                            Ok(services) => services,
                            Err(error) => {
                                log::warn!("could not open split pane: {error}");
                                return;
                            }
                        };
                        let insert = match side {
                            SplitSide::Left => index,
                            SplitSide::Right => index + 1,
                        };
                        let adjacent = match side {
                            SplitSide::Left => index.checked_sub(1),
                            SplitSide::Right => (index + 1 < root.panes.len()).then_some(index + 1),
                        };
                        if let Some(adjacent) = adjacent {
                            root.panes[adjacent].view.update(cx, |pane, cx| {
                                pane.return_to_library(window, cx);
                                pane.browser.update(cx, |browser, cx| browser.restore_navigation(session, window, cx));
                            });
                            root.active = adjacent;
                        } else {
                            root.add_pane(insert, session, services, window, cx);
                            root.active = insert;
                        }
                        root.refresh_split_controls(cx);
                        root.save_navigation(cx);
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }

    fn close_pane(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.len() <= 1 {
            return;
        }
        let pane = self.panes.remove(index);
        trim_unused_images_after_transition(window);
        let controller = pane.view.read(cx).playback.clone();
        let flush = controller.update(cx, |controller, cx| controller.close_pane(cx));
        cx.spawn(async move |_, _| {
            // Retain the controller until positions and its restore tombstone
            // are saved, including writes from an already closed dock.
            let _controller = controller;
            if let Err(error) = flush.await {
                log::warn!("could not save closed pane playback: {error}");
            }
        })
        .detach();
        if self.active > index {
            self.active -= 1;
        }
        self.active = self.active.min(self.panes.len() - 1);
        self.refresh_split_controls(cx);
        if let Some(focus) = &self.panes[self.active].last_focus {
            focus.focus(window, cx);
        } else {
            self.active_view().update(cx, |pane, cx| pane.browser.update(cx, |browser, cx| browser.focus_active_page(window, cx)));
        }
        self.save_navigation(cx);
        cx.notify();
    }

    fn refresh_split_controls(&self, cx: &mut App) {
        let enabled = self.panes.len() > 1;
        for pane in &self.panes {
            pane.view.update(cx, |pane, cx| {
                pane.split_enabled = enabled;
                pane.browser.update(cx, |browser, cx| browser.set_split_close_enabled(enabled, cx));
                cx.notify();
            });
        }
    }

    fn active_view(&self) -> Entity<ApplicationPane> {
        self.panes[self.active].view.clone()
    }

    fn save_navigation(&mut self, cx: &mut App) {
        if self.panes.is_empty() {
            return;
        }
        let sessions = self
            .panes
            .iter()
            .map(|pane| {
                let mut saved = pane.view.read(cx).last_saved_navigation.clone().unwrap_or_default();
                saved.pane_id = Some(pane.id);
                saved
            })
            .collect::<Vec<_>>();
        let mut saved = sessions[self.active].clone();
        if sessions.len() > 1 {
            saved.panes = sessions;
            saved.active_pane = self.active;
        }
        if self.last_saved.as_ref() != Some(&saved) {
            self.last_saved = Some(saved.clone());
            (self.save)(saved, cx);
        }
    }

    pub fn open_window_target(&mut self, target: browser_ui::NewWindowTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.open_window_target(target, window, cx));
    }
    pub fn open_book(&mut self, locator: BookLocator, title: String, target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.open_book(locator, title, target, window, cx));
    }
    pub fn return_to_library(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.return_to_library(window, cx));
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn import_and_open_path(&mut self, name: String, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.import_and_open_path(name, path, window, cx));
    }
    pub fn incoming_import_failed(&mut self, error: app::BackendError, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.incoming_import_failed(error, window, cx));
    }
    pub fn native_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if gpui_component::Root::dismiss_active_overlay(window, cx) {
            return true;
        }
        if self.native_back_overlay.as_ref().is_some_and(|handler| handler(window, cx)) {
            return true;
        }
        self.active_view().update(cx, |pane, cx| pane.native_back(window, cx))
    }
    pub fn set_native_back_overlay(&mut self, handler: impl Fn(&mut Window, &mut App) -> bool + 'static) {
        self.native_back_overlay = Some(Rc::new(handler));
    }

    pub fn audiobook_session(&self, cx: &App) -> Option<Entity<reader_ui::PlaybackSession>> {
        self.active_view().read(cx).audiobook_session()
    }
    pub fn audiobook_is_expanded(&self, cx: &App) -> bool {
        self.active_view().read(cx).audiobook_is_expanded()
    }
    pub fn expand_audiobook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.expand_audiobook(window, cx));
    }
    pub fn close_audiobook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_view().update(cx, |pane, cx| pane.close_audiobook(window, cx));
    }
}

impl Render for ApplicationRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let split = browser_ui::native_windows_available() && !ui_components::WindowWidthClass::for_window(window).is_compact();
        let theme = ui_components::browser_theme(cx);
        let multiple = self.panes.len() > 1;
        if multiple {
            cx.set_keep_screen_awake(self.panes.iter().any(|pane| pane.view.read(cx).reader.is_some()));
        }
        div()
            .size_full()
            .relative()
            .bg(theme.page_bg)
            .flex()
            .flex_row()
            .items_stretch()
            .gap(px(ui_components::SPACE_SM))
            .children(self.panes.iter().enumerate().filter(|(index, _)| split || *index == self.active).map(|(index, pane)| {
                div()
                    .id(("application-pane", pane.view.entity_id()))
                    .debug_selector(move || format!("application-pane-{index}"))
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .capture_any_mouse_down(cx.listener(move |root, _, window, cx| {
                        if root.active != index {
                            root.panes[root.active].last_focus = window.focused(cx);
                            root.active = index;
                            root.save_navigation(cx);
                            cx.notify();
                        }
                    }))
                    .child(pane.view.clone())
            }))
            .when(multiple, |root| {
                root.children(gpui_component::Root::render_dialog_layer(window, cx))
                    .children(gpui_component::Root::render_notification_layer(window, cx))
                    .children(gpui_component::Root::render_sheet_layer(window, cx).map(|sheet| gpui::deferred(sheet).with_priority(2)))
            })
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use browser_ui::{AppStartup, LibraryRoute, Route};
    use gpui::{TestAppContext, WindowHandle};
    use gpui_component::Root;
    use sync_common::{BookId, LibraryId, ROOT_DIR_ID};

    async fn fixture(cx: &mut TestAppContext) -> (tempfile::TempDir, WindowHandle<Root>, Entity<ApplicationRoot>, [LibraryId; 2]) {
        cx.executor().allow_parking();
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, startup) = AppClient::start_native(launch).unwrap();
        let first = backend.create_library_with_asset_storage("First pane".into(), true).await.unwrap();
        let second = backend.create_library_with_asset_storage("Second pane".into(), true).await.unwrap();
        let ids = [*first.library_id(), *second.library_id()];
        let services = AppServices::new(backend.clone(), AppStartup { libraries: backend.libraries().await.unwrap(), browsing_preferences: startup.browsing_preferences, account_status: startup.account_status });
        cx.update(|cx| {
            gpui_component::init(cx);
            ui_components::init_page_scrolling(cx);
            ui_components::register_fonts(cx).unwrap();
            reader_ui::configure(reader_ui::ReaderPreferences::default(), Rc::new(|_, _, _| {}), cx);
        });
        let initial = NavigationSession {
            panes: vec![
                NavigationSession { library_id: Some(ids[0]), route: Route::Library(LibraryRoute::Folders), ..Default::default() },
                NavigationSession { library_id: Some(ids[1]), route: Route::Library(LibraryRoute::Books), ..Default::default() },
            ],
            ..Default::default()
        };
        let window = cx.add_window(|window, cx| {
            let root = cx.new(|cx| ApplicationRoot::new(services, WindowSession { initial, ..Default::default() }, backend, "Panes test", None, window, cx));
            Root::new(root, window, cx)
        });
        let root = window.update(cx, |root, _, _| root.view().clone().downcast::<ApplicationRoot>().unwrap()).unwrap();
        (directory, window, root, ids)
    }

    #[gpui::test]
    async fn panes_share_width_evenly_with_space_for_each_sidebar(cx: &mut TestAppContext) {
        let (_directory, window, _root, _) = fixture(cx).await;
        let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let left = visual.debug_bounds("application-pane-0").expect("left pane");
        let right = visual.debug_bounds("application-pane-1").expect("right pane");
        assert_eq!(left.size.width, right.size.width);
        assert_eq!(right.origin.x - left.right(), px(ui_components::SPACE_SM));
    }

    #[gpui::test]
    async fn panes_navigate_and_restore_different_libraries_independently(cx: &mut TestAppContext) {
        let (_directory, window, root, ids) = fixture(cx).await;
        window
            .update(cx, |_, window, cx| {
                root.update(cx, |root, cx| {
                    let second = root.panes[1].view.read(cx).browser.read(cx).navigation_session(cx);
                    root.panes[0].view.update(cx, |pane, cx| pane.browser.update(cx, |browser, cx| browser.open_folder(ids[1], ROOT_DIR_ID, "Folders".into(), window, cx)));
                    assert_eq!(root.panes[1].view.read(cx).browser.read(cx).navigation_session(cx), second);
                    root.panes[1].view.update(cx, |pane, cx| {
                        pane.browser.update(cx, |browser, cx| browser.restore_navigation(NavigationSession { library_id: Some(ids[0]), route: Route::Library(LibraryRoute::Authors), ..Default::default() }, window, cx))
                    });
                    let first = root.panes[0].view.read(cx).browser.read(cx).navigation_session(cx);
                    assert_eq!(first.library_id, Some(ids[1]));
                    assert_eq!(first.route, Route::Library(LibraryRoute::Folders));
                })
            })
            .unwrap();
        cx.run_until_parked();
        let saved = root.update(cx, |root, cx| {
            root.save_navigation(cx);
            root.last_saved.clone().unwrap()
        });
        let restored: NavigationSession = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(restored.panes.len(), 2);
        assert_ne!(restored.panes[0].pane_id, restored.panes[1].pane_id);
        assert_eq!(restored.panes[0].route, Route::Library(LibraryRoute::Folders));
        assert_eq!(restored.panes[1].route, Route::Library(LibraryRoute::Authors));
        let (services, backend) = root.read_with(cx, |root, _| (root.services.clone(), root.backend.clone()));
        let reopened = cx.add_window(|window, cx| ApplicationRoot::new(services, WindowSession { initial: restored, ..Default::default() }, backend, "Restored", None, window, cx));
        reopened
            .read_with(cx, |root, cx| {
                assert_eq!(root.panes.len(), 2);
                let first = root.panes[0].view.read(cx).browser.read(cx).navigation_session(cx);
                let second = root.panes[1].view.read(cx).browser.read(cx).navigation_session(cx);
                assert_eq!(first.library_id, Some(ids[1]));
                assert_eq!(first.route, Route::Library(LibraryRoute::Folders));
                assert_eq!(second.library_id, Some(ids[0]));
                assert_eq!(second.route, Route::Library(LibraryRoute::Authors));
            })
            .unwrap();
    }

    #[gpui::test]
    async fn closing_one_reader_leaves_the_other_reader_open(cx: &mut TestAppContext) {
        let (_directory, window, root, ids) = fixture(cx).await;
        window
            .update(cx, |_, window, cx| {
                root.update(cx, |root, cx| {
                    for (index, library) in ids.into_iter().enumerate() {
                        root.panes[index].view.update(cx, |pane, cx| pane.open_reader_surface(BookLocator::new(library, BookId::from_bytes([index as u8 + 1; 32])), format!("Reader {index}"), None, window, cx));
                    }
                    let second = root.panes[1].view.read(cx).reader.as_ref().unwrap().view.entity_id();
                    root.panes[0].view.update(cx, |pane, cx| pane.return_to_library(window, cx));
                    assert!(root.panes[0].view.read(cx).reader.is_none());
                    assert_eq!(root.panes[1].view.read(cx).reader.as_ref().unwrap().view.entity_id(), second);
                    root.save_navigation(cx);
                    assert!(root.last_saved.as_ref().unwrap().panes[0].reader.is_none());
                    assert!(root.last_saved.as_ref().unwrap().panes[1].reader.is_some());
                    root.panes[1].view.update(cx, |pane, cx| pane.return_to_library(window, cx));
                })
            })
            .unwrap();
    }

    #[gpui::test]
    async fn panes_have_separate_players_and_docks(cx: &mut TestAppContext) {
        let (_directory, window, root, ids) = fixture(cx).await;
        window
            .update(cx, |_, window, cx| {
                root.update(cx, |root, cx| {
                    for (index, library) in ids.into_iter().enumerate() {
                        let locator = BookLocator::new(library, BookId::from_bytes([index as u8 + 1; 32]));
                        let metadata = library_model::PlaybackMetadata {
                            format: book_model::BookFormat::M4b,
                            title: format!("Audio {index}"),
                            author: String::new(),
                            narrator: None,
                            duration_ms: 60_000,
                            chapters: Vec::new(),
                            tracks: Vec::new(),
                            cover: Vec::new(),
                        };
                        let record = reader_ui::ActiveAudiobook::new(locator.clone(), 1000, 1.0, metadata);
                        let session = reader_ui::restore_audiobook(&record, root.backend.library(library), cx);
                        root.panes[index].view.update(cx, |pane, cx| {
                            pane.playback.update(cx, |controller, cx| controller.replace(locator, record.metadata.title, session, cx));
                            pane.sync_playback(window, cx);
                        });
                    }
                    let first = root.panes[0].view.read(cx);
                    let second = root.panes[1].view.read(cx);
                    assert_ne!(first.playback.entity_id(), second.playback.entity_id());
                    assert_ne!(first.audiobook.as_ref().unwrap().dock.entity_id(), second.audiobook.as_ref().unwrap().dock.entity_id());
                    let second_session = second.audiobook.as_ref().unwrap().session.entity_id();
                    root.panes[0].view.update(cx, |pane, cx| pane.close_audiobook(window, cx));
                    assert!(root.panes[0].view.read(cx).audiobook.is_none());
                    assert_eq!(root.panes[1].view.read(cx).audiobook.as_ref().unwrap().session.entity_id(), second_session);
                    root.close_pane(0, window, cx);
                    assert_eq!(root.panes.len(), 1);
                    assert_eq!(root.panes[0].view.read(cx).audiobook.as_ref().unwrap().session.entity_id(), second_session);
                })
            })
            .unwrap();
    }
}
