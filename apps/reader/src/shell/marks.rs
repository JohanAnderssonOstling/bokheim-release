//! What the reader has annotated in a book.
//!
//! The library owns the durable copy; this is the in-memory mirror kept in
//! step with it. Every reader that can annotate keeps the same mirror, so it
//! lives here rather than beside one of them. How a mark is anchored, and how
//! it becomes something painted over the page, is the reader's own business.

use std::collections::HashSet;
use std::rc::Rc;

use book_model::AnnotationStyle;
use gpui::App;
use library_backend::LibraryClient;
use library_backend::ReaderAnnotation;

use crate::shell::persistence::MutationWriter;

#[derive(Default)]
struct MarksState {
    annotations: Vec<ReaderAnnotation>,
    /// The annotation whose note editor is open, if any.
    selected_id: Option<String>,
    deleted_annotation_ids: HashSet<String>,
}

impl MarksState {
    fn annotation(&self, id: &str) -> Option<&ReaderAnnotation> {
        self.annotations.iter().find(|annotation| annotation.id == id)
    }

    /// Replaces a stored annotation in place, or does nothing if it is gone.
    fn replace(&mut self, annotation: ReaderAnnotation) {
        if let Some(existing) = self.annotations.iter_mut().find(|item| item.id == annotation.id) {
            *existing = annotation;
        }
    }

    /// Merges an asynchronous load into marks already changed optimistically.
    /// Local entries win duplicate ids, while local deletions must not be
    /// resurrected by a load that started before the deletion.
    fn merge_loaded(&mut self, annotations: Vec<ReaderAnnotation>) {
        let local_annotation_ids = self.annotations.iter().map(|annotation| annotation.id.clone()).collect::<HashSet<_>>();
        self.annotations.extend(annotations.into_iter().filter(|annotation| !local_annotation_ids.contains(annotation.id.as_str()) && !self.deleted_annotation_ids.contains(&annotation.id)));
    }

    /// Forgets an annotation, also closing its editor if it was open.
    fn remove_annotation(&mut self, id: &str) -> bool {
        let closed_editor = self.selected_id.as_deref() == Some(id);
        self.deleted_annotation_ids.insert(id.to_owned());
        self.annotations.retain(|item| item.id != id);
        if closed_editor {
            self.selected_id = None;
        }
        closed_editor
    }
}

/// Annotation state plus its serialized durable mutation queue.
///
/// Readers still decide how annotations are anchored and painted, but the
/// optimistic in-memory mutation and matching backend write happen together.
pub(crate) struct MarkSession {
    state: MarksState,
    writes: MutationWriter,
}

impl MarkSession {
    pub(crate) fn new(cx: &App, library: Rc<LibraryClient>) -> Self {
        Self { state: MarksState::default(), writes: MutationWriter::new(cx, library) }
    }

    pub(crate) fn merge_loaded(&mut self, annotations: Vec<ReaderAnnotation>) {
        self.state.merge_loaded(annotations);
    }

    pub(crate) fn annotations(&self) -> &[ReaderAnnotation] {
        &self.state.annotations
    }

    pub(crate) fn annotation(&self, id: &str) -> Option<&ReaderAnnotation> {
        self.state.annotation(id)
    }

    pub(crate) fn selected(&self) -> Option<&ReaderAnnotation> {
        self.state.selected_id.as_deref().and_then(|id| self.state.annotation(id))
    }

    pub(crate) fn selected_id(&self) -> Option<&str> {
        self.state.selected_id.as_deref()
    }

    /// Selects an annotation and returns the note the UI should edit.
    pub(crate) fn select(&mut self, id: String) -> String {
        let note = self.state.annotation(&id).map(|annotation| annotation.note.clone()).unwrap_or_default();
        self.state.selected_id = Some(id);
        note
    }

    pub(crate) fn clear_selection(&mut self) -> bool {
        self.state.selected_id.take().is_some()
    }

    pub(crate) fn append(&mut self, annotation: ReaderAnnotation, what: &'static str) {
        self.state.annotations.push(annotation.clone());
        self.persist(annotation, what);
    }

    pub(crate) fn prepend(&mut self, annotation: ReaderAnnotation, what: &'static str) {
        self.state.annotations.insert(0, annotation.clone());
        self.persist(annotation, what);
    }

    fn update(&mut self, annotation: ReaderAnnotation, what: &'static str) {
        self.state.replace(annotation.clone());
        self.persist(annotation, what);
    }

    /// Updates and persists the annotation currently selected by the UI.
    pub(crate) fn update_selected(&mut self, note: String, style: Option<AnnotationStyle>, color: Option<&str>, what: &'static str) -> Option<ReaderAnnotation> {
        let id = self.state.selected_id.clone()?;
        let mut annotation = self.state.annotation(&id)?.clone();
        annotation.note = note;
        if let Some(style) = style {
            annotation.style = style;
        }
        if let Some(color) = color {
            annotation.color = color.to_owned();
        }
        annotation.modified_at = now_millis();
        self.update(annotation.clone(), what);
        Some(annotation)
    }

    /// Deletes an annotation and reports whether its editor was closed.
    pub(crate) fn delete(&mut self, id: String, what: &'static str) -> bool {
        let closed_editor = self.state.remove_annotation(&id);
        let modified_at = now_millis();
        self.writes.record(what, move |library| {
            let library = library.clone();
            Box::pin(async move { library.delete_annotation(id, modified_at).await })
        });
        closed_editor
    }

    fn persist(&self, annotation: ReaderAnnotation, what: &'static str) {
        self.writes.record(what, move |library| {
            let library = library.clone();
            Box::pin(async move { library.upsert_annotation(annotation).await })
        });
    }
}

pub(crate) fn now_millis() -> i64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::AnnotationAnchor;

    const TEST_CONTENT_HASH: &str = concat!("00000000000000000000000000000000", "00000000000000000000000000000001");

    fn annotation(id: &str) -> ReaderAnnotation {
        ReaderAnnotation {
            id: id.to_owned(),
            content_hash: sync_common::ContentHash::new(TEST_CONTENT_HASH),
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/4!/4/2,/1:0,/1:8)".to_owned()),
            exact_text: "a furrow".to_owned(),
            style: AnnotationStyle::Highlight,
            color: ui_components::READER_ANNOTATION_DEFAULT_COLOR.to_owned(),
            note: String::new(),
            created_at: 0,
            modified_at: 0,
            toc_ordinal: None,
            progress: None,
        }
    }

    fn annotation_with_note(id: &str, note: &str) -> ReaderAnnotation {
        ReaderAnnotation { note: note.to_owned(), ..annotation(id) }
    }

    #[test]
    fn deleting_the_open_annotation_closes_its_editor() {
        let mut marks = MarksState { annotations: vec![annotation("a"), annotation("b")], selected_id: Some("a".to_owned()), ..MarksState::default() };

        marks.remove_annotation("a");

        assert_eq!(marks.annotations.len(), 1);
        assert_eq!(marks.selected_id, None, "the editor must not stay open on a deleted annotation");
    }

    #[test]
    fn deleting_another_annotation_leaves_the_editor_open() {
        let mut marks = MarksState { annotations: vec![annotation("a"), annotation("b")], selected_id: Some("a".to_owned()), ..MarksState::default() };

        marks.remove_annotation("b");

        assert_eq!(marks.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn replacing_a_missing_annotation_adds_nothing() {
        let mut marks = MarksState { annotations: vec![annotation("a")], ..MarksState::default() };

        marks.replace(annotation("gone"));

        assert_eq!(marks.annotations.len(), 1);
    }

    #[test]
    fn a_late_load_preserves_local_changes_and_deletions() {
        let mut marks = MarksState { annotations: vec![annotation_with_note("local", "new")], ..MarksState::default() };
        marks.remove_annotation("deleted");

        marks.merge_loaded(vec![annotation_with_note("local", "stale"), annotation("deleted"), annotation("server")]);

        assert_eq!(marks.annotations.iter().map(|annotation| annotation.id.as_str()).collect::<Vec<_>>(), vec!["local", "server"]);
        assert_eq!(marks.annotation("local").map(|annotation| annotation.note.as_str()), Some("new"));
    }
}
