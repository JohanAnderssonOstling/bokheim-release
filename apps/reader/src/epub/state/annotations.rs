//! EPUB annotation loading, editing, and renderer projection.

use book_model::{AnnotationAnchor, AnnotationStyle};
use gpui::{Context, Window};
use html_view_core::RendererCommand;
use library_backend::ReaderAnnotation;

use crate::epub::{ReaderView, Selection, marks};
use crate::invalidation::Component;
use crate::shell::persistence;

impl ReaderView {
    pub(super) fn load_reader_state(&mut self, cx: &mut Context<Self>) {
        let content_hash = self.locator.content_hash();
        persistence::write(
            cx,
            self.library.clone(),
            "load reader state",
            move |library| {
                let library = library.clone();
                Box::pin(async move { library.annotations(content_hash).await })
            },
            |this, cx, annotations| {
                // Preserve optimistic edits made while the backend snapshot was
                // in flight, including deletions that snapshot still contains.
                this.marks.merge_loaded(annotations);
                let overlays = marks::overlays(this.marks.annotations());
                this.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetAnnotations(overlays), cx));
                crate::invalidation::notify(cx, Component::ReaderShell, "marks_loaded");
            },
        );
    }

    pub(in crate::epub) fn highlight_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Selection { cfi_range, text: exact_text }) = self.selection.take() else { return };
        self.annotation_note_input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        let now = crate::shell::marks::now_millis();
        let annotation = ReaderAnnotation {
            id: uuid::Uuid::new_v4().to_string(),
            content_hash: self.locator.content_hash(),
            anchor: AnnotationAnchor::epub_cfi(cfi_range),
            exact_text,
            style: AnnotationStyle::Highlight,
            color: ui_components::READER_ANNOTATION_DEFAULT_COLOR.to_owned(),
            note: String::new(),
            created_at: now,
            modified_at: now,
            // The selection is visible, so the viewport's entry is the
            // selection's entry in every case but a stale position event.
            toc_ordinal: self.toc.ordinal_of_active().map(|ordinal| ordinal as i64),
            progress: None,
        };
        let overlay = marks::renderer_annotation(&annotation).expect("new EPUB annotation has an EPUB anchor");
        let annotation_id = annotation.id.clone();
        self.marks.append(annotation, "save reader annotation");
        self.marks.select(annotation_id);
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::UpsertAnnotation(overlay), cx));
        crate::invalidation::notify(cx, Component::ReaderShell, "annotation_created");
    }

    pub(in crate::epub) fn copy_selection_citation(&self, cx: &mut Context<Self>) {
        let title = self.display_title();
        let cfi = self.selection.as_ref().map(|selection| selection.cfi_range.clone()).or_else(|| self.current_cfi.clone());
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::CopySelectionCitation { title, cfi }, cx));
    }

    pub(in crate::epub) fn select_annotation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let note = self.marks.select(id);
        self.annotation_note_input.update(cx, |input, cx| input.set_value(note, window, cx));
        crate::invalidation::notify(cx, Component::ReaderShell, "annotation_selected");
    }

    pub(in crate::epub) fn close_annotation_popup(&mut self, cx: &mut Context<Self>) {
        if self.marks.clear_selection() {
            self.annotation_popup_anchor = None;
            crate::invalidation::notify(cx, Component::ReaderShell, "annotation_closed");
        }
    }

    pub(in crate::epub) fn navigate_to_cfi(&self, cfi: String, cx: &mut Context<Self>) {
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::SetCfiPosition(Some(cfi)), cx));
    }

    /// Writes the open annotation back while leaving the editor open.
    pub(in crate::epub) fn save_selected_annotation(&mut self, style: Option<AnnotationStyle>, color: Option<&'static str>, cx: &mut Context<Self>) {
        let note = self.annotation_note_input.read(cx).value().to_string();
        let Some(annotation) = self.marks.update_selected(note, style, color, "update reader annotation") else { return };
        if let Some(overlay) = marks::renderer_annotation(&annotation) {
            self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::UpsertAnnotation(overlay), cx));
        }
        crate::invalidation::notify(cx, Component::ReaderShell, "annotation_updated");
    }

    pub(in crate::epub) fn delete_annotation(&mut self, id: String, cx: &mut Context<Self>) {
        self.marks.delete(id.clone(), "delete reader annotation");
        self.annotation_popup_anchor = None;
        self.with_renderer(cx, |renderer, cx| renderer.apply(RendererCommand::RemoveAnnotation(id), cx));
        crate::invalidation::notify(cx, Component::ReaderShell, "annotation_deleted");
    }
}
