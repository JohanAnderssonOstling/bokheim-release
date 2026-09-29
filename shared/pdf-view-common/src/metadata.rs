//! Ingestion output tied to the exact PDF bytes, independent of a renderer.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfReaderMetadata {
    pub version: u32,
    pub checksum: String,
    pub pages: Vec<PdfStoredPage>,
    /// Zero-based pages containing no content or only a blank-page notice.
    /// None denotes metadata written before page classification was available.
    #[serde(default)]
    pub skippable_pages: Option<Vec<usize>>,
    #[serde(default)]
    pub annotations: Vec<PdfStoredAnnotation>,
    #[serde(default)]
    pub dependencies: Option<PdfDependencyIndex>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfStoredPage {
    /// Display dimensions, with the page's intrinsic rotation applied.
    pub width: f32,
    pub height: f32,
    pub rotation: u16,
}

impl PdfReaderMetadata {
    pub fn valid_for(&self, checksum: &str) -> bool {
        self.version == 1
            && self.checksum == checksum
            && checksum.len() == 64
            && checksum.bytes().all(|c| c.is_ascii_hexdigit())
            && !self.pages.is_empty()
            && self.pages.len() <= 32767
            && self.pages.iter().all(|p| p.width.is_finite() && p.height.is_finite() && p.width > 0.0 && p.height > 0.0 && matches!(p.rotation, 0 | 90 | 180 | 270))
            && self.skippable_pages.as_ref().is_none_or(|pages| pages.windows(2).all(|pair| pair[0] < pair[1]) && pages.iter().all(|&page| page < self.pages.len()))
            && self.annotations.len() <= 16384
            && self.annotations.iter().all(|a| a.valid(self.pages.len()))
            && self.dependencies.as_ref().is_none_or(|d| d.valid(self.pages.len()))
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 7 * 512 * 1024)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfStoredAnnotation {
    pub page_index: usize,
    pub index: usize,
    pub kind: PdfStoredAnnotationKind,
    /// Rectangles in the same normalized display coordinates as text selection.
    pub rects: Vec<[f32; 4]>,
    pub color: String,
    pub text: String,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PdfStoredAnnotationKind {
    Highlight,
    Underline,
    Squiggly,
    Strikethrough,
    Note,
}

impl PdfStoredAnnotation {
    fn valid(&self, pages: usize) -> bool {
        self.page_index < pages
            && self.rects.len() <= 4096
            && !self.rects.is_empty()
            && self.rects.iter().all(|r| r.iter().all(|v| v.is_finite()) && r[0] >= 0.0 && r[1] >= 0.0 && r[2] > 0.0 && r[3] > 0.0 && r[0] + r[2] <= 1.0001 && r[1] + r[3] <= 1.0001)
            && self.text.len() <= 16384
            && self.note.len() <= 16384
            && self.color.len() == 7
            && self.color.starts_with('#')
            && self.color[1..].bytes().all(|c| c.is_ascii_hexdigit())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PdfByteRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PdfDependencyIndex {
    pub length: u64,
    pub startup: Vec<PdfByteRange>,
    pub pages: Vec<Vec<PdfByteRange>>,
}

impl PdfDependencyIndex {
    pub fn valid(&self, pages: usize) -> bool {
        self.length > 0
            && self.pages.len() == pages
            && self.startup.len() + self.pages.iter().map(Vec::len).sum::<usize>() <= 100_000
            && self.startup.iter().chain(self.pages.iter().flatten()).all(|r| r.length > 0 && r.offset.checked_add(r.length).is_some_and(|end| end <= self.length))
    }
}

/// Runs on the document worker before PDFium touches the requested page.
/// Implementations share their bounded byte cache with the seekable reader.
pub trait PdfPageSource: Send + Sync {
    fn prepare_startup(&self) -> Result<(), String>;
    fn prepare_page(&self, page: usize) -> Result<(), String>;
}

impl PdfReaderMetadata {
    /// Keep each synchronized snapshot below the mutation envelope limit.
    pub fn bound_optional_data(&mut self) {
        let mut remaining = 512 * 1024usize;
        self.annotations.truncate(16384);
        self.annotations.retain(|a| {
            let size = serde_json::to_vec(a).map_or(usize::MAX, |bytes| bytes.len());
            if size > remaining {
                false
            } else {
                remaining -= size;
                true
            }
        });
        if self.dependencies.as_ref().is_some_and(|d| serde_json::to_vec(d).map_or(true, |bytes| bytes.len() > 1024 * 1024)) {
            self.dependencies = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_snapshots_default_optional_fields_and_large_notes_stay_syncable() {
        let checksum = "a".repeat(64);
        let mut metadata: PdfReaderMetadata = serde_json::from_value(serde_json::json!({
            "version":1,"checksum":checksum,"pages":[{"width":600,"height":800,"rotation":0}],"outline":[]
        }))
        .unwrap();
        assert!(metadata.valid_for(&checksum));
        let note = PdfStoredAnnotation { page_index: 0, index: 0, kind: PdfStoredAnnotationKind::Note, rects: vec![[0.1, 0.1, 0.2, 0.1]], color: "#abcdef".into(), text: String::new(), note: "\n".repeat(16384) };
        metadata.annotations = vec![note; 200];
        assert!(!metadata.valid_for(&checksum));
        metadata.bound_optional_data();
        assert!(!metadata.annotations.is_empty());
        assert!(metadata.valid_for(&checksum));
        assert!(serde_json::to_vec(&metadata).unwrap().len() < 4 * 1024 * 1024);
    }
}
