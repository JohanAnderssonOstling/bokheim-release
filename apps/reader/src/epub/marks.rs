//! The EPUB half of marking up a book: the text selection a new highlight
//! would be made from, and the translation from a stored annotation to the
//! overlay the renderer paints.
//!
//! The marks themselves are shared with the other readers and live in
//! [`crate::shell::marks`]. What is here depends on EPUB CFIs, which no other
//! reader has.

use book_model::AnnotationStyle;
use html_view_core::{AnnotationStyle as RendererAnnotationStyle, RendererAnnotation};
use library_backend::ReaderAnnotation;

use crate::epub::Selection;

/// Highlight fill used when an annotation's colour cannot be parsed.
const DEFAULT_HIGHLIGHT: [u8; 4] = [(ui_components::READER_ANNOTATION_DEFAULT_RGB >> 16) as u8, (ui_components::READER_ANNOTATION_DEFAULT_RGB >> 8) as u8, ui_components::READER_ANNOTATION_DEFAULT_RGB as u8, HIGHLIGHT_ALPHA];

/// Highlights are drawn over text, so they are always translucent.
const HIGHLIGHT_ALPHA: u8 = 96;

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
pub(crate) fn overlays(annotations: &[ReaderAnnotation]) -> Vec<RendererAnnotation> {
    annotations.iter().filter_map(renderer_annotation).collect()
}

/// Translates a stored annotation into a renderer overlay.
///
/// Returns `None` for annotations anchored to something other than an EPUB CFI
/// — PDF annotations share the same table and must be skipped here.
pub(crate) fn renderer_annotation(annotation: &ReaderAnnotation) -> Option<RendererAnnotation> {
    let cfi_range = annotation.anchor.epub_cfi_value()?.to_owned();
    let style = match annotation.style {
        AnnotationStyle::Highlight => RendererAnnotationStyle::Highlight,
        AnnotationStyle::Underline => RendererAnnotationStyle::Underline,
        AnnotationStyle::Squiggly => RendererAnnotationStyle::Squiggly,
        AnnotationStyle::Strikethrough => RendererAnnotationStyle::Strikethrough,
    };
    Some(RendererAnnotation { id: annotation.id.clone(), cfi_range, exact_text: annotation.exact_text.clone(), prefix: None, suffix: None, style, color: highlight_color(&annotation.color) })
}

/// Parses a `#rgb` or `#rrggbb` colour into a translucent RGBA fill, falling
/// back to the default highlight for anything unparseable.
fn highlight_color(color: &str) -> [u8; 4] {
    let Some(hex) = color.strip_prefix('#') else { return DEFAULT_HIGHLIGHT };
    let Ok(value) = u32::from_str_radix(hex, 16) else { return DEFAULT_HIGHLIGHT };
    let (red, green, blue) = match hex.len() {
        // CSS shorthand: each digit is doubled, so #fff is white, not #000fff.
        3 => (((value >> 8) & 0xf) * 0x11, ((value >> 4) & 0xf) * 0x11, (value & 0xf) * 0x11),
        6 => ((value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff),
        _ => return DEFAULT_HIGHLIGHT,
    };
    [red as u8, green as u8, blue as u8, HIGHLIGHT_ALPHA]
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{AnnotationAnchor, AnnotationStyle};

    const TEST_CONTENT_HASH: &str = concat!("00000000000000000000000000000000", "00000000000000000000000000000001");

    fn annotation(id: &str, color: &str) -> ReaderAnnotation {
        ReaderAnnotation {
            id: id.to_owned(),
            content_hash: sync_common::ContentHash::new(TEST_CONTENT_HASH),
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/4!/4/2,/1:0,/1:8)".to_owned()),
            exact_text: "a furrow".to_owned(),
            style: AnnotationStyle::Highlight,
            color: color.to_owned(),
            note: String::new(),
            created_at: 0,
            modified_at: 0,
            toc_ordinal: None,
            progress: None,
        }
    }

    fn selection(text: &str) -> Selection {
        Selection { cfi_range: "epubcfi(/6/4!/4/2,/1:0,/1:8)".to_owned(), text: text.to_owned() }
    }

    #[test]
    fn re_reporting_the_same_selection_does_not_repaint() {
        let mut current = None;
        assert!(set_selection(&mut current, selection("a furrow")));
        assert!(!set_selection(&mut current, selection("a furrow")));
        assert!(set_selection(&mut current, selection("a deeper furrow")));
    }

    #[test]
    fn a_valid_colour_becomes_a_translucent_fill() {
        assert_eq!(highlight_color("#ff8000"), [0xff, 0x80, 0x00, HIGHLIGHT_ALPHA]);
    }

    #[test]
    fn css_shorthand_expands_rather_than_being_read_as_a_dark_colour() {
        assert_eq!(highlight_color("#fff"), [0xff, 0xff, 0xff, HIGHLIGHT_ALPHA]);
        assert_eq!(highlight_color("#f80"), [0xff, 0x88, 0x00, HIGHLIGHT_ALPHA]);
    }

    #[test]
    fn unparseable_colours_fall_back_to_the_default_highlight() {
        assert_eq!(highlight_color("f6c945"), DEFAULT_HIGHLIGHT, "a missing # must not be reinterpreted");
        assert_eq!(highlight_color("#zzz"), DEFAULT_HIGHLIGHT);
        assert_eq!(highlight_color("#ff"), DEFAULT_HIGHLIGHT, "a length that is neither shorthand nor full must not be reinterpreted");
    }

    #[test]
    fn overlays_skip_annotations_the_renderer_cannot_place() {
        let mut pdf_annotation = annotation("pdf", "#f6c945");
        let rects = vec![book_model::PdfAnnotationRect { left: 0.1, top: 0.1, width: 0.2, height: 0.05 }];
        let anchor = book_model::PdfAnnotationAnchor::new(0, rects, None);
        pdf_annotation.anchor = AnnotationAnchor::pdf(anchor);
        let annotations = vec![annotation("epub", "#f6c945"), pdf_annotation];

        let overlays = overlays(&annotations);

        assert_eq!(overlays.len(), 1, "a PDF annotation shares the table but has no EPUB CFI");
        assert_eq!(overlays[0].id, "epub");
    }
}
