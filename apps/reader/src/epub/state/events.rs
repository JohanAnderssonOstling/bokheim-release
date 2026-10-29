//! Projection of renderer events into EPUB reader state.

use crate::invalidation::notify;
use std::sync::Arc;

use gpui::{App, Context, Window};
use skrift_column_paginator::RendererEvent;
use skrift_column_paginator::cfi::source_position_cfi;

use crate::epub::marks;
use crate::epub::{LoadState, ReaderView, Selection};
use crate::invalidation::Component;
use crate::settings::ReaderSettings;
use crate::shell::persistence::StoredPosition;

impl ReaderView {
    pub(super) fn handle_renderer_event(&mut self, event: &RendererEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            RendererEvent::ReadingPositionChanged(position) => {
                if self.reading_position.as_ref() == Some(position) {
                    return;
                }
                let document_changed = self.reading_position.as_ref().is_none_or(|previous| previous.source.document != position.source.document);
                let toc_changed = self.toc.update_position(&position.source);
                self.toc.update_read_status(position.fraction >= 1.0);
                self.progress.update(position.fraction, position.document_fraction, position.location, position.total_locations, position.source.document, position.document_count);
                self.reading_position = Some(position.clone());
                if document_changed {
                    self.refresh_chapter_search(cx);
                }
                if toc_changed {
                    self.sync_toc_position(cx);
                }
                self.record_current_reading_position(cx);
                #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
                self.tts_navigation_changed(cx);
                notify(cx, Component::ReaderShell, "reading_position_changed");
            }
            // Compatibility signals remain available to other renderer hosts.
            // This reader projects all position state from the settled snapshot.
            RendererEvent::CfiChanged(_) | RendererEvent::NavAnchorChanged { .. } | RendererEvent::ReadingProgress { .. } => {}
            RendererEvent::TitleChanged(title) => {
                let title = title.clone().unwrap_or_else(|| self.book_title.clone());
                if self.title != title {
                    self.title = title;
                    notify(cx, Component::ReaderShell, "title_changed");
                }
            }
            RendererEvent::HistoryAvailability { .. } => {}
            RendererEvent::SearchActiveChanged(active) => {
                if self.search.query.is_some() != *active {
                    if *active {
                        self.search.query = Some(self.search_input.read(cx).value().to_string());
                    } else {
                        self.search.clear();
                    }
                    notify(cx, Component::ReaderShell, "search_visibility_changed");
                }
            }
            // Font controls persist their requested value before reflow. An
            // initial snapshot or delayed acknowledgement must not revert it.
            RendererEvent::FontSizeChanged(_) => {}
            RendererEvent::ColumnWidthChanged(column_width) => {
                if ReaderSettings::update(cx, |preferences| preferences.column_width = *column_width) {
                    notify(cx, Component::ReaderShell, "column_width_changed");
                }
            }
            RendererEvent::ScaleChanged(scale) => {
                if ReaderSettings::update(cx, |preferences| preferences.scale = *scale) {
                    notify(cx, Component::ReaderShell, "scale_changed");
                }
            }
            RendererEvent::SelectionFinished { cfi_range, exact_text, .. } => {
                // A released handle can queue several ranges. Ignore snapshots
                // that were superseded or dismissed before they reach the shell.
                if let LoadState::Ready(renderer) = &self.load_state {
                    if renderer.read(cx).selected_passage().is_none_or(|(current, _)| current != *cfi_range) {
                        return;
                    }
                }
                if marks::set_selection(&mut self.selection, Selection { cfi_range: cfi_range.clone(), text: exact_text.clone() }) {
                    self.annotation_popup_anchor = Some(window.mouse_position());
                    notify(cx, Component::ReaderShell, "selection_finished");
                }
            }
            RendererEvent::AnnotationActivated { id } => {
                self.annotation_popup_anchor = Some(window.mouse_position());
                self.select_annotation(id.clone(), window, cx);
            }
            RendererEvent::FootnoteOpened(preview) => {
                let prepared = match &self.load_state {
                    LoadState::Ready(renderer) => renderer.update(cx, |view, _| view.prepare_note()),
                    LoadState::Loading | LoadState::Error(_) => None,
                };
                self.footnote = prepared.map(|prepared| (preview.clone(), prepared));
                notify(cx, Component::ReaderShell, "footnote_opened");
            }
            RendererEvent::ImageOpened { uri, bytes } => {
                let extension = uri.rsplit('.').next().unwrap_or_default().split(['?', '#']).next().unwrap_or_default().to_ascii_lowercase();
                let format = match extension.as_str() {
                    "jpg" | "jpeg" => gpui::ImageFormat::Jpeg,
                    "webp" => gpui::ImageFormat::Webp,
                    "gif" => gpui::ImageFormat::Gif,
                    "svg" | "svgz" => gpui::ImageFormat::Svg,
                    "bmp" => gpui::ImageFormat::Bmp,
                    "tif" | "tiff" => gpui::ImageFormat::Tiff,
                    "ico" => gpui::ImageFormat::Ico,
                    "pnm" | "ppm" | "pgm" | "pbm" => gpui::ImageFormat::Pnm,
                    _ => gpui::ImageFormat::Png,
                };
                self.image_preview = Some((uri.clone(), Arc::new(gpui::Image::from_bytes(format, bytes.clone()))));
                notify(cx, Component::ReaderShell, "image_preview_opened");
            }
            RendererEvent::MatchInfo { current, total } => {
                let next = Some((*current, *total));
                if self.search.match_position != next {
                    self.search.match_position = next;
                    notify(cx, Component::ReaderShell, "search_match_changed");
                }
            }
            RendererEvent::SearchResults(results) => {
                self.search.results = results.clone();
                self.search.rebuild_rows(&self.toc);
                notify(cx, Component::ReaderShell, "search_results_changed");
            }
            RendererEvent::OperationFailed { operation, message } => {
                log::warn!("renderer failed to {operation}: {message}");
                if *operation == skrift_column_paginator::RendererOperation::UpdateDocument {
                    self.page_turn = None;
                    self.swipe_preview.clear();
                    cx.notify();
                }
                #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
                if *operation == skrift_column_paginator::RendererOperation::UpdateDocument {
                    self.tts_document_failed(message, cx);
                }
            }
            _ => {}
        }
    }

    /// Save the source address, progress and resolved entry from one snapshot.
    pub(super) fn record_current_reading_position(&self, cx: &App) {
        if let Some(position) = &self.reading_position
            && let LoadState::Ready(view) = &self.load_state
        {
            self.reading_positions.record(StoredPosition::epub(source_position_cfi(&position.source), position.fraction).within(self.toc.active_link()), view.read(cx).navigation_revision());
        }
    }
}
