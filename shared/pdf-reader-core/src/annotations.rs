use super::*;
use pdf_view_common::{PdfStoredAnnotation, PdfStoredAnnotationKind as Kind};

pub(super) fn inspect(page: &PdfPage<'_>, page_index: usize, budget: &mut usize) -> Vec<PdfStoredAnnotation> {
    let config = PdfRenderConfig::new().set_target_size(10000, 10000);
    let normalized = |bounds: PdfRect| -> Option<[f32; 4]> {
        let a = page.points_to_pixels(bounds.left(), bounds.bottom(), &config).ok()?;
        let b = page.points_to_pixels(bounds.right(), bounds.top(), &config).ok()?;
        let left = (a.0.min(b.0) as f32 / 10000.0).clamp(0.0, 1.0);
        let top = (a.1.min(b.1) as f32 / 10000.0).clamp(0.0, 1.0);
        let right = (a.0.max(b.0) as f32 / 10000.0).clamp(0.0, 1.0);
        let bottom = (a.1.max(b.1) as f32 / 10000.0).clamp(0.0, 1.0);
        (right > left && bottom > top).then_some([left, top, right - left, bottom - top])
    };
    page.annotations()
        .iter()
        .enumerate()
        .take(16384)
        .filter_map(|(index, a)| {
            if *budget < 256 {
                return None;
            }
            if a.name().is_some_and(|name| name.starts_with("bokheim:")) {
                return None;
            }
            let kind = match a.annotation_type() {
                PdfPageAnnotationType::Highlight => Kind::Highlight,
                PdfPageAnnotationType::Underline => Kind::Underline,
                PdfPageAnnotationType::Squiggly => Kind::Squiggly,
                PdfPageAnnotationType::Strikeout => Kind::Strikethrough,
                PdfPageAnnotationType::Text => Kind::Note,
                _ => return None,
            };
            let mut bounds = a.attachment_points().iter().map(|quad| quad.to_rect()).take(4096).collect::<Vec<_>>();
            if bounds.is_empty() {
                bounds.push(a.bounds().ok()?);
            }
            let rects = bounds.iter().filter_map(|r| normalized(*r)).collect::<Vec<_>>();
            if rects.is_empty() {
                return None;
            }
            let text = if kind == Kind::Note {
                String::new()
            } else {
                page.text()
                    .ok()
                    .map(|text| {
                        let mut selected = String::new();
                        for bounds in &bounds {
                            if !selected.is_empty() {
                                selected.push(' ');
                            }
                            selected.push_str(&bounded(text.inside_rect(*bounds)));
                            if selected.len() >= 16384 {
                                break;
                            }
                        }
                        bounded(selected)
                    })
                    .unwrap_or_default()
            };
            let color = a.fill_color().or_else(|_| a.stroke_color()).map(|c| format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())).unwrap_or_else(|_| "#f6c945".into());
            let note = bounded(a.contents().unwrap_or_default());
            // Includes worst-case JSON escaping; bound accumulation during ingestion,
            // before the final synchronized snapshot is serialized.
            let cost = 256 + (text.len() + note.len()) * 6 + rects.len() * 128;
            if cost > *budget {
                return None;
            }
            *budget -= cost;
            Some(PdfStoredAnnotation { page_index, index, kind, rects, color, text, note })
        })
        .take(16384)
        .collect()
}

fn bounded(mut text: String) -> String {
    if text.len() > 16384 {
        let mut end = 16384;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
