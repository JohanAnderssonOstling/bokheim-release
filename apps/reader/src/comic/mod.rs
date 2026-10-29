//! Image-only fixed-page reader. Archive access and rendering remain separate.

mod controls;
mod image_decode;
mod layout;
#[cfg(all(test, not(feature = "kobo"), not(target_arch = "wasm32")))]
mod tests;

#[cfg(feature = "kobo")]
use gpui::AnyView;

use book_content::comic::ComicReader;
use fixed_page::{DISPLAY_CACHE_BUDGET, PageCache, PageImage, PageLayout, PageSwipe, PageZoom as Zoom};
use gpui::{App, Bounds, Context, FocusHandle, Focusable, MouseButton, PinchEvent, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, Task, TouchPhase, Window, div, img, prelude::*, px};
use gpui_component::ElementExt;
use library_backend::LibraryClient;
use library_model::BookLocator;
use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};
use ui_components as components;

use layout::{ComicLayout, MAX_COLUMNS};

use crate::invalidation::Component;
use crate::settings::ReaderSettings;
use crate::shell::{
    chrome,
    page::ReaderSurface,
    persistence::{ReadingPositionWriter, StoredPosition},
    settings_navigation::SettingsNavigation,
    state_panel::{SidebarState, SidebarTab},
    toolbar::{self, SidebarContent, SidebarTarget},
};

pub(crate) struct ComicReaderView {
    reader: ComicReader,
    title: String,
    current: usize,
    columns: usize,
    page_gap: f32,
    right_to_left: bool,
    zoom: Zoom,
    offset: f32,
    pan_x: f32,
    viewport: Bounds<Pixels>,
    /// Pointer origin, absolute flow position, and horizontal pan at drag start.
    drag: Option<(Point<Pixels>, f32, f32)>,
    dragged: bool,
    swipe: PageSwipe,
    touchpad: components::TouchpadNavigation,
    pages: PageCache<usize, PageImage>,
    errors: HashMap<usize, String>,
    load_task: Option<Task<()>>,
    requested_pages: VecDeque<usize>,
    loading_page: Option<usize>,
    page_sizes: HashMap<usize, (u32, u32)>,
    positions: ReadingPositionWriter,
    navigation_revision: u64,
    sidebar: SidebarState,
    chrome: chrome::ChromeState,
    focus: FocusHandle,
    sidebar_focus: FocusHandle,
    rail_focus: FocusHandle,
    settings_navigation: SettingsNavigation,
    close_reader: crate::CloseReader,
    #[cfg(feature = "kobo")]
    device_controls: Option<AnyView>,
}

impl Focusable for ComicReaderView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ComicReaderView {
    pub(crate) fn new(
        locator: BookLocator, title: String, reader: ComicReader, initial_page: usize, offset: f32, _app: app::AppClient, library: Rc<LibraryClient>, close_reader: crate::CloseReader, #[cfg(feature = "kobo")] device_controls: Option<AnyView>, window: &mut Window, cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let current = initial_page.min(reader.pages.len().saturating_sub(1));
        let mut sidebar = SidebarState::from_preferences(cx);
        if sidebar.is_visible() && !matches!(sidebar.effective_or_default(true), SidebarTab::Contents | SidebarTab::Settings) {
            sidebar.show(SidebarTab::Contents);
        }
        let mut view = Self {
            right_to_left: reader.right_to_left,
            reader,
            title,
            current,
            columns: 1,
            page_gap: ReaderSettings::preferences(cx).comic_page_gap_px,
            zoom: if offset > 0.0 { Zoom::FitWidth } else { Zoom::FitPage },
            offset,
            pan_x: 0.0,
            viewport: Bounds::default(),
            drag: None,
            dragged: false,
            swipe: PageSwipe::default(),
            touchpad: components::TouchpadNavigation::default(),
            pages: PageCache::new(DISPLAY_CACHE_BUDGET),
            errors: HashMap::new(),
            load_task: None,
            requested_pages: VecDeque::new(),
            loading_page: None,
            page_sizes: HashMap::new(),
            positions: ReadingPositionWriter::new(cx, locator.content_hash(), library),
            navigation_revision: 0,
            sidebar,
            chrome: chrome::ChromeState::from_preferences(Component::ComicShell, cx),
            focus,
            sidebar_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            rail_focus: cx.focus_handle().tab_stop(true),
            settings_navigation: SettingsNavigation::new(window, cx),
            close_reader,
            #[cfg(feature = "kobo")]
            device_controls,
        };
        view.request_pages(cx);
        view
    }

    fn flow(&self) -> ComicLayout {
        ComicLayout::new(&self.reader.pages, self.columns, (f32::from(self.viewport.size.width), f32::from(self.viewport.size.height)), self.zoom, self.page_gap, |index| self.page_sizes.get(&index).copied())
    }

    fn request_pages(&mut self, cx: &mut Context<Self>) {
        let flow = self.flow();
        let top = flow.scroll_top(self.current, self.offset);
        self.requested_pages = flow.requested_pages(top).into_iter().filter(|index| !self.pages.contains(*index) && !self.errors.contains_key(index) && self.loading_page != Some(*index)).collect();
        if self.load_task.is_some() || self.requested_pages.is_empty() {
            return;
        }
        self.load_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let next = this.update(cx, |view, _| {
                    let next = view.requested_pages.pop_front();
                    view.loading_page = next;
                    if next.is_none() {
                        view.load_task = None;
                    }
                    next.map(|index| (index, view.reader.clone()))
                });
                let Ok(Some((index, reader))) = next else { return };
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let bytes = reader.read_page(index).await.map_err(|error| error.to_string())?;
                        image_decode::decode(bytes).map(|page| PageImage::from_bgra(page.pixels).with_source_size(page.width, page.height))
                    })
                    .await;
                if this
                    .update(cx, |view, cx| {
                        view.loading_page = None;
                        match result {
                            Ok(page) => {
                                gpui_image_eviction::track_image(&page.image, cx);
                                view.page_sizes.insert(index, (page.width, page.height));
                                let flow = view.flow();
                                let visible = flow.visible_pages(flow.scroll_top(view.current, view.offset));
                                let bytes = page.byte_len;
                                view.pages.insert_protected(index, Arc::new(page), bytes, |key| visible.contains(&key));
                            }
                            Err(error) => {
                                view.errors.insert(index, error);
                            }
                        }
                        view.clamp_position();
                        view.request_pages(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn evict_pages(&mut self) {
        let flow = self.flow();
        let visible = flow.visible_pages(flow.scroll_top(self.current, self.offset));
        self.pages.trim(|key| visible.contains(&key));
    }

    fn layout(&self, index: usize) -> Option<PageLayout> {
        let size = self.page_sizes.get(&index).copied()?;
        let flow = self.flow();
        let row = flow.rows[flow.row_for_page(index)];
        Some(PageLayout::new(size, (row.cell_width, f32::from(self.viewport.size.height)), self.zoom, 0.0, self.pan_x))
    }

    fn record_position(&self) {
        let flow = self.flow();
        let visible = flow.visible_pages(flow.scroll_top(self.current, self.offset));
        // Whole-page mode resumes at the current page. A nonzero saved offset
        // denotes fit-width mode in the existing comic position format.
        let offset = if self.zoom == Zoom::FitPage { 0.0 } else { self.offset };
        self.positions.record(StoredPosition::comic_page(self.current, offset, self.reader.pages.len(), visible.len()), self.navigation_revision);
    }

    fn max_pan(&self, flow: &ComicLayout, top: f32) -> f32 {
        flow.visible_pages(top)
            .filter_map(|index| {
                let size = self.page_sizes.get(&index).copied()?;
                let cell_width = flow.rows[flow.row_for_page(index)].cell_width;
                Some((PageLayout::new(size, (cell_width, f32::from(self.viewport.size.height)), self.zoom, 0.0, 0.0).width - cell_width).max(0.0))
            })
            .fold(0.0, f32::max)
    }

    fn clamp_position(&mut self) {
        let flow = self.flow();
        let top = flow.scroll_top(self.current, self.offset);
        (self.current, self.offset) = flow.anchor(top);
        self.pan_x = self.pan_x.clamp(0.0, self.max_pan(&flow, top));
    }

    fn move_viewport(&mut self, top: f32, pan_x: f32, cx: &mut Context<Self>) {
        let flow = self.flow();
        let top = top.clamp(0.0, flow.max_scroll);
        let (current, offset) = flow.anchor(top);
        let pan_x = pan_x.clamp(0.0, self.max_pan(&flow, top));
        if (self.current, self.offset, self.pan_x) == (current, offset, pan_x) {
            return;
        }
        if self.current != current {
            self.navigation_revision += 1;
        }
        self.current = current;
        self.offset = offset;
        self.pan_x = pan_x;
        self.record_position();
        self.evict_pages();
        self.request_pages(cx);
        cx.notify();
    }

    fn go_to(&mut self, page: usize, cx: &mut Context<Self>) {
        let flow = self.flow();
        let page = page.min(self.reader.pages.len().saturating_sub(1));
        self.move_viewport(flow.rows[flow.row_for_page(page)].top, 0.0, cx);
    }

    fn turn(&mut self, direction: i32, cx: &mut Context<Self>) {
        let flow = self.flow();
        self.move_viewport(flow.scroll_top(self.current, self.offset) + direction as f32 * flow.viewport_height, self.pan_x, cx);
    }

    fn scroll(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let flow = self.flow();
        self.move_viewport(flow.scroll_top(self.current, self.offset) - dy, self.pan_x - dx, cx);
    }

    fn adjust_columns(&mut self, delta: isize, cx: &mut Context<Self>) {
        let next = self.columns.saturating_add_signed(delta).clamp(1, MAX_COLUMNS);
        if next != self.columns {
            self.columns = next;
            self.pan_x = 0.0;
            self.clamp_position();
            self.record_position();
            self.evict_pages();
            self.request_pages(cx);
            cx.notify();
        }
    }

    fn adjust_zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        if !factor.is_finite() || factor <= 0.0 || factor == 1.0 {
            return;
        }
        if let Some(layout) = self.layout(self.current) {
            self.zoom = Zoom::Fixed((layout.scale * factor).clamp(0.01, 16.0));
            self.clamp_position();
            self.record_position();
            self.request_pages(cx);
            cx.notify();
        }
    }

    fn finish_swipe(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some((dx, dy)) = self.swipe.finish() {
            if let Some(forward) = components::swipe_turns_forward_with_direction(gpui::point(dx, dy), window, self.right_to_left) {
                self.turn(if forward { 1 } else { -1 }, cx);
            }
        }
    }

    pub(crate) fn native_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar.is_visible() && self.chrome.is_visible() {
            self.hide_sidebar(window, cx);
        } else {
            self.close_reader(window, cx);
        }
    }
}

impl Render for ComicReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        components::sync_reader_rem_size(window, cx);
        crate::invalidation::record_render(Component::ComicShell);
        let theme = components::browser_theme(cx);
        let overlay = chrome::sidebar_overlays_document(window);
        let active_tab = if overlay { self.sidebar.effective(true) } else { Some(self.sidebar_tab()) };
        let active_tab = active_tab.map(|tab| if matches!(tab, SidebarTab::Contents | SidebarTab::Settings) { tab } else { SidebarTab::Contents });
        let visible = self.chrome.is_visible();
        let active_tab = active_tab.filter(|_| visible);
        self.chrome.sync_system_bars(visible, cx);
        let preferences = ReaderSettings::preferences(cx);
        let available_width = f32::from(window.viewport_size().width) - chrome::sidebar_width(active_tab.is_some(), window, cx);
        let horizontal_margin = preferences.comic_horizontal_margin_px.min(((available_width - 100.0) / 2.0).max(0.0));
        let vertical_margin = preferences.comic_vertical_margin_px.min(((f32::from(window.viewport_size().height) - 100.0) / 2.0).max(0.0));
        if self.page_gap != preferences.comic_page_gap_px {
            self.page_gap = preferences.comic_page_gap_px;
            self.clamp_position();
            self.request_pages(cx);
        }
        let entity = cx.entity();
        let mut document = div().id("comic-document").debug_selector(|| "comic-document".into()).size_full().relative().overflow_hidden().bg(gpui::rgb(0x242424)).track_focus(&self.focus).key_context("ComicReaderDocument");
        let flow = self.flow();
        let scroll_top = flow.scroll_top(self.current, self.offset);
        let visible_pages = flow.visible_pages(scroll_top);
        for row_index in flow.visible_rows(scroll_top) {
            let row = flow.rows[row_index];
            let cell_width = row.cell_width;
            for index in row.first..row.first + row.count {
                let slot = if self.right_to_left { row.count - 1 - (index - row.first) } else { index - row.first };
                let cell = div().debug_selector(move || format!("comic-page-{index}")).absolute().left(px(slot as f32 * (cell_width + row.gap))).top(px(row.top - scroll_top)).w(px(cell_width)).h(px(row.height)).overflow_hidden();
                if let Some(page) = self.pages.get(index) {
                    let layout = PageLayout::new((page.width, page.height), (cell_width, f32::from(self.viewport.size.height)), self.zoom, 0.0, self.pan_x);
                    let top = (row.height - layout.height).max(0.0) * 0.5;
                    document = document.child(cell.child(img(page.image.clone()).absolute().left(px(layout.left)).top(px(top)).w(px(layout.width)).h(px(layout.height)).bg(gpui::rgb(0xffffff))));
                } else {
                    let message = self.errors.get(&index).map(|error| format!("Could not display page {}: {error}", index + 1)).unwrap_or_else(|| format!("Loading page {}…", index + 1));
                    document = document.child(cell.flex().items_center().justify_center().text_color(gpui::rgb(0xffffff)).child(message).when(self.errors.contains_key(&index), |body| {
                        body.child(components::outlined_text_button(("comic-retry", index), "Retry").on_click(cx.listener(move |view, _, _, cx| {
                            view.errors.remove(&index);
                            view.request_pages(cx);
                        })))
                    }));
                }
            }
        }
        let measure_target = entity.clone();
        let document = document
            .on_prepaint(move |bounds, _, cx| {
                measure_target.update(cx, |view, cx| {
                    if view.viewport != bounds {
                        let previous = (view.current, view.offset, view.pan_x);
                        view.viewport = bounds;
                        view.clamp_position();
                        if previous != (view.current, view.offset, view.pan_x) {
                            view.record_position();
                        }
                        view.request_pages(cx);
                        cx.notify();
                    }
                });
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                    view.focus.focus(window, cx);
                    view.drag = Some((event.position, view.flow().scroll_top(view.current, view.offset), view.pan_x));
                    view.dragged = false;
                }),
            )
            .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, window, cx| {
                if !components::uses_mobile_reader_controls(window) && !view.focus.is_focused(window) {
                    view.focus.focus(window, cx);
                }
                let Some((start, top, pan)) = view.drag else { return };
                if !event.dragging() {
                    return;
                }
                let (dx, dy) = (f32::from(event.position.x - start.x), f32::from(event.position.y - start.y));
                if dx.abs() + dy.abs() < 6.0 && !view.dragged {
                    return;
                }
                view.dragged = true;
                view.move_viewport(top - dy, pan - dx, cx);
                cx.stop_propagation();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.drag = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, _| {
                    view.drag = None;
                }),
            )
            .on_click(cx.listener(|view, event: &gpui::ClickEvent, _, cx| {
                if view.dragged {
                    view.dragged = false;
                    return;
                }
                let fraction = f32::from(event.position().x - view.viewport.origin.x) / f32::from(view.viewport.size.width).max(1.0);
                if fraction < 0.25 || fraction > 0.75 {
                    let direction = if (fraction < 0.25) != view.right_to_left { -1 } else { 1 };
                    view.turn(direction, cx);
                }
            }))
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                let (dx, dy) = match event.delta {
                    ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
                    ScrollDelta::Lines(delta) => (delta.x * 32.0, delta.y * 32.0),
                    ScrollDelta::Pages(delta) => (delta.x * f32::from(view.viewport.size.width), delta.y * f32::from(view.viewport.size.height)),
                };
                if matches!(event.delta, ScrollDelta::Pages(_)) {
                    view.swipe.begin(false);
                    view.swipe.push(dx, dy);
                    view.finish_swipe(window, cx);
                } else if window.is_page_scroll_swipe() {
                    if matches!(event.touch_phase, TouchPhase::Started) {
                        view.swipe.begin(components::in_bottom_gesture_edge(event.position, window));
                    }
                    if matches!(event.touch_phase, TouchPhase::Cancelled) {
                        view.swipe.cancel();
                    }
                    if view.swipe.push(dx, dy).is_some() {
                        let previous = (view.current, view.offset, view.pan_x);
                        view.scroll(dx, dy, cx);
                        if previous != (view.current, view.offset, view.pan_x) {
                            view.swipe.mark_panned();
                        }
                    }
                    if matches!(event.touch_phase, TouchPhase::Ended) {
                        view.finish_swipe(window, cx);
                    }
                } else if matches!(event.delta, ScrollDelta::Pixels(_)) || view.touchpad.is_active() || matches!(event.delta, ScrollDelta::Lines(delta) if delta.x.abs() > delta.y.abs()) {
                    let turn = view.touchpad.update(event);
                    let previous = (view.current, view.offset, view.pan_x);
                    view.scroll(dx, dy, cx);
                    if previous != (view.current, view.offset, view.pan_x) {
                        view.touchpad.consume();
                    } else if let Some(travel) = turn {
                        let forward = if travel.x != 0.0 { (travel.x < 0.0) != view.right_to_left } else { travel.y < 0.0 };
                        view.turn(if forward { 1 } else { -1 }, cx);
                    }
                } else {
                    view.scroll(dx, dy, cx);
                }
                view.chrome.hide_for_motion(window, cx);
                cx.stop_propagation();
            }))
            .on_pinch(cx.listener(|view, event: &PinchEvent, window, cx| {
                view.swipe.cancel();
                view.touchpad.consume();
                if event.phase != TouchPhase::Cancelled {
                    view.adjust_zoom(1.0 + event.delta, cx);
                    view.chrome.hide_for_motion(window, cx);
                }
                cx.stop_propagation();
            }));
        let count = self.reader.pages.len();
        let percent = book_model::pdf_page_progress(visible_pages.end - 1, count);
        let progress_label = format!("{percent:.0}%");
        let sidebar = active_tab.map(|tab| {
            let body = self.render_panel(tab, window, cx);
            let content = SidebarContent::new(body);
            toolbar::reader_sidebar(self, &entity, content, theme, window, cx)
        });
        let surface = ReaderSurface {
            document: components::reader_document_body().px(px(horizontal_margin)).py(px(vertical_margin)).bg(gpui::rgb(0x242424)).child(document).into_any_element(),
            dock: None,
            sidebar,
            active_tab,
            chrome_visible: visible,
            document_ready: true,
            loading_focus: self.focus.clone(),
            title: self.title.clone(),
            chapter: format!("Page {} of {count}", self.current + 1),
            progress_label,
            progress_fraction: percent / 100.0,
            progress_detail: format!("{}/{count}", self.current + 1),
            bottom_bar_id: "comic-reader-bottom-bar",
        };
        let touch = cx.supports_touch_input() || components::uses_mobile_reader_controls(window);
        let interactions = chrome::surface_interactions("comic", active_tab.is_some(), touch, cx, |view: &mut Self, action, _, cx| match action {
            chrome::SurfaceAction::PreviousPage => view.turn(-1, cx),
            chrome::SurfaceAction::NextPage => view.turn(1, cx),
            chrome::SurfaceAction::ToggleChrome => {}
        });
        crate::shell::page::reader_surface(self, surface, theme, window, cx).children(interactions)
    }
}
