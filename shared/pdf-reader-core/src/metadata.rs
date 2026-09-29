use super::*;
use pdf_view_common::PdfStoredPage;
use std::sync::Mutex;

struct SharedReader<R>(Arc<Mutex<R>>);
impl<R: Read> Read for SharedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.lock().map_err(|_| std::io::Error::other("PDF reader poisoned"))?.read(buffer)
    }
}
impl<R: Seek> Seek for SharedReader<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().map_err(|_| std::io::Error::other("PDF reader poisoned"))?.seek(position)
    }
}

/// Ingestion only: hashes the exact bytes and inspects page geometry/navigation.
/// Returns the original reader, rewound, without retaining a full-file copy.
/// The ingestion pass: the byte-specific snapshot a later open reuses, and the
/// book's navigation, which is neither byte-specific nor the renderer's
/// to keep. The outline is returned beside the snapshot rather than inside it
/// so a PDF reaches the library in the same navigation form an EPUB does.
pub fn inspect_reader_metadata<R: Read + Seek + 'static>(mut reader: R) -> PdfResult<(R, PdfReaderMetadata, Vec<BookTocEntry>)> {
    reader.seek(SeekFrom::Start(0)).map_err(PdfError::new_io)?;
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result.map_err(PdfError::new_io)?,
        };
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    reader.seek(SeekFrom::Start(0)).map_err(PdfError::new_io)?;
    let shared = Arc::new(Mutex::new(reader));
    let pdfium = runtime::lock();
    let session = PdfDocumentSession::from_reader("ingestion", SharedReader(shared.clone()))?;
    let mut annotations = Vec::new();
    let mut annotation_budget = 512 * 1024;
    let mut skippable_pages = Vec::new();
    let pages = session
        .info
        .pages
        .iter()
        .enumerate()
        .map(|(index, info)| {
            let page = session.document.pages().get(index as i32).map_err(|e| PdfError::new(e.to_string()))?;
            annotations.extend(super::annotations::inspect(&page, index, &mut annotation_budget));
            if super::page_is_skippable(&page) {
                skippable_pages.push(index);
            }
            let rotation = page.rotation().map_err(|e| PdfError::new(e.to_string()))?.as_degrees() as u16;
            Ok(PdfStoredPage { width: info.width_points, height: info.height_points, rotation })
        })
        .collect::<PdfResult<Vec<_>>>()?;
    let outline = document_outline(&session.document);
    let mut metadata = PdfReaderMetadata { version: 1, checksum: hash.finalize().to_hex().to_string(), pages, skippable_pages: Some(skippable_pages), annotations, dependencies: None };
    drop(session);
    drop(pdfium);
    let mut reader = Arc::try_unwrap(shared).map_err(|_| PdfError::new("PDF ingestion reader still in use"))?.into_inner().map_err(|_| PdfError::new("PDF reader poisoned"))?;
    reader.seek(SeekFrom::Start(0)).map_err(PdfError::new_io)?;
    metadata.dependencies = pdf_range_reader::dependency_index(&mut reader).ok().filter(|d| d.valid(metadata.pages.len()));
    reader.seek(SeekFrom::Start(0)).map_err(PdfError::new_io)?;
    metadata.bound_optional_data();
    Ok((reader, metadata, outline))
}

impl PdfError {
    fn new_io(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl PdfDocumentSession {
    /// The caller supplies the checksum of the opened source, never the stable
    /// book identity. Invalid, old, or missing metadata uses normal inspection.
    pub fn from_reader_with_metadata<R: Read + Seek + 'static>(source_name: impl Into<String>, reader: R, checksum: &str, metadata: Option<PdfReaderMetadata>) -> PdfResult<Self> {
        let _pdfium = runtime::lock();
        let source_name = source_name.into();
        let Some(metadata) = metadata.filter(|m| m.valid_for(checksum)) else {
            return Self::from_reader(source_name, reader);
        };
        let document = load_document(reader).map_err(|e| PdfError::new(e.to_string()))?;
        if document.pages().len() as usize != metadata.pages.len() {
            return Self::from_document(source_name, document);
        }
        let skippable_pages = metadata.skippable_pages.unwrap_or_default();
        let pages = metadata.pages.into_iter().map(|p| PdfPageInfo { width_points: p.width, height_points: p.height }).collect();
        // Page geometry is cached because reading it costs one call per page.
        // The outline is not: it is one walk of a tree the document already
        // holds, and keeping a second copy of the library's navigation only to
        // save that walk is what this stopped being.
        let outline = document_outline(&document);
        Ok(Self { document, info: PdfDocumentInfo { source_name, pages, outline, skippable_pages }, analysis: RefCell::new(PageAnalysisCache::default()), page_source: None })
    }
}

impl PdfDocumentSession {
    pub fn from_reader_with_page_source<R: Read + Seek + 'static>(source_name: impl Into<String>, reader: R, checksum: &str, metadata: Option<PdfReaderMetadata>, page_source: Option<Arc<dyn PdfPageSource>>) -> PdfResult<Self> {
        if let Some(source) = &page_source {
            source.prepare_startup().map_err(PdfError::new)?;
        }
        let already_classified = metadata.as_ref().is_some_and(|value| value.valid_for(checksum) && value.skippable_pages.is_some());
        let mut session = Self::from_reader_with_metadata(source_name, reader, checksum, metadata)?;
        session.page_source = page_source;
        if !already_classified {
            let pages = session.inspect_skippable_pages();
            session.set_skippable_pages(pages);
        }
        Ok(session)
    }
}
