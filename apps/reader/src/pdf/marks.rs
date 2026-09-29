//! PDF projection of shared reader annotations.

use book_model::AnnotationStyle;
use library_backend::ReaderAnnotation;
use pdf_reader_core::NormalizedRect;
use pdf_view_gpui::{PdfAnnotationOverlay, PdfAnnotationStyle};

pub(super) fn annotation_overlays(annotations: &[ReaderAnnotation]) -> Vec<PdfAnnotationOverlay> {
    annotations
        .iter()
        .filter(|annotation| !annotation.is_external_pdf_annotation())
        .filter_map(|annotation| {
            let anchor = annotation.pdf_anchor()?;
            let style = match annotation.style {
                AnnotationStyle::Highlight => PdfAnnotationStyle::Highlight,
                AnnotationStyle::Underline => PdfAnnotationStyle::Underline,
                AnnotationStyle::Squiggly => PdfAnnotationStyle::Squiggly,
                AnnotationStyle::Strikethrough => PdfAnnotationStyle::Strikethrough,
            };
            let rects = anchor.rects.into_iter().map(|rect| NormalizedRect::new(rect.left, rect.top, rect.width, rect.height)).collect();
            Some(PdfAnnotationOverlay { id: annotation.id.clone(), page_index: anchor.page_index as usize, rects, style, color: annotation_color(&annotation.color, style) })
        })
        .collect()
}

fn annotation_color(value: &str, style: PdfAnnotationStyle) -> u32 {
    let rgb = value.strip_prefix('#').and_then(|hex| u32::from_str_radix(hex, 16).ok()).filter(|_| value.len() == 7).unwrap_or(ui_components::READER_ANNOTATION_DEFAULT_RGB);
    let alpha = if style == PdfAnnotationStyle::Highlight { 0x60 } else { 0xff };
    (rgb << 8) | alpha
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_colors_use_translucent_highlights_and_opaque_rules() {
        assert_eq!(annotation_color("#f6c945", PdfAnnotationStyle::Highlight), 0xf6c94560);
        assert_eq!(annotation_color("#f6c945", PdfAnnotationStyle::Underline), 0xf6c945ff);
        assert_eq!(annotation_color("invalid", PdfAnnotationStyle::Highlight), 0xf6c94560);
    }
}

pub(super) fn imported_annotations(content_hash: content_address::ContentHash, metadata: Option<&pdf_reader_core::PdfReaderMetadata>) -> Vec<ReaderAnnotation> {
    use book_model::{AnnotationAnchor, PdfAnnotationAnchor, PdfAnnotationRect};
    use pdf_reader_core::PdfStoredAnnotationKind as Kind;
    let Some(metadata) = metadata.filter(|m| m.valid_for(&m.checksum)) else { return Vec::new() };
    metadata
        .annotations
        .iter()
        .map(|a| ReaderAnnotation {
            id: format!("pdf-external:{}:{}:{}", metadata.checksum, a.page_index, a.index),
            content_hash,
            anchor: AnnotationAnchor::pdf(PdfAnnotationAnchor::new(a.page_index as u32, a.rects.iter().map(|r| PdfAnnotationRect::new(r[0], r[1], r[2], r[3])).collect(), None)),
            exact_text: a.text.clone(),
            style: match a.kind {
                Kind::Highlight | Kind::Note => AnnotationStyle::Highlight,
                Kind::Underline => AnnotationStyle::Underline,
                Kind::Squiggly => AnnotationStyle::Squiggly,
                Kind::Strikethrough => AnnotationStyle::Strikethrough,
            },
            color: a.color.clone(),
            note: a.note.clone(),
            created_at: 0,
            modified_at: 0,
            toc_ordinal: None,
            progress: None,
        })
        .collect()
}
