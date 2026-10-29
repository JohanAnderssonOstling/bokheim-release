mod annotations;
mod appearance;
mod events;
mod navigation;
mod search;

use std::{rc::Rc, sync::Arc};

use gpui::prelude::*;
use gpui::{App, Context, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::tree::TreeState;
use skrift_column_paginator::{RendererCommand, RendererEvent, RendererInitialConfig};
use html_view_gpui::{DocumentTap, HtmlView};
use library_backend::LibraryClient;
use library_model::BookLocator;

use crate::epub::toc::TocState;
use crate::epub::{LoadState, ProgressState, ReaderView, SearchState};
use crate::invalidation::{Component, notify};
use crate::settings::{ReaderSettings, library_interaction_palette, library_link_foreground, library_paint_palette, library_theme_adaptation};
use crate::shell::marks::MarkSession;
use crate::shell::persistence::ReadingPositionWriter;
use crate::shell::search::SearchBarTarget;
use crate::shell::settings_navigation::SettingsNavigation;
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
        style_overrides: preferences.style_overrides(Some(library_theme_adaptation(cx)), library_link_foreground(cx)),
        paint_palette: library_paint_palette(cx),
        interaction_palette: library_interaction_palette(cx),
        ..Default::default()
    }
}

impl ReaderView {
    pub(crate) fn new(
        locator: BookLocator, title: String, _app: app::AppClient, library: Rc<LibraryClient>, book: Arc<crate::book::NativeBook>, cjk_font: Option<crate::cjk::CjkFont>, preparation: skrift_column_paginator::RendererPreparation,
        close_reader: crate::CloseReader, #[cfg(feature = "kobo")] device_controls: Option<gpui::AnyView>, window: &mut Window, cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            let input = InputState::new(window, cx);
            #[cfg(feature = "kobo")]
            let input = input.placeholder("Search current chapter");
            input
        });
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
        let font_picker = cx.new(|cx| super::font_picker::FontPicker::new(window, cx));
        let cjk_font_picker = cx.new(|cx| {
            let mut picker = super::font_picker::FontPicker::new(window, cx);
            picker.set_cjk(true);
            picker
        });
        for picker in [&font_picker, &cjk_font_picker] {
            cx.subscribe_in(picker, window, |this, _, event: &super::font_picker::FontSelected, _, cx| {
                this.set_custom_font_family(event.role, event.family.clone(), cx);
            }).detach();
        }
        let annotation_filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter annotations"));
        let annotation_filter_subscription = cx.subscribe(&annotation_filter_input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                notify(cx, Component::ReaderShell, "annotation_filter_changed");
            }
        });
        let focus = cx.focus_handle();
        let toc_tree = cx.new(|cx| TreeState::new(cx));
        focus.focus(window, cx);
        let reading_positions = ReadingPositionWriter::new(cx, locator.content_hash(), library.clone());
        let marks = MarkSession::new(cx, library.clone());

        #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
        let tts = crate::shell::tts::Narration::for_languages(&book.languages())
            .with_title(title.clone())
            .with_rate(ReaderSettings::preferences(cx).audiobook_speed)
            .with_pauses(ReaderSettings::preferences(cx).narration_pauses)
            .with_voice(ReaderSettings::preferences(cx).narration_voice)
            .with_availability(cx);

        let mut reader = Self {
            #[cfg(feature = "kobo")]
            device_controls,
            locator,
            library,
            book_title: title.clone(),
            title,
            load_state: LoadState::Loading,
            applied_palette: None,
            last_column_viewport_plan: None,
            desired_column_count: u8::MAX,
            toc: TocState::default(),
            toc_tree,
            annotation_tree: Default::default(),
            sidebar: SidebarState::from_preferences(cx),
            chrome: crate::shell::chrome::ChromeState::from_preferences(Component::ReaderShell, cx),
            wheel_delta: 0.0,
            wheel_debounce: None,
            touchpad: ui_components::TouchpadNavigation::default(),
            touch_swipe: None,
            swipe_preview: ui_components::SwipePreview::default(),
            page_turn: None,
            page_size: None,
            touch_swipe_from_edge: false,
            search_input,
            font_picker,
            cjk_font_picker,
            search: SearchState::default(),
            book,
            cjk_font,
            reading_position: None,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_fragments: Vec::new(),
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_doc: 0,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_navigation_revision: 0,
            progress: ProgressState::default(),
            marks,
            selection: None,
            touch_selection: Default::default(),
            annotation_note_input,
            annotation_filter_input,
            annotation_popup_anchor: None,
            footnote: None,
            image_preview: None,
            context_sheet: None,
            focus,
            sidebar_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            rail_focus: cx.focus_handle().tab_stop(true),
            settings_navigation: SettingsNavigation::new(window, cx),
            renderer_subscription: None,
            document_tap_subscription: None,
            reading_positions,
            close_reader,
            _search_subscription: search_subscription,
            _annotation_filter_subscription: annotation_filter_subscription,
        };
        #[cfg(all(feature = "native-tts", target_os = "android"))]
        crate::shell::tts::load_media_metadata(reader.library.clone(), reader.locator.clone(), cx);
        reader.finish_load(preparation, window, cx);
        reader
    }

    fn finish_load(&mut self, preparation: skrift_column_paginator::RendererPreparation, window: &mut Window, cx: &mut Context<Self>) {
        let renderer = match HtmlView::create_from_preparation(preparation, window, cx) {
            Ok(renderer) => renderer,
            Err(error) => {
                log::error!("could not initialize EPUB renderer: {error}");
                let failure = error.clone();
                // The parent installs its failure subscription after this
                // constructor returns, before deferred events are delivered.
                cx.defer_in(window, move |_, _, cx| cx.emit(crate::BookOpenFailed(failure)));
                self.load_state = LoadState::Error(ui_components::user_facing_error(&error, "Try opening this book again. If it still fails, download another copy or choose another edition.").to_owned());
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
        self.renderer_subscription = Some(cx.subscribe_in(&renderer, window, |this, _, event: &RendererEvent, window, cx| {
            this.handle_renderer_event(event, window, cx);
        }));
        self.document_tap_subscription = Some(cx.subscribe_in(&renderer, window, |this, _, _: &DocumentTap, window, cx| {
            this.toggle_mobile_controls(window, cx);
        }));
        self.load_state = LoadState::Ready(renderer);
        self.install_document_progress_weights(cx);
        // The window focused the page container before there was a document to
        // focus. Hand keyboard control to the document now that one exists, so
        // paging works without clicking the text first.
        self.focus_document(window, cx);
        self.load_reader_state(cx);
        self.load_database_toc(cx);
        notify(cx, Component::ReaderShell, "reader_ready");
    }

    /// Installs weights obtained from container metadata. For EPUB these are
    /// uncompressed ZIP entry sizes, so progress is available without opening
    /// and parsing every chapter. The renderer command retains its historical
    /// name, but consumes generic relative weights.
    fn install_document_progress_weights(&mut self, cx: &mut Context<Self>) {
        let weights = self.book.document_weights().to_vec();
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetDocumentTextLengths(weights), cx));
    }
}
