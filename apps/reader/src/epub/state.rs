mod annotations;
mod appearance;
mod events;
mod navigation;
mod search;

use std::{rc::Rc, sync::Arc};
use web_time::Instant;

use gpui::prelude::*;
use gpui::{App, Context, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::tree::TreeState;
use html_view_core::{RendererCommand, RendererEvent, RendererInitialConfig};
use html_view_gpui::{DocumentTap, HtmlView};
use library_backend::LibraryClient;
use library_model::BookLocator;

use crate::epub::toc::TocState;
use crate::epub::{LoadState, ProgressState, ReaderView, SearchState};
use crate::invalidation::Component;
use crate::settings::{ReaderSettings, library_interaction_palette, library_paint_palette, library_theme_adaptation};
use crate::shell::marks::MarkSession;
use crate::shell::persistence::ReadingPositionWriter;
use crate::shell::search::SearchBarTarget;
use crate::shell::state_panel::SidebarState;
use app_preferences::LINE_WIDTH_MAX;

pub(crate) fn renderer_initial_config(cx: &App) -> RendererInitialConfig {
    let preferences = ReaderSettings::preferences(cx).clone();
    // A phone owns its physical reading width. Start with one column and let
    // the reader derive its width from the live viewport rather than carrying
    // a desktop line-width preference into a narrow screen.
    let column_width = if cfg!(any(feature = "kobo", target_os = "android")) { LINE_WIDTH_MAX } else { preferences.column_width };
    RendererInitialConfig {
        font_size: preferences.font_size,
        column_width,
        scale: preferences.scale,
        max_column_count: Some(if cfg!(any(feature = "kobo", target_os = "android")) { 1 } else { preferences.max_column_count }),
        style_overrides: preferences.style_overrides(Some(library_theme_adaptation(cx))),
        paint_palette: library_paint_palette(cx),
        interaction_palette: library_interaction_palette(cx),
        ..Default::default()
    }
}

impl ReaderView {
    pub(crate) fn new(
        locator: BookLocator, title: String, _app: app::AppClient, library: Rc<LibraryClient>, book: Arc<crate::book::NativeBook>, preparation: html_view_core::RendererPreparation, close_reader: crate::CloseReader, window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reader_started = Instant::now();
        startup_phase!("phase=reader_view_begin");
        let search_input = cx.new(|cx| InputState::new(window, cx));
        let search_subscription = cx.subscribe_in(&search_input, window, |this, input, event: &InputEvent, _window, cx| match event {
            InputEvent::Change => {
                let query = input.read(cx).value().to_string();
                this.queue_search(query, cx);
            }
            InputEvent::PressEnter { shift, .. } if !this.apply_pending_search(cx) => {
                this.search_navigate(if *shift { -1 } else { 1 }, cx);
            }
            _ => {}
        });
        let annotation_note_input = cx.new(|cx| crate::shell::annotation_editor::note_input(window, cx));
        let focus = cx.focus_handle();
        let toc_tree = cx.new(|cx| TreeState::new(cx));
        focus.focus(window, cx);
        let reading_positions = ReadingPositionWriter::new(cx, locator.content_hash(), library.clone());
        let marks = MarkSession::new(cx, library.clone());

        let mut reader = Self {
            #[cfg(feature = "kobo")]
            brightness: cx.new(|cx| crate::shell::brightness::BrightnessControls::new(_app.clone(), window, cx)),
            locator,
            library,
            book_title: title.clone(),
            title,
            load_state: LoadState::Loading,
            applied_palette: None,
            desired_column_count: if cfg!(feature = "kobo") { 1 } else { u8::MAX },
            toc: TocState::default(),
            toc_tree,
            sidebar: SidebarState::from_preferences(cx),
            chrome: crate::shell::chrome::ChromeState::new(Component::ReaderShell),
            wheel_delta: 0.0,
            wheel_debounce: None,
            touch_swipe_active: false,
            search_input,
            search: SearchState::default(),
            book,
            current_cfi: None,
            progress: ProgressState::default(),
            marks,
            selection: None,
            annotation_note_input,
            annotation_popup_anchor: None,
            footnote: None,
            image_preview: None,
            context_sheet: None,
            focus,
            renderer_subscription: None,
            document_tap_subscription: None,
            reading_positions,
            close_reader,
            _search_subscription: search_subscription,
        };
        reader.finish_load(preparation, window, cx);
        startup_phase!("phase=reader_view_ready phase_ms={}", reader_started.elapsed().as_millis());
        reader
    }

    fn finish_load(&mut self, preparation: html_view_core::RendererPreparation, window: &mut Window, cx: &mut Context<Self>) {
        let finish_started = Instant::now();
        startup_phase!("phase=finish_load_begin");
        let renderer_started = Instant::now();
        let renderer = match HtmlView::create_from_preparation(preparation, window, cx) {
            Ok(renderer) => renderer,
            Err(error) => {
                log::error!("could not initialize EPUB renderer: {error}");
                self.load_state = LoadState::Error(error);
                cx.notify();
                return;
            }
        };
        renderer.update(cx, |view, _| {
            // The wheel turns pages rather than scrolling lines, and page
            // turning is reader navigation, so the document must not consume
            // the event first.
            view.set_wheel_scroll_enabled(false);
        });
        startup_phase!("phase=renderer_entity_ready elapsed_ms={} phase_ms={}", finish_started.elapsed().as_millis(), renderer_started.elapsed().as_millis());
        self.renderer_subscription = Some(cx.subscribe_in(&renderer, window, |this, _, event: &RendererEvent, window, cx| {
            this.handle_renderer_event(event, window, cx);
        }));
        self.document_tap_subscription = Some(cx.subscribe_in(&renderer, window, |this, _, _: &DocumentTap, window, cx| {
            this.toggle_mobile_controls(window, cx);
        }));
        startup_phase!("phase=initial_preferences_embedded elapsed_ms={}", finish_started.elapsed().as_millis());
        self.load_state = LoadState::Ready(renderer);
        self.install_document_progress_weights(cx);
        // The window focused the page container before there was a document to
        // focus. Hand keyboard control to the document now that one exists, so
        // paging works without clicking the text first.
        self.focus_document(window, cx);
        self.load_reader_state(cx);
        self.load_database_toc(cx);
        startup_phase!("phase=reader_state_requested elapsed_ms={}", finish_started.elapsed().as_millis());
        crate::invalidation::notify(cx, Component::ReaderShell, "reader_ready");
        startup_phase!("phase=finish_load_complete elapsed_ms={}", finish_started.elapsed().as_millis());
    }

    /// Installs weights obtained from container metadata. For EPUB these are
    /// uncompressed ZIP entry sizes, so progress is available without opening
    /// and parsing every chapter. The renderer command retains its historical
    /// name, but consumes generic relative weights.
    fn install_document_progress_weights(&mut self, cx: &mut Context<Self>) {
        let weights = self.book.document_weights.clone();
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetDocumentTextLengths(weights), cx));
    }
}
