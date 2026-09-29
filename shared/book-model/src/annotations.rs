use crate::UnixMillis;
use content_address::ContentHash;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationStyle {
    Highlight,
    Underline,
    Squiggly,
    Strikethrough,
}

impl AnnotationStyle {
    pub const fn storage(&self) -> &'static str {
        match self {
            Self::Highlight => "highlight",
            Self::Underline => "underline",
            Self::Squiggly => "squiggly",
            Self::Strikethrough => "strikethrough",
        }
    }

    pub fn parse_storage(value: &str) -> Option<Self> {
        match value {
            "highlight" => Some(Self::Highlight),
            "underline" => Some(Self::Underline),
            "squiggly" => Some(Self::Squiggly),
            "strikethrough" => Some(Self::Strikethrough),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct PdfAnnotationRect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl PdfAnnotationRect {
    pub const fn new(left: f32, top: f32, width: f32, height: f32) -> Self {
        Self { left, top, width, height }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PdfAnnotationAnchor {
    pub page_index: u32,
    pub rects: Vec<PdfAnnotationRect>,
    pub fallback_cfi: Option<String>,
}

impl PdfAnnotationAnchor {
    pub fn new(page_index: u32, rects: Vec<PdfAnnotationRect>, fallback_cfi: Option<String>) -> Self {
        Self { page_index, rects, fallback_cfi }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnotationAnchor {
    EpubCfi { cfi: String },
    Pdf { page_index: u32, rects: Vec<PdfAnnotationRect>, fallback_cfi: Option<String> },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "AnnotationAnchor")]
enum AnnotationAnchorBinary {
    EpubCfi { cfi: String },
    Pdf { page_index: u32, rects: Vec<PdfAnnotationRect>, fallback_cfi: Option<String> },
}

mod annotation_anchor_binary {
    use super::{AnnotationAnchor, AnnotationAnchorBinary};
    pub fn serialize<S: serde::Serializer>(value: &AnnotationAnchor, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serde::Serialize::serialize(value, serializer)
        } else {
            AnnotationAnchorBinary::serialize(value, serializer)
        }
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<AnnotationAnchor, D::Error> {
        if deserializer.is_human_readable() {
            serde::Deserialize::deserialize(deserializer)
        } else {
            AnnotationAnchorBinary::deserialize(deserializer)
        }
    }
}

impl AnnotationAnchor {
    pub fn epub_cfi(cfi: impl Into<String>) -> Self {
        Self::EpubCfi { cfi: cfi.into() }
    }
    pub fn pdf(anchor: PdfAnnotationAnchor) -> Self {
        Self::Pdf { page_index: anchor.page_index, rects: anchor.rects, fallback_cfi: anchor.fallback_cfi }
    }
    pub fn epub_cfi_value(&self) -> Option<&str> {
        match self {
            Self::EpubCfi { cfi } => Some(cfi),
            Self::Pdf { fallback_cfi, .. } => fallback_cfi.as_deref(),
        }
    }
    pub fn pdf_anchor(&self) -> Option<PdfAnnotationAnchor> {
        match self {
            Self::EpubCfi { .. } => None,
            Self::Pdf { page_index, rects, fallback_cfi } => Some(PdfAnnotationAnchor::new(*page_index, rects.clone(), fallback_cfi.clone())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnnotationState {
    pub content_hash: ContentHash,
    #[serde(with = "annotation_anchor_binary")]
    pub anchor: AnnotationAnchor,
    pub exact_text: String,
    pub style: AnnotationStyle,
    pub color: String,
    pub note: String,
    pub created_at: UnixMillis,
    pub modified_at: UnixMillis,
    pub deleted: bool,
    /// Depth-first table-of-contents index locating the annotation for
    /// reading-order display. Derived at write time; `None` sorts last.
    #[serde(default)]
    pub toc_ordinal: Option<i64>,
    /// Position through the book on the shared 0–100 scale. Derived at
    /// write time; `None` sorts after positioned rows.
    #[serde(default)]
    pub progress: Option<f32>,
}

impl AnnotationState {
    /// Stored JSON detail blob, including the positioning fields. The row
    /// columns are a cached projection of this blob for ordering.
    pub fn detail_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&StoredAnnotationDetail::from(self))
    }

    pub fn from_detail(content_hash: ContentHash, detail: &[u8], modified_at: i64, deleted: bool) -> Result<Self, serde_json::Error> {
        let detail = StoredAnnotationDetail::decode(detail)?;
        Ok(Self {
            content_hash,
            anchor: detail.anchor,
            exact_text: detail.exact_text,
            style: detail.style,
            color: detail.color,
            note: detail.note,
            created_at: detail.created_at.max(0) as u64,
            modified_at: modified_at.max(0) as u64,
            deleted,
            toc_ordinal: detail.toc_ordinal,
            progress: detail.progress,
        })
    }
}

/// Opaque stored form of one annotation's detail. `modified_at` stays a row
/// column (it changes on delete, when the blob is untouched); everything else
/// the row projects for ordering also rides in the blob so sync decodes it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredAnnotationDetail {
    pub anchor: AnnotationAnchor,
    pub exact_text: String,
    pub style: AnnotationStyle,
    pub color: String,
    pub note: String,
    pub created_at: i64,
    #[serde(default)]
    pub toc_ordinal: Option<i64>,
    #[serde(default)]
    pub progress: Option<f32>,
}

impl StoredAnnotationDetail {
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

impl From<&AnnotationState> for StoredAnnotationDetail {
    fn from(value: &AnnotationState) -> Self {
        Self {
            anchor: value.anchor.clone(),
            exact_text: value.exact_text.clone(),
            style: value.style.clone(),
            color: value.color.clone(),
            note: value.note.clone(),
            created_at: i64::try_from(value.created_at).unwrap_or(i64::MAX),
            toc_ordinal: value.toc_ordinal,
            progress: value.progress,
        }
    }
}

/// Position through the book on the shared 0–100 progress scale. Returns
/// `None` for empty totals or out-of-range parts instead of clamping silently.
pub fn annotation_progress(part: f64, total: f64) -> Option<f32> {
    if !part.is_finite() || !total.is_finite() || total <= 0.0 || part < 0.0 || part > total {
        return None;
    }
    Some((part / total * 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_rejects_degenerate_parts() {
        assert_eq!(annotation_progress(55.53, 100.0), Some(55.53));
        assert_eq!(annotation_progress(0.0, 100.0), Some(0.0));
        assert_eq!(annotation_progress(100.0, 100.0), Some(100.0));
        assert_eq!(annotation_progress(1.0, 0.0), None);
        assert_eq!(annotation_progress(-1.0, 100.0), None);
        assert_eq!(annotation_progress(101.0, 100.0), None);
        assert_eq!(annotation_progress(f64::NAN, 100.0), None);
    }

    #[test]
    fn detail_blob_round_trips_positioning() {
        let state = AnnotationState {
            content_hash: ContentHash::new(&"a".repeat(64)),
            anchor: AnnotationAnchor::epub_cfi("epubcfi(/6/2)"),
            exact_text: "Text".to_owned(),
            style: AnnotationStyle::Underline,
            color: "#fff".to_owned(),
            note: "note".to_owned(),
            created_at: 7,
            modified_at: 9,
            deleted: false,
            toc_ordinal: Some(5),
            progress: Some(42.5),
        };
        let decoded = AnnotationState::from_detail(state.content_hash.clone(), &state.detail_json().unwrap(), 9, false).unwrap();
        assert_eq!(decoded, state);
    }
}
