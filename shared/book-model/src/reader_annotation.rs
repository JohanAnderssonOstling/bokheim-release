use serde::{Deserialize, Serialize};

use crate::{AnnotationAnchor, AnnotationState, AnnotationStyle, PdfAnnotationAnchor, StoredAnnotationDetail};
use content_address::ContentHash;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReaderAnnotation {
    pub id: String,
    pub content_hash: ContentHash,
    pub anchor: AnnotationAnchor,
    pub exact_text: String,
    pub style: AnnotationStyle,
    pub color: String,
    pub note: String,
    pub created_at: i64,
    pub modified_at: i64,
    pub toc_ordinal: Option<i64>,
    pub progress: Option<f32>,
}

pub const EXTERNAL_PDF_ANNOTATION_ID_PREFIX: &str = "pdf-external:";

impl ReaderAnnotation {
    pub fn is_external_pdf_annotation(&self) -> bool {
        self.id.starts_with(EXTERNAL_PDF_ANNOTATION_ID_PREFIX)
    }

    pub fn pdf_anchor(&self) -> Option<PdfAnnotationAnchor> {
        self.anchor.pdf_anchor()
    }

    pub fn validate(&self, deleted: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.toc_ordinal.is_some_and(|ordinal| ordinal < 0) {
            return Err("annotation TOC ordinal must be non-negative".into());
        }
        if self.progress.is_some_and(|progress| !progress.is_finite() || !(0.0..=100.0).contains(&progress)) {
            return Err("annotation progress must be finite and between 0 and 100".into());
        }
        self.to_sync_state(deleted).map(|_| ())
    }

    pub fn to_sync_state(&self, deleted: bool) -> Result<AnnotationState, Box<dyn std::error::Error + Send + Sync>> {
        Ok(AnnotationState {
            content_hash: self.content_hash.clone(),
            anchor: self.anchor.clone(),
            exact_text: self.exact_text.clone(),
            style: self.style.clone(),
            color: self.color.clone(),
            note: self.note.clone(),
            created_at: u64::try_from(self.created_at)?,
            modified_at: u64::try_from(self.modified_at)?,
            deleted,
            toc_ordinal: self.toc_ordinal,
            progress: self.progress,
        })
    }

    pub fn detail_json(&self) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.to_sync_state(false)?.detail_json()?)
    }

    /// Rebuilds a stored annotation. Positioning (`toc_ordinal`, `progress`)
    /// and `modified_at` come from the row columns used for ordering; the
    /// remaining detail comes from the blob.
    pub fn from_row(id: String, content_hash: ContentHash, toc_ordinal: Option<i64>, progress: Option<f32>, modified_at: i64, detail: &[u8]) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let detail = StoredAnnotationDetail::decode(detail)?;
        Ok(Self { id, content_hash, anchor: detail.anchor, exact_text: detail.exact_text, style: detail.style, color: detail.color, note: detail.note, created_at: detail.created_at, modified_at, toc_ordinal, progress })
    }
}
