//! Projection of renderer events into EPUB reader state.

use std::sync::Arc;

use gpui::{Context, ScrollStrategy, Window};
use gpui_component::tree::TreeItem;
use html_view_core::RendererEvent;

use crate::epub::marks;
use crate::epub::{LoadState, ReaderView, Selection};
use crate::invalidation::Component;
use crate::settings::ReaderSettings;
use crate::shell::persistence::StoredPosition;

/// The CFI to persist, or `None` when the renderer re-reported the one
/// already recorded.
fn update_cfi_position(current: &mut Option<String>, next: &Option<String>) -> Option<String> {
    if current == next {
        return None;
    }
    current.clone_from(next);
    current.clone()
}

impl ReaderView {
    pub(super) fn handle_renderer_event(&mut self, event: &RendererEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            RendererEvent::CfiChanged(cfi) => {
                if let Some(cfi) = update_cfi_position(&mut self.current_cfi, cfi) {
                    self.reading_positions.record(StoredPosition::epub(cfi, self.progress.fraction).within(self.toc.active_link()));
                }
            }
            RendererEvent::TitleChanged(title) => {
                let title = title.clone().unwrap_or_else(|| self.book_title.clone());
                if self.title != title {
                    self.title = title;
                    crate::invalidation::notify(cx, Component::ReaderShell, "title_changed");
                }
            }
            RendererEvent::TocChanged(_) | RendererEvent::HistoryAvailability { .. } => {}
            RendererEvent::NavAnchorChanged { doc, anchor } => {
                let next = self.toc.link_at(*doc, anchor);
                if self.toc.set_active_link(next.clone()) {
                    self.toc_tree.update(cx, |tree, cx| {
                        if let Some(link) = next.as_ref() {
                            let selected = TreeItem::new(link.clone(), link.clone());
                            tree.set_selected_item(Some(&selected), cx);
                            tree.reveal_item(&selected.id, ScrollStrategy::Center, cx);
                        } else {
                            tree.set_selected_item(None, cx);
                        }
                    });
                    crate::invalidation::notify(cx, Component::ReaderShell, "toc_position_changed");
                }
            }
            RendererEvent::SearchActiveChanged(active) => {
                if self.search.query.is_some() != *active {
                    if *active {
                        self.search.query = Some(self.search_input.read(cx).value().to_string());
                    } else {
                        self.search.clear();
                    }
                    crate::invalidation::notify(cx, Component::ReaderShell, "search_visibility_changed");
                }
            }
            RendererEvent::FontSizeChanged(font_size) => {
                if ReaderSettings::update(cx, |preferences| preferences.font_size = *font_size) {
                    crate::invalidation::notify(cx, Component::ReaderShell, "font_size_changed");
                }
            }
            RendererEvent::ColumnWidthChanged(column_width) => {
                if ReaderSettings::update(cx, |preferences| preferences.column_width = *column_width) {
                    crate::invalidation::notify(cx, Component::ReaderShell, "column_width_changed");
                }
            }
            RendererEvent::ScaleChanged(scale) => {
                if ReaderSettings::update(cx, |preferences| preferences.scale = *scale) {
                    crate::invalidation::notify(cx, Component::ReaderShell, "scale_changed");
                }
            }
            RendererEvent::ReadingProgress { fraction, location, total_locations, doc, doc_count, .. } => {
                // TODO(2026-08-19): wire this to the renderer's `doc_fraction`
                // field once HtmlViewCore has been updated to emit it.
                if self.progress.update(*fraction, 0.0, *location, *total_locations, *doc, *doc_count) {
                    self.record_current_reading_position();
                    crate::invalidation::notify(cx, Component::ReaderShell, "reading_progress_changed");
                }
            }
            RendererEvent::SelectionFinished { cfi_range, exact_text, .. } => {
                if marks::set_selection(&mut self.selection, Selection { cfi_range: cfi_range.clone(), text: exact_text.clone() }) {
                    self.annotation_popup_anchor = Some(window.mouse_position());
                    crate::invalidation::notify(cx, Component::ReaderShell, "selection_finished");
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
                crate::invalidation::notify(cx, Component::ReaderShell, "footnote_opened");
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
                crate::invalidation::notify(cx, Component::ReaderShell, "image_preview_opened");
            }
            RendererEvent::MatchInfo { current, total } => {
                let next = Some((*current, *total));
                if self.search.match_position != next {
                    self.search.match_position = next;
                    crate::invalidation::notify(cx, Component::ReaderShell, "search_match_changed");
                }
            }
            RendererEvent::SearchResults(_) => {}
            RendererEvent::OperationFailed { operation, message } => {
                log::warn!("renderer failed to {operation}: {message}");
            }
            _ => {}
        }
    }

    /// CFI and progress are emitted as separately deduplicated renderer
    /// signals. Persist whenever either half changes.
    fn record_current_reading_position(&self) {
        if let Some(cfi) = self.current_cfi.clone() {
            self.reading_positions.record(StoredPosition::epub(cfi, self.progress.fraction).within(self.toc.active_link()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::update_cfi_position;

    #[test]
    fn a_changed_cfi_is_persisted_even_when_progress_is_unchanged() {
        let mut current = Some("epubcfi(/6/14!/4/2[ch03]/2/2/2/1:0)".to_owned());
        let next = Some("epubcfi(/6/14!/4/2[ch03]/54/1:121)".to_owned());

        let update = update_cfi_position(&mut current, &next);

        assert_eq!(update, next);
        assert_eq!(current, next);
    }

    #[test]
    fn an_identical_cfi_does_not_queue_another_write() {
        let mut current = Some("epubcfi(/6/14!/4/2[ch03]/54/1:121)".to_owned());
        let next = current.clone();

        assert_eq!(update_cfi_position(&mut current, &next), None);
    }
}
