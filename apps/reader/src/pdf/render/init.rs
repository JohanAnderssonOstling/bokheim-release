//! PDF reader construction and renderer/input event wiring.

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
use crate::shell::state_panel::{SidebarState, SidebarTab};

use super::PdfReaderView;

impl PdfReaderView {
    pub(crate) fn new(
        locator: BookLocator, title: String, _app: app::AppClient, library: Rc<LibraryClient>, reader: BoxedBookReader, metadata: Option<pdf_reader_core::PdfReaderMetadata>,
        page_source: Option<std::sync::Arc<dyn pdf_reader_core::PdfPageSource>>, initial_page: usize, initial_full_page_position: f32, close_reader: crate::CloseReader, window: &mut Window, cx: &mut Context<Self>,
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
                pdf_for_search.update(cx, |pdf, cx| pdf.search(query, cx));
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
        let reader = Self {
            #[cfg(feature = "kobo")]
            brightness: cx.new(|cx| crate::shell::brightness::BrightnessControls::new(_app.clone(), window, cx)),
            locator,
            title,
            library,
            pdf,
            search_input,
            search_match: None,
            toc_tree,
            toc: PdfTocState::default(),
            sidebar: SidebarState::from_preferences(cx),
            chrome: crate::shell::chrome::ChromeState::new(Component::PdfShell),
            marks,
            annotation_note_input,
            annotation_popup_anchor: None,
            context_sheet: None,
            current_page: 0,
            page_count: 0,
            positions,
            close_reader,
            focus,
            _subscriptions: vec![pdf_subscription, search_subscription],
        };
        reader.load_reader_state(cx);
        reader.load_database_toc(cx);
        reader
    }

    fn handle_pdf_event(&mut self, pdf: &Entity<PdfView>, event: &PdfViewEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PdfViewEvent::PageChanged { current, total } => {
                let current_page = current.saturating_sub(1);
                let full_page_position = pdf.read(cx).full_page_position();
                let unchanged = self.current_page == current_page && self.page_count == *total;
                self.current_page = current_page;
                self.page_count = *total;
                self.update_active_toc(current_page, cx);
                if unchanged {
                    return;
                }
                self.positions.record(StoredPosition::pdf_page(current_page, full_page_position, *total));
                crate::invalidation::notify(cx, Component::PdfShell, "page_changed");
            }
            PdfViewEvent::SearchChanged { current, total } => {
                let Some(match_position) = self.search_match.as_mut() else { return };
                if *match_position != (*current, *total) {
                    *match_position = (*current, *total);
                    crate::invalidation::notify(cx, Component::PdfShell, "match_changed");
                }
            }
            PdfViewEvent::PageTapped => self.toggle_mobile_controls(window, cx),
            PdfViewEvent::AnnotationActivated { id, position } => {
                crate::shell::state_panel::select_tab(&mut self.sidebar, SidebarTab::Annotations, cx);
                self.annotation_popup_anchor = Some(*position);
                self.select_annotation(id.clone(), window, cx);
            }
            PdfViewEvent::LoadFailed(error) => {
                log::error!("failed to load PDF reader for book {}: {error}", self.locator.content_hash());
            }
        }
    }
}
