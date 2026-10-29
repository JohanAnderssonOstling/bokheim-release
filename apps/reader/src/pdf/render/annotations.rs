//! PDF annotation loading, editing, navigation, and sidebar presentation.

use crate::invalidation::notify;
use crate::shell::sidebar_list;
use crate::shell::state_panel::{AnnotationSidebarTarget, reader_state_rows, render_annotation_row};
use book_model::{AnnotationAnchor, AnnotationStyle, PdfAnnotationAnchor, PdfAnnotationRect};
use gpui::prelude::*;
use gpui::{Context, Window, div};
use library_backend::Annotation;
use pdf_reader_core::NormalizedRect;
use ui_components as components;

use super::PdfReaderView;
use crate::invalidation::Component;
use crate::pdf::marks::annotation_overlays;
use crate::shell::annotation_editor;
use crate::shell::annotation_editor::AnnotationEditorTarget;
use crate::shell::persistence;

impl AnnotationEditorTarget for PdfReaderView {
    fn save_annotation_changes(&mut self, style: Option<AnnotationStyle>, color: Option<u32>, cx: &mut Context<Self>) {
        self.save_selected_annotation(style, color, cx);
    }

    fn dismiss_annotation_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_annotation_editor(cx);
        self.focus_document(window, cx);
    }

    fn delete_edited_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.marks.selected_id().map(str::to_owned) {
            self.delete_annotation(id, cx);
            self.focus_document(window, cx);
        }
    }
}

impl AnnotationSidebarTarget for PdfReaderView {
    fn select_sidebar_annotation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.annotation_popup_anchor = None;
        self.select_annotation(id, window, cx);
    }

    fn go_to_annotation(&mut self, annotation: &Annotation, cx: &mut Context<Self>) {
        self.navigate_to_annotation(annotation, cx);
    }

    fn delete_sidebar_annotation(&mut self, id: String, cx: &mut Context<Self>) {
        self.delete_annotation(id, cx);
    }
}

impl PdfReaderView {
    pub(super) fn load_reader_state(&self, cx: &mut Context<Self>) {
        let content_hash = self.locator.content_hash();
        persistence::write(
            cx,
            self.library.clone(),
            "load PDF reader state",
            move |library| {
                let library = library.clone();
                Box::pin(async move { Ok(library.annotations(content_hash).await?.into_iter().filter(|item| item.anchor.pdf_anchor().is_some()).collect()) })
            },
            |this, cx, annotations| {
                this.marks.merge_loaded(annotations);
                this.refresh_annotation_overlays(cx);
                notify(cx, Component::PdfShell, "marks_loaded");
            },
        );
    }

    fn refresh_annotation_overlays(&self, cx: &mut Context<Self>) {
        let overlays = annotation_overlays(self.marks.annotations());
        self.pdf.update(cx, |pdf, cx| pdf.set_annotations(overlays, cx));
    }

    /// Creates an annotation and selects it so
    /// its editor is immediately available for an optional note.
    pub(super) fn create_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.pdf.read(cx).selection_snapshot() else { return };
        let rects = selection.rects.iter().map(|rect| PdfAnnotationRect::new(rect.left(), rect.top(), rect.width(), rect.height())).collect::<Vec<_>>();
        let anchor = AnnotationAnchor::Pdf(PdfAnnotationAnchor::new(selection.page_index as u32, rects, None));
        let now = crate::shell::marks::now_millis();
        let annotation = Annotation {
            id: uuid::Uuid::new_v4().to_string(),
            content_hash: self.locator.content_hash(),
            anchor,
            exact_text: selection.text,
            style: AnnotationStyle::Highlight,
            color: ui_components::READER_ANNOTATION_DEFAULT_COLOR,
            note: String::new(),
            created_at: now,
            toc_ordinal: self.toc.ordinal_at(selection.page_index).map(|ordinal| ordinal as i64),
            progress: Some(book_model::pdf_page_progress(selection.page_index, self.page_count)),
            modified_at: now,
            deleted: false,
        };
        let annotation_id = annotation.id.clone();
        self.marks.prepend(annotation, "save PDF annotation");
        let note = self.marks.select(annotation_id);
        self.annotation_note_input.update(cx, |input, cx| input.set_value(note, window, cx));
        self.refresh_annotation_overlays(cx);
        notify(cx, Component::PdfShell, "annotation_created");
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
        notify(cx, Component::PdfShell, "annotation_selected");
    }

    fn close_annotation_editor(&mut self, cx: &mut Context<Self>) {
        self.marks.clear_selection();
        self.annotation_popup_anchor = None;
        self.pdf.update(cx, |pdf, cx| pdf.set_active_annotation(None, cx));
        notify(cx, Component::PdfShell, "annotation_editor_closed");
    }

    fn save_selected_annotation(&mut self, style: Option<AnnotationStyle>, color: Option<u32>, cx: &mut Context<Self>) {
        if self.marks.selected().is_some_and(Annotation::is_external_pdf_annotation) {
            return;
        }
        let note = self.annotation_note_input.read(cx).value().to_string();
        if self.marks.update_selected(note, style, color, "update PDF annotation").is_none() {
            return;
        }
        self.refresh_annotation_overlays(cx);
        notify(cx, Component::PdfShell, "annotation_updated");
    }

    pub(in crate::pdf) fn delete_annotation(&mut self, id: String, cx: &mut Context<Self>) {
        if self.marks.annotation(&id).is_some_and(Annotation::is_external_pdf_annotation) {
            return;
        }
        if self.marks.delete(id, "delete PDF annotation") {
            self.annotation_popup_anchor = None;
            self.pdf.update(cx, |pdf, cx| pdf.set_active_annotation(None, cx));
        }
        self.refresh_annotation_overlays(cx);
        notify(cx, Component::PdfShell, "annotation_deleted");
    }

    fn navigate_to_annotation(&self, annotation: &Annotation, cx: &mut Context<Self>) {
        let Some(anchor) = annotation.anchor.pdf_anchor() else { return };
        let Some(rect) = anchor.rects.first() else { return };
        self.pdf.update(cx, |pdf, cx| {
            pdf.reveal_annotation(anchor.page_index as usize, NormalizedRect::new(rect.left, rect.top, rect.width, rect.height), cx);
        });
    }

    pub(super) fn render_annotations_body(&mut self, theme: components::BrowserTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let rows = reader_state_rows(self.marks.annotations(), "No notes yet. Select PDF text, then choose Annotation to add a note.", |annotation: &Annotation| {
            let page = annotation.anchor.pdf_anchor().map(|anchor| anchor.page_index as usize);
            if let Some(page) = page { (format!("page:{page}"), format!("Page {}", page + 1)) } else { ("other".to_owned(), "Other notes".to_owned()) }
        });
        let selected = self.marks.selected_id();
        let selected_row = selected.and_then(|id| rows.iter().position(|row| row.annotation_id() == Some(id)));
        self.annotation_list.set_rows(rows);
        self.annotation_list.reveal_selected(selected, selected_row);
        sidebar_list::panel("pdf-annotation-list", &cx.entity(), &self.annotation_list.state, theme, |this, row, target, theme, _| {
            let Some(row) = this.annotation_list.rows.get(row) else { return div().into_any_element() };
            render_annotation_row(target, row, this.marks.annotations(), this.marks.selected_id(), "", theme)
        })
    }

    pub(super) fn render_annotation_popup(&self, annotation: Annotation, geometry: annotation_editor::Geometry, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        annotation_editor::standard_editor("pdf-popup", &cx.entity(), &annotation, geometry, &self.annotation_note_input, components::browser_theme(cx), window, cx)
    }
}
