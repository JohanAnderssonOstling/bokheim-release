//! PDF reader construction and renderer/input event wiring.

use crate::invalidation::notify;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Context, Entity, Window};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::tree::TreeState;
use library_backend::BoxedBookReader;
use library_backend::LibraryClient;
use library_model::BookLocator;
use pdf_view_gpui::{PdfView, PdfViewEvent};

use crate::invalidation::Component;
use crate::pdf::search::renderer_query;
use crate::pdf::state::stored_zoom_mode;
use crate::pdf::toc::PdfTocState;
use crate::settings::{ReaderSettings, library_interaction_style};
use crate::shell::marks::MarkSession;
use crate::shell::persistence::{ReadingPositionWriter, StoredPosition};
use crate::shell::search::SearchBarTarget;
use crate::shell::settings_navigation::SettingsNavigation;
use crate::shell::state_panel::{SidebarState, SidebarTab};

use super::PdfReaderView;

impl PdfReaderView {
    pub(crate) fn new(
        locator: BookLocator, title: String, _app: app::AppClient, library: Rc<LibraryClient>, reader: BoxedBookReader, metadata: Option<pdf_reader_core::PdfReaderMetadata>,
        page_source: Option<std::sync::Arc<dyn pdf_reader_core::PdfPageSource>>, initial_page: usize, initial_full_page_position: f32, close_reader: crate::CloseReader, #[cfg(feature = "kobo")] device_controls: Option<gpui::AnyView>, window: &mut Window, cx: &mut Context<Self>,
    ) -> Self {
        let preferences = ReaderSettings::preferences(cx);
        let zoom_mode = stored_zoom_mode(preferences);
        let trim_margins = preferences.pdf_trim_margins;
        let source_name = title.clone();
        let interaction_style = library_interaction_style(cx);
        let imported = crate::pdf::marks::imported_annotations(locator.content_hash(), metadata.as_ref());
        let pdf = cx.new(|cx| {
            let mut pdf = PdfView::from_reader_with_page_source(source_name, reader, metadata, page_source, initial_page, window, cx);
            pdf.set_interaction_style(interaction_style, cx);
            pdf.set_zoom_mode(zoom_mode, cx);
            pdf.set_trim_margins(trim_margins, cx);
            pdf.set_full_page_position(initial_full_page_position, cx);
            pdf
        });
        let search_input = cx.new(|cx| InputState::new(window, cx));
        let toc_tree = cx.new(|cx| TreeState::new(cx));
        let annotation_note_input = cx.new(|cx| crate::shell::annotation_editor::note_input(window, cx));
        let pdf_subscription = cx.subscribe_in(&pdf, window, |this, pdf, event, window, cx| this.handle_pdf_event(pdf, event, window, cx));
        let pdf_for_search = pdf.clone();
        let search_subscription = cx.subscribe_in(&search_input, window, move |this, input, event: &InputEvent, _window, cx| match event {
            InputEvent::Change => {
                let query = renderer_query(&input.read(cx).value());
                this.search_list.clear();
                pdf_for_search.update(cx, |pdf, cx| pdf.search(query, cx));
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } => this.search_navigate(if *shift { -1 } else { 1 }, cx),
            _ => {}
        });
        let focus = cx.focus_handle();
        // Page elements establish `PdfReaderDocument`; until one is laid out,
        // the shell owns keyboard focus.
        focus.focus(window, cx);
        let positions = ReadingPositionWriter::new(cx, locator.content_hash(), library.clone());
        let mut marks = MarkSession::new(cx, library.clone());
        marks.merge_loaded(imported);
        #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
        let tts = crate::shell::tts::Narration::default()
            .with_title(title.clone())
            .with_rate(ReaderSettings::preferences(cx).audiobook_speed)
            .with_pauses(ReaderSettings::preferences(cx).narration_pauses)
            .with_voice(ReaderSettings::preferences(cx).narration_voice)
            .with_availability(cx);
        let reader = Self {
            #[cfg(feature = "kobo")]
            device_controls,
            locator,
            title,
            library,
            pdf,
            search_input,
            search_match: None,
            search_list: Default::default(),
            toc_tree,
            toc: PdfTocState::default(),
            sidebar: SidebarState::from_preferences(cx),
            chrome: crate::shell::chrome::ChromeState::from_preferences(Component::PdfShell, cx),
            marks,
            annotation_list: Default::default(),
            annotation_note_input,
            annotation_popup_anchor: None,
            context_sheet: None,
            current_page: 0,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_fragments: Vec::new(),
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_request: 0,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_page: 0,
            #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
            tts_navigation_revision: 0,
            page_count: 0,
            positions,
            close_reader,
            focus,
            sidebar_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            rail_focus: cx.focus_handle().tab_stop(true),
            settings_navigation: SettingsNavigation::new(window, cx),
            _subscriptions: vec![pdf_subscription, search_subscription],
        };
        #[cfg(all(feature = "native-tts", target_os = "android"))]
        crate::shell::tts::load_media_metadata(reader.library.clone(), reader.locator.clone(), cx);
        reader.load_reader_state(cx);
        reader.load_database_toc(cx);
        reader
    }

    fn handle_pdf_event(&mut self, pdf: &Entity<PdfView>, event: &PdfViewEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PdfViewEvent::PositionChanged { position, total } => {
                if *total == 0 {
                    return;
                }
                let current_page = position.page_index();
                let full_page_position = position.page_y();
                let unchanged = self.current_page == current_page && self.page_count == *total;
                self.current_page = current_page;
                self.page_count = *total;
                self.update_active_toc(current_page, cx);
                #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
                self.tts_navigation_changed(cx);
                self.positions.record(StoredPosition::pdf_page(current_page, full_page_position, *total), self.pdf.read(cx).navigation_revision());
                if unchanged {
                    return;
                }
                notify(cx, Component::PdfShell, "page_changed");
            }
            PdfViewEvent::SearchChanged { current, total } => {
                if *total < self.search_list.hit_count() {
                    self.search_list.clear();
                }
                let indexed = self.search_list.hit_count();
                self.search_list.extend_pages(pdf.read(cx).search_hits().iter().skip(indexed).map(|hit| hit.page_index()));
                if self.search_match.is_none_or(|(previous, _)| previous != *current) {
                    self.search_list.reveal_match(*current);
                }
                let Some(match_position) = self.search_match.as_mut() else { return };
                if *match_position != (*current, *total) || indexed != self.search_list.hit_count() {
                    *match_position = (*current, *total);
                    notify(cx, Component::PdfShell, "match_changed");
                }
            }
            PdfViewEvent::PageTapped => self.toggle_mobile_controls(window, cx),
            PdfViewEvent::AnnotationActivated { id, position } => {
                crate::shell::state_panel::select_tab(&mut self.sidebar, SidebarTab::Notes, cx);
                self.annotation_popup_anchor = Some(*position);
                self.select_annotation(id.clone(), window, cx);
            }
            PdfViewEvent::LoadFailed(error) => {
                log::error!("failed to load PDF reader for book {}: {error}", self.title);
                cx.emit(crate::BookOpenFailed(error.clone()));
            }
        }
    }
}
