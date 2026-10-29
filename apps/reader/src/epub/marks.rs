//! The EPUB half of marking up a book: the text selection a new highlight
//! would be made from, and the translation from a stored annotation to the
//! overlay the renderer paints.
//!
//! The marks themselves are shared with the other readers and live in
//! [`crate::shell::marks`]. What is here depends on EPUB CFIs, which no other
//! reader has.

use book_model::AnnotationStyle;
use skrift_column_paginator::{AnnotationStyle as RendererAnnotationStyle, RendererAnnotation};
use library_backend::Annotation;

use crate::epub::Selection;

/// Records a new selection, reporting whether it differs from the last one so
/// an unchanged re-report does not repaint.
pub(in crate::epub) fn set_selection(current: &mut Option<Selection>, selection: Selection) -> bool {
    if current.as_ref() == Some(&selection) {
        return false;
    }
    *current = Some(selection);
    true
}

/// Overlays for every annotation the renderer can place.
pub(crate) fn overlays(annotations: &[Annotation]) -> Vec<RendererAnnotation> {
    annotations.iter().filter_map(renderer_annotation).collect()
}

/// Translates a stored annotation into a renderer overlay.
///
/// Returns `None` for annotations anchored to something other than an EPUB CFI
/// — PDF annotations share the same table and must be skipped here.
pub(crate) fn renderer_annotation(annotation: &Annotation) -> Option<RendererAnnotation> {
    let cfi_range = annotation.anchor.epub_cfi_value()?.to_owned();
    let style = match annotation.style {
        AnnotationStyle::Highlight => RendererAnnotationStyle::Highlight,
        AnnotationStyle::Underline => RendererAnnotationStyle::Underline,
        AnnotationStyle::Squiggly => RendererAnnotationStyle::Squiggly,
        AnnotationStyle::Strikethrough => RendererAnnotationStyle::Strikethrough,
    };
    let mut color = highlight_color(annotation.color);
    if annotation.style != AnnotationStyle::Highlight {
        color[3] = 255;
    }
    Some(RendererAnnotation { id: annotation.id.clone(), cfi_range, exact_text: annotation.exact_text.clone(), prefix: None, suffix: None, style, color })
}

/// An annotation's `0xRRGGBB` as a translucent RGBA fill. Highlights are drawn
/// over text, so they are always translucent.
fn highlight_color(color: u32) -> [u8; 4] {
    let color = ui_components::reader_annotation_paint_color(color);
    [(color >> 16) as u8, (color >> 8) as u8, color as u8, ui_components::READER_ANNOTATION_HIGHLIGHT_ALPHA]
}
