use crate::settings::{ReaderSettings, SidebarTab};
use std::time::Duration;
mod document;
mod input;
mod overlays;
mod search;
mod sidebar;
pub(super) mod touch_selection;

use crate::epub::{LoadState, ReaderView};
use crate::invalidation::Component;
use crate::shell::chrome;
use crate::shell::input::{self as shell_input, CommandTarget};
use crate::shell::page::ReaderSurface;
use crate::shell::toolbar::{self as sidebar_chrome, SidebarContent};
use crate::{DecreaseColumnWidth, DecreaseFont, IncreaseColumnWidth, IncreaseFont, NextLine, NextPage, NextSection, PreviousLine, PreviousPage, PreviousSection, ToggleFullscreen};
use gpui::prelude::*;
use gpui::{App, Context, Div, FocusHandle, Focusable, IntoElement, Pixels, Point, Render, ScrollDelta, Window};
use ui_components as components;

impl Focusable for ReaderView {
    /// Focusing the reader means focusing the document.
    ///
    /// Navigation keys are bound in the `ReaderDocument` context, which only
    /// the HTML view establishes, so anything that focuses this view has to
    /// land on the document or the keyboard goes dead. Answering with the
    /// document handle keeps that true for every caller instead of leaving a
    /// list of call sites to remember. Before the book is ready there
    /// is no document, so the page container stands in.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.document_focus(cx).unwrap_or_else(|| self.focus.clone())
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(all(target_os = "android", feature = "native-tts", not(feature = "kobo")))]
        self.tts.sync_volume_buttons(ReaderSettings::preferences(cx).reading_ruler != html_view_gpui::ReadingRulerStyle::Off, cx);
        if cfg!(target_os = "android") {
            self.install_ruler_hardware_keys(window, cx);
        }
        components::sync_reader_rem_size(window, cx);
        crate::invalidation::record_render(Component::ReaderShell);
        let theme = components::browser_theme(cx);
        if let LoadState::Ready(renderer) = &self.load_state {
            let selected = cx.try_global::<components::theme::SelectedPalette>().copied();
            let renderer_changed = self.last_column_viewport_plan.is_none_or(|plan| plan.0 != renderer.entity_id());
            if renderer_changed {
                // Preferences can change while the book is still preparing.
                let font_size = ReaderSettings::preferences(cx).font_size;
                renderer.update(cx, |view, cx| view.queue_appearance(skrift_column_paginator::RendererCommand::SetFontSize(font_size), cx));
            }
            if renderer_changed || self.applied_palette != selected {
                self.applied_palette = selected;
                let appearance = crate::settings::library_theme_adaptation(cx);
                let overrides = ReaderSettings::preferences(cx).style_overrides_for_book(self.cjk_font, Some(appearance), crate::settings::library_link_foreground(cx));
                let paint = crate::settings::library_paint_palette(cx);
                renderer.update(cx, |renderer, cx| {
                    renderer.queue_appearance(skrift_column_paginator::RendererCommand::SetReaderStyleOverrides(overrides), cx);
                    renderer.queue_appearance(skrift_column_paginator::RendererCommand::SetReaderPaintPalette(paint), cx);
                });
            }
        }
        let entity = cx.entity();
        let mobile = components::uses_mobile_reader_controls(window);
        if mobile {
            self.settings_navigation.keyboard_visibility().set(false);
        }
        let touch_input = cx.supports_touch_input() || mobile;
        let chrome_visible = self.chrome.is_visible();
        let active_tab = self.active_sidebar_tab(window);
        let sidebar_visible = chrome_visible && active_tab.is_some();
        // A saved Search panel starts searching when first opened; hiding
        // it afterwards preserves the query, matches, and highlights.
        if sidebar_visible && active_tab == Some(SidebarTab::Search) {
            self.open_search(cx);
        }

        let preferences = ReaderSettings::preferences(cx);
        let ruler_style = preferences.reading_ruler;
        let ruler_lines = preferences.reading_ruler_lines;
        let (split_sidebar_width, horizontal_margin, available_width) = Self::horizontal_layout(window, cx, sidebar_visible);
        let vertical_margin = preferences.vertical_margin_px.min(((f32::from(window.viewport_size().height) - 100.0) / 2.0).max(0.0));
        let max_columns = self.max_columns_for_gap(f64::from(available_width), cx);
        let fitted_column_width = self.fitted_column_width(f64::from(available_width), cx);
        let column_gap = f64::from(preferences.min_column_gap_px) / preferences.scale.max(0.1);
        if let LoadState::Ready(renderer) = &self.load_state {
            renderer.update(cx, |view, cx| {
                view.set_reading_ruler_accent(theme.accent, cx);
                view.set_reading_ruler(ruler_style, ruler_lines, cx);
            });
            let plan = (renderer.entity_id(), f64::from(available_width), fitted_column_width, max_columns, column_gap);
            if self.last_column_viewport_plan != Some(plan) {
                renderer.update(cx, |renderer, cx| {
                    renderer.queue_appearance(skrift_column_paginator::RendererCommand::SetColumnViewportPlan { available_width: plan.1, column_width: plan.2, column_count: plan.3, column_gap: plan.4 }, cx);
                });
                self.last_column_viewport_plan = Some(plan);
            }
        }
        let page_size = self.page_size.unwrap_or(window.viewport_size());
        let page_ready = matches!(&self.load_state, LoadState::Ready(renderer) if renderer.read(cx).reading_document().is_some());
        let turn_frame = self.page_turn.as_mut().and_then(|turn| turn.frame(page_size, page_ready, window, cx));
        if turn_frame.is_none() {
            self.page_turn = None;
        }
        let drag_offset = turn_frame.as_ref().map_or_else(|| self.swipe_preview.offset(window, cx), |frame| frame.incoming);
        let document =
            swipe_document(self.render_document(entity.clone(), horizontal_margin, vertical_margin, mobile, cx), drag_offset).bg(theme.page_bg).when(turn_frame.as_ref().is_some_and(|frame| !frame.ready), |document| document.invisible());

        let mut document_body = components::reader_document_body().id("epub-reader-document-surface");
        if let Some(frame) = &turn_frame
            && let Some(turn) = &self.page_turn
        {
            let outgoing = components::reader_content().px(gpui::px(horizontal_margin)).py(gpui::px(vertical_margin)).child(turn.outgoing.clone());
            document_body = document_body.child(gpui::div().absolute().inset_0().flex().flex_col().bg(theme.page_bg).transform(gpui::ElementTransform::default().translate(frame.outgoing)).child(outgoing));
        }
        let measured = entity.clone();
        document_body = document_body
            .child(document)
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        measured.update(cx, |reader, cx| {
                            if reader.page_size != Some(bounds.size) {
                                reader.page_size = Some(bounds.size);
                                if reader.page_turn.is_some() {
                                    cx.notify();
                                }
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_scroll_wheel(cx.listener(Self::handle_mouse_wheel));
        document_body = document_body.children(self.render_selection_actions(window, cx));
        if let Some(annotation) = self.render_annotation_popup(&entity, sidebar_visible, window, cx) {
            document_body = document_body.child(annotation);
        }
        if let Some(footnote) = self.render_footnote(cx) {
            document_body = document_body.child(footnote);
        }
        if let Some(image) = self.render_image_preview(&entity, split_sidebar_width, window, cx) {
            document_body = document_body.child(image);
        }

        #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
        let dock = self.tts.dock_open.then(|| crate::shell::tts::dock(&self.tts, mobile, theme, cx));
        #[cfg(not(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo"))))]
        let dock: Option<gpui::AnyElement> = None;
        let sidebar = active_tab.filter(|_| sidebar_visible).map(|active| {
            let body = self.render_sidebar(active, entity.clone(), theme, window, cx);
            let sidebar_content = SidebarContent::new(body);
            sidebar_chrome::reader_sidebar(self, &entity, sidebar_content, theme, window, cx)
        });

        // The page container is only focusable while the book is
        // loading, when it is the one thing that can hold the keyboard. A
        // focusable container steals focus from the document on any click that
        // reaches it — a margin, the sidebar's background — and navigation is
        // bound in the document's key context, so the paging keys would go
        // dead until the text was clicked again. Dropping the handle once the
        // document exists leaves clicks on the chrome harmless; the container
        // still sits on the dispatch path, so `Reader` bindings keep firing.
        let document_ready = self.document_focus(cx).is_some();
        self.chrome.sync_system_bars(chrome_visible, cx);
        let surface = ReaderSurface {
            document: document_body.into_any_element(),
            dock,
            sidebar,
            active_tab,
            chrome_visible,
            document_ready,
            loading_focus: self.focus.clone(),
            title: self.book_title.clone(),
            chapter: self.spine_title(),
            progress_label: self.progress.summary().label(),
            progress_fraction: f32::from(self.progress.summary().percent) / 100.0,
            progress_detail: self.progress.summary().tooltip(),
            bottom_bar_id: "epub-reader-bottom-bar",
        };
        let page = crate::shell::page::reader_surface(self, surface, theme, window, cx)
            .id("epub-reader-page")
            // Reading is pointer-first: merely returning to this window should
            // make its page keys available again. Activate at the platform
            // level as well as restoring the document's GPUI focus, so this
            // also takes keyboard input back from another application.
            .on_mouse_move(cx.listener(|reader: &mut Self, _, window, cx| reader.reclaim_hover_focus(window, cx)))
            .capture_any_touch(cx.listener(Self::handle_selection_touch))
            .on_action(cx.listener(shell_input::dispatch::<Self, PreviousSection>))
            .on_action(cx.listener(shell_input::dispatch::<Self, NextSection>))
            .on_action(cx.listener(shell_input::dispatch::<Self, IncreaseFont>))
            .on_action(cx.listener(shell_input::dispatch::<Self, DecreaseFont>))
            .on_action(cx.listener(shell_input::dispatch::<Self, IncreaseColumnWidth>))
            .on_action(cx.listener(shell_input::dispatch::<Self, DecreaseColumnWidth>))
            .on_action(cx.listener(shell_input::dispatch::<Self, ToggleFullscreen>));
        let interactions = chrome::surface_interactions("epub", sidebar_visible, touch_input, cx, |reader: &mut Self, action, window, cx| match action {
            chrome::SurfaceAction::PreviousPage => {
                reader.execute(&PreviousPage, window, cx);
            }
            chrome::SurfaceAction::NextPage => {
                reader.execute(&NextPage, window, cx);
            }
            chrome::SurfaceAction::ToggleChrome => reader.chrome.toggle(cx),
        });
        let mut page = page.children(interactions);
        if let Some(actions) = &self.context_sheet {
            let sheet = actions.sheet(theme, window.insets().safe_area.bottom, &entity);
            page = page.child(sheet);
        }
        page
    }
}

fn swipe_document(document: impl IntoElement, offset: Point<Pixels>) -> Div {
    // The document uses flex_1; a block wrapper leaves its percentage-height
    // HTML viewport without a definite height and clips all text away.
    gpui::div().size_full().flex().flex_col().transform(gpui::ElementTransform::default().translate(offset)).child(document)
}

/// Quiet period that ends a physical wheel burst.
const WHEEL_SCROLL_END_DELAY: Duration = Duration::from_millis(150);

impl ReaderView {
    fn reading_ruler_enabled(&self, cx: &App) -> bool {
        ReaderSettings::preferences(cx).reading_ruler != html_view_gpui::ReadingRulerStyle::Off
    }

    fn advance_from_gesture(&mut self, forward: bool, vertical: bool, window: &mut Window, cx: &mut Context<Self>) {
        if vertical && self.reading_ruler_enabled(cx) {
            if forward {
                self.execute(&NextLine, window, cx);
            } else {
                self.execute(&PreviousLine, window, cx);
            }
        } else if forward {
            self.execute(&NextPage, window, cx);
        } else {
            self.execute(&PreviousPage, window, cx);
        }
    }

    fn install_ruler_hardware_keys(&self, window: &mut Window, cx: &mut Context<Self>) {
        let target = cx.entity().downgrade();
        let handle = window.window_handle();
        window.use_keyed_state(("epub-ruler-hardware", cx.entity_id()), cx, move |_, cx| {
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != handle || !event.context_stack.iter().any(|context| context.contains(DOCUMENT_CONTEXT)) {
                    return;
                }
                if let Some(target) = target.upgrade() {
                    target.update(cx, |reader, cx| reader.handle_ruler_hardware_key(&event.keystroke, window, cx));
                }
            })
        });
    }

    /// Android's captured volume keys arrive as Page Up/Down. Only take them
    /// while the document has focus, leaving inputs and settings in control.
    fn handle_ruler_hardware_key(&mut self, key: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(target_os = "android")
            || !self.reading_ruler_enabled(cx)
            || self.document_focus(cx).is_none_or(|focus| !focus.is_focused(window))
            || key.modifiers.control
            || key.modifiers.alt
            || key.modifiers.platform
            || key.modifiers.shift
        {
            return;
        }
        match key.key.as_str() {
            "pagedown" => self.execute(&NextLine, window, cx),
            "pageup" => self.execute(&PreviousLine, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Reclaim document focus on hover without interrupting sidebar keyboard input.
    fn reclaim_hover_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_focus.contains_focused(window, cx) || self.rail_focus.contains_focused(window, cx) || self.marks.selected().is_some() {
            return;
        }
        window.activate_window();
        self.focus_document(window, cx);
    }

    /// Vertical scrolling advances the ruler when enabled. Horizontal gestures
    /// and scrolling with the ruler disabled turn whole pages.
    ///
    /// Android touch swipes commit on their end event. Physical wheel bursts
    /// use a quiet period; touchpads advance once per gesture after sufficient movement.
    /// Scrolling down goes forward, the same direction as vertical touch swipes.
    fn handle_mouse_wheel(&mut self, event: &gpui::ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.touch_selection.active {
            cx.stop_propagation();
            return;
        }
        if let ScrollDelta::Pages(delta) = event.delta {
            let direction = if delta.y != 0.0 { delta.y } else { delta.x };
            self.wheel_delta = 0.0;
            self.wheel_debounce.take();
            self.touch_swipe = None;
            self.touch_swipe_from_edge = false;
            if direction == 0.0 {
                self.swipe_preview.settle();
            }
            cx.notify();
            if direction.is_finite() && direction != 0.0 {
                self.advance_from_gesture(direction < 0.0, delta.y.abs() >= delta.x.abs(), window, cx);
            }
            cx.stop_propagation();
            return;
        }
        let delta = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y),
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pages(delta) => {
                if delta.y != 0.0 {
                    delta.y
                } else {
                    delta.x
                }
            }
        };

        if cfg!(target_os = "android") || window.is_page_scroll_swipe() || self.touch_swipe.is_some() {
            let travel = match event.delta {
                ScrollDelta::Pixels(delta) => gpui::point(f32::from(delta.x), f32::from(delta.y)),
                ScrollDelta::Lines(delta) | ScrollDelta::Pages(delta) => gpui::point(delta.x, delta.y),
            };
            let travel = if travel.x.is_finite() && travel.y.is_finite() { travel } else { gpui::point(0.0, 0.0) };
            match event.touch_phase {
                gpui::TouchPhase::Started => {
                    self.page_turn = None;
                    self.wheel_debounce.take();
                    self.touch_swipe = Some(travel);
                    self.touch_swipe_from_edge = components::in_bottom_gesture_edge(event.position, window);
                    if !self.touch_swipe_from_edge && (!self.reading_ruler_enabled(cx) || travel.x.abs() > travel.y.abs()) {
                        if self.swipe_preview.drag_in_viewport(travel, self.page_size.unwrap_or(window.viewport_size())) {
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                gpui::TouchPhase::Moved if self.touch_swipe.is_some() => {
                    let ruler_enabled = self.reading_ruler_enabled(cx);
                    if let Some(swipe) = &mut self.touch_swipe {
                        *swipe = gpui::point(swipe.x + travel.x, swipe.y + travel.y);
                        if !self.touch_swipe_from_edge {
                            let preview = if ruler_enabled && swipe.y.abs() >= swipe.x.abs() { gpui::point(0.0, 0.0) } else { *swipe };
                            if self.swipe_preview.drag_in_viewport(preview, self.page_size.unwrap_or(window.viewport_size())) {
                                cx.notify();
                            }
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                gpui::TouchPhase::Ended if self.touch_swipe.is_some() => {
                    let Some(swipe) = self.touch_swipe.take() else { return };
                    // A swipe from the bottom edge is the system's.
                    let from_edge = std::mem::take(&mut self.touch_swipe_from_edge);
                    match components::swipe_turns_forward(swipe, window).filter(|_| !from_edge) {
                        Some(forward) => {
                            self.advance_from_gesture(forward, swipe.y.abs() >= swipe.x.abs(), window, cx);
                        }
                        None => self.swipe_preview.settle(),
                    }
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                gpui::TouchPhase::Cancelled if self.touch_swipe.is_some() => {
                    self.touch_swipe = None;
                    self.touch_swipe_from_edge = false;
                    self.swipe_preview.settle();
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        if matches!(event.delta, ScrollDelta::Pixels(_)) || self.touchpad.is_active() || matches!(event.delta, ScrollDelta::Lines(delta) if delta.x.abs() > delta.y.abs()) {
            self.wheel_delta = 0.0;
            self.wheel_debounce.take();
            if let Some(travel) = self.touchpad.update(event) {
                let forward = if travel.y != 0.0 { travel.y < 0.0 } else { travel.x < 0.0 };
                self.advance_from_gesture(forward, travel.y != 0.0, window, cx);
            }
            cx.stop_propagation();
            return;
        }

        if !delta.is_finite() || delta == 0.0 {
            return;
        }
        self.wheel_delta += delta;
        self.wheel_debounce.take();
        let timer = cx.background_executor().timer(WHEEL_SCROLL_END_DELAY);
        self.wheel_debounce = Some(cx.spawn_in(window, async move |reader, cx| {
            timer.await;
            let _ = reader.update_in(cx, |reader, window, cx| {
                reader.wheel_debounce.take();
                let delta = std::mem::take(&mut reader.wheel_delta);
                if delta != 0.0 {
                    reader.advance_from_gesture(delta < 0.0, true, window, cx);
                }
            });
        }));
        cx.stop_propagation();
    }
}

/// The key context the HTML document element establishes while it holds focus.
///
/// Navigation binds here rather than to `Reader` so that page keys — `space`
/// above all — never fire while the search input has focus.
const DOCUMENT_CONTEXT: &str = "ReaderDocument";

pub(crate) fn bind_keys(cx: &mut App) {
    shell_input::bind_document_keys(cx, DOCUMENT_CONTEXT);
    use ui_components::{ShortcutPage::Reader, bind_shortcut};
    bind_shortcut(cx, Reader, "reader.previous_section", "Previous section", &["secondary-left"], PreviousSection, DOCUMENT_CONTEXT);
    bind_shortcut(cx, Reader, "reader.next_section", "Next section", &["secondary-right"], NextSection, DOCUMENT_CONTEXT);
    #[cfg(not(target_os = "macos"))]
    bind_shortcut(cx, Reader, "reader.fullscreen", "Toggle fullscreen", &["f11"], ToggleFullscreen, "Reader");
    #[cfg(target_os = "macos")]
    bind_shortcut(cx, Reader, "reader.fullscreen", "Toggle fullscreen", &["ctrl-cmd-f"], ToggleFullscreen, "Reader");
    bind_shortcut(cx, Reader, "reader.font_increase", "Increase book font size", &["alt-="], IncreaseFont, DOCUMENT_CONTEXT);
    bind_shortcut(cx, Reader, "reader.font_decrease", "Decrease book font size", &["alt--"], DecreaseFont, DOCUMENT_CONTEXT);
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use gpui::{TestAppContext, point, px};
    use skrift_column_paginator::{RendererInitialConfig, ResourceProvider};
    use html_view_gpui::HtmlView;
    use std::{io, sync::Arc};

    struct DocumentLayout;

    impl Render for DocumentLayout {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let document = components::reader_content().px(px(26.0)).py(px(24.0)).child(gpui::div().size_full().debug_selector(|| "epub-document-viewport".into()));
            components::reader_document_body().child(swipe_document(document, point(px(0.0), px(0.0))))
        }
    }

    #[gpui::test]
    fn swipe_wrapper_preserves_document_viewport_in_both_orientations(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| DocumentLayout);
        for (width, height) in [(632.0, 840.0), (840.0, 632.0)] {
            cx.simulate_resize(gpui::size(px(width), px(height)));
            cx.run_until_parked();
            let viewport = cx.debug_bounds("epub-document-viewport").unwrap();
            assert_eq!(viewport.size.width, px(width - 52.0));
            assert_eq!(viewport.size.height, px(height - 48.0));
        }
    }
    struct PageTurnBook;

    impl ResourceProvider for PageTurnBook {
        fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
            if uri != "book.html" {
                return Err(io::ErrorKind::NotFound.into());
            }
            let paragraphs = (0..24).map(|index| format!("<p>Paragraph {index} for page navigation.</p>")).collect::<String>();
            Ok(format!("<html><head><style>body {{ margin: 0 }} p {{ height: 180px; margin: 0 }}</style></head><body>{paragraphs}</body></html>").into_bytes())
        }
        fn exists(&self, uri: &str) -> bool {
            uri == "book.html"
        }
        fn resolve(&self, _: &str, href: &str) -> String {
            href.to_owned()
        }
        fn list_html_candidates(&self, _: &str) -> io::Result<Vec<String>> {
            Ok(vec!["book.html".into()])
        }
    }

    #[gpui::test]
    fn epub_turns_prepare_each_destination_and_preserve_the_outgoing_page(cx: &mut TestAppContext) {
        let (document, visual) = cx.add_window_view(|window, cx| HtmlView::from_provider(Arc::new(PageTurnBook), vec!["book.html".into()], 0, None, RendererInitialConfig { max_column_count: Some(1), ..Default::default() }, window, cx));
        visual.simulate_resize(gpui::size(px(600.0), px(800.0)));
        visual.run_until_parked();
        document.update(visual, |document, cx| {
            let first = document.current_cfi().expect("initial reading position");
            let outgoing = document.page_snapshot().expect("retained displayed page");
            let before = document.paint_cache_stats();
            let _another_snapshot = document.page_snapshot().unwrap();
            assert_eq!(document.paint_cache_stats(), before, "capturing the outgoing page must not run layout or build paint commands");
            assert!(!document.previous_page(cx), "the first page must reject a backward turn");
            assert!(document.next_page(cx));
            let second = document.current_cfi();
            assert_ne!(second.as_deref(), Some(first.as_str()));
            assert!(document.next_page(cx), "a second request before a frame still advances to the next page");
            assert_ne!(document.current_cfi(), second);
            assert!(document.previous_page(cx));
            assert_eq!(document.current_cfi(), second);
            assert!(document.previous_page(cx));
            assert_eq!(document.current_cfi().as_deref(), Some(first.as_str()));
            drop(outgoing);
        });
    }
}
