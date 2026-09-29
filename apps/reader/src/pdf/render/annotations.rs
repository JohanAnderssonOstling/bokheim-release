//! PDF annotation loading, editing, navigation, and sidebar presentation.

use book_model::{AnnotationAnchor, AnnotationStyle, PdfAnnotationAnchor, PdfAnnotationRect, annotation_progress};
use gpui::prelude::*;
use gpui::{Context, Window};
use library_backend::ReaderAnnotation;
use pdf_reader_core::NormalizedRect;
use ui_components as components;

use super::PdfReaderView;
use crate::invalidation::Component;
use crate::pdf::marks::annotation_overlays;
use crate::shell::annotation_editor;
use crate::shell::persistence;

impl PdfReaderView {
    pub(super) fn load_reader_state(&self, cx: &mut Context<Self>) {
        let content_hash = self.locator.content_hash();
        persistence::write(
            cx,
            self.library.clone(),
            "load PDF reader state",
            move |library| {
                let library = library.clone();
                Box::pin(async move { Ok(library.annotations(content_hash).await?.into_iter().filter(|item| item.pdf_anchor().is_some()).collect()) })
            },
            |this, cx, annotations| {
                this.marks.merge_loaded(annotations);
                this.refresh_annotation_overlays(cx);
                crate::invalidation::notify(cx, Component::PdfShell, "marks_loaded");
            },
        );
    }

    fn refresh_annotation_overlays(&self, cx: &mut Context<Self>) {
        let overlays = annotation_overlays(self.marks.annotations());
        self.pdf.update(cx, |pdf, cx| pdf.set_annotations(overlays, cx));
    }

    /// Mirrors EPUB's context-menu action: create a highlight and select it so
    /// its editor is immediately available for an optional note.
    pub(super) fn create_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.pdf.read(cx).selection_snapshot() else { return };
        let rects = selection.rects.iter().map(|rect| PdfAnnotationRect::new(rect.left(), rect.top(), rect.width(), rect.height())).collect::<Vec<_>>();
        let anchor = AnnotationAnchor::pdf(PdfAnnotationAnchor::new(selection.page_index as u32, rects, None));
        let now = crate::shell::marks::now_millis();
        let annotation = ReaderAnnotation {
            id: uuid::Uuid::new_v4().to_string(),
            content_hash: self.locator.content_hash(),
            anchor,
            exact_text: selection.text,
            style: AnnotationStyle::Highlight,
            color: ui_components::READER_ANNOTATION_DEFAULT_COLOR.to_owned(),
            note: String::new(),
            created_at: now,
            modified_at: now,
            toc_ordinal: self.toc.ordinal_at(selection.page_index).map(|ordinal| ordinal as i64),
            progress: annotation_progress(selection.page_index as f64 + 1.0, self.page_count as f64),
        };
        let annotation_id = annotation.id.clone();
        self.marks.prepend(annotation, "save PDF annotation");
        let note = self.marks.select(annotation_id);
        self.annotation_note_input.update(cx, |input, cx| input.set_value(note, window, cx));
        self.refresh_annotation_overlays(cx);
        crate::invalidation::notify(cx, Component::PdfShell, "annotation_created");
    }

    pub(super) fn select_annotation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(annotation) = self.marks.annotation(&id).filter(|annotation| annotation.is_external_pdf_annotation()).cloned() {
            self.marks.clear_selection();
            self.annotation_popup_anchor = None;
            self.pdf.update(cx, |pdf, cx| pdf.set_active_annotation(None, cx));
            self.navigate_to_annotation(&annotation, cx);
            return;
        }
        let note = self.marks.select(id.clone());
        self.pdf.update(cx, |pdf, cx| pdf.set_active_annotation(Some(id), cx));
        self.annotation_note_input.update(cx, |input, cx| input.set_value(note, window, cx));
        crate::invalidation::notify(cx, Component::PdfShell, "annotation_selected");
    }

    fn close_annotation_editor(&mut self, cx: &mut Context<Self>) {
        self.marks.clear_selection();
        self.annotation_popup_anchor = None;
        self.pdf.update(cx, |pdf, cx| pdf.set_active_annotation(None, cx));
        crate::invalidation::notify(cx, Component::PdfShell, "annotation_editor_closed");
    }

    fn save_selected_annotation(&mut self, style: Option<AnnotationStyle>, color: Option<&'static str>, cx: &mut Context<Self>) {
        if self.marks.selected().is_some_and(ReaderAnnotation::is_external_pdf_annotation) {
            return;
        }
        let note = self.annotation_note_input.read(cx).value().to_string();
        if self.marks.update_selected(note, style, color, "update PDF annotation").is_none() {
            return;
        }
        self.refresh_annotation_overlays(cx);
        crate::invalidation::notify(cx, Component::PdfShell, "annotation_updated");
    }

    pub(in crate::pdf) fn delete_annotation(&mut self, id: String, cx: &mut Context<Self>) {
        if self.marks.annotation(&id).is_some_and(ReaderAnnotation::is_external_pdf_annotation) {
            return;
        }
        if self.marks.delete(id, "delete PDF annotation") {
            self.annotation_popup_anchor = None;
            self.pdf.update(cx, |pdf, cx| pdf.set_active_annotation(None, cx));
        }
        self.refresh_annotation_overlays(cx);
        crate::invalidation::notify(cx, Component::PdfShell, "annotation_deleted");
    }

    fn navigate_to_annotation(&self, annotation: &ReaderAnnotation, cx: &mut Context<Self>) {
        let Some(anchor) = annotation.pdf_anchor() else { return };
        let Some(rect) = anchor.rects.first() else { return };
        self.pdf.update(cx, |pdf, cx| {
            pdf.reveal_annotation(anchor.page_index as usize, NormalizedRect::new(rect.left, rect.top, rect.width, rect.height), cx);
        });
    }

    pub(super) fn render_annotations_body(&self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let target = cx.entity();
        components::reader_state_panel(theme)
            .children(crate::shell::state_panel::reader_state_rows(
                &target,
                &self.marks,
                "Select PDF text, then use Highlight or Note.",
                theme,
                |annotation: &ReaderAnnotation| {
                    let page = annotation.pdf_anchor().map(|anchor| anchor.page_index as usize);
                    let ordinal = annotation.toc_ordinal.and_then(|ordinal| usize::try_from(ordinal).ok()).or_else(|| page.and_then(|page| self.toc.ordinal_at(page)));
                    if let Some((ordinal, title)) = ordinal.and_then(|ordinal| self.toc.title_at_ordinal(ordinal).map(|title| (ordinal, title))) {
                        (format!("section:{ordinal}"), title.to_owned())
                    } else if let Some(page) = page {
                        (format!("page:{page}"), format!("Page {}", page + 1))
                    } else {
                        ("other".to_owned(), "Other annotations".to_owned())
                    }
                },
                |id, window, this: &mut Self, cx| {
                    this.annotation_popup_anchor = None;
                    this.select_annotation(id, window, cx);
                },
                |annotation: &ReaderAnnotation, this: &mut Self, cx| this.navigate_to_annotation(annotation, cx),
                |id, this: &mut Self, cx| this.delete_annotation(id, cx),
            ))
            .into_any_element()
    }

    pub(super) fn render_annotation_popup(&self, annotation: ReaderAnnotation, geometry: annotation_editor::Geometry, cx: &mut Context<Self>) -> gpui::AnyElement {
        annotation_editor::standard_editor(
            "pdf-popup",
            &cx.entity(),
            &annotation,
            geometry,
            &self.annotation_note_input,
            components::browser_theme(cx),
            |style, color, this, cx| this.save_selected_annotation(style, color, cx),
            |window, this, cx| {
                this.close_annotation_editor(cx);
                this.focus_document(window, cx);
            },
        )
    }
}
