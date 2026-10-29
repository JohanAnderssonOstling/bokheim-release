//! Parsed containers and renderer-independent reading preparation.

use crate::epub::{EpubProvider, EpubSession};
use crate::fb2::{Fb2Container, Fb2Resources};
use crate::mobi::{MobiContainer, MobiResources};
use crate::profile::{PreparationProfile, measure};
use crate::resource_path::local_resource_uri;
use crate::resources::PreparedResources;
use crate::source::{ReaderFactory, Source};
use crate::{BookReader, DeclaredMetadata, DocumentInspection, TocEntry};
use futures_lite::future::block_on;
use std::future::Future;
use std::io::{self, Read, Seek};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookFormat {
    Epub,
    Mobi,
    Fb2,
}

enum Container {
    Epub(EpubSession),
    Mobi(MobiContainer),
    Fb2(Fb2Container),
}

/// Opening parses container headers or bounded FB2 XML. Text preparation is a
/// separate, consuming step, so scans and covers never decode MOBI text.
pub struct Book {
    container: Container,
}

impl Book {
    pub async fn open<F, R>(format: BookFormat, factory: F) -> io::Result<Self>
    where
        F: Fn() -> R + Send + Sync + 'static,
        R: BookReader + 'static,
    {
        Self::open_source(format, Source::reopenable(ReaderFactory::new(factory))).await
    }

    async fn open_source(format: BookFormat, source: Source) -> io::Result<Self> {
        let container = match format {
            BookFormat::Epub => Container::Epub(EpubSession::open(source).await?),
            BookFormat::Mobi => Container::Mobi(MobiContainer::open(source).await?),
            BookFormat::Fb2 => Container::Fb2(Fb2Container::open(source).await?),
        };
        Ok(Self { container })
    }

    pub fn open_reader<R: Read + Seek + Send + 'static>(format: BookFormat, reader: R) -> io::Result<Self> {
        block_on(Self::open_source(format, Source::from_reader(reader)))
    }

    pub fn metadata(&mut self) -> io::Result<DeclaredMetadata> {
        match &mut self.container {
            Container::Epub(epub) => epub.metadata(),
            Container::Mobi(mobi) => Ok(mobi.metadata()),
            Container::Fb2(fb2) => Ok(fb2.metadata()),
        }
    }

    pub fn toc(&mut self) -> io::Result<Option<Vec<TocEntry>>> {
        block_on(self.toc_async())
    }

    pub async fn toc_async(&mut self) -> io::Result<Option<Vec<TocEntry>>> {
        match &mut self.container {
            Container::Epub(epub) => epub.toc().await,
            Container::Mobi(mobi) => mobi.toc().await,
            Container::Fb2(fb2) => fb2.toc().map(Some),
        }
    }

    /// Package-declared cover art, or MOBI's thumbnail and cover records.
    pub fn preferred_cover_resources(&mut self) -> io::Result<Vec<String>> {
        match &mut self.container {
            Container::Epub(epub) => Ok(epub.declared_cover_image_path()?.into_iter().collect()),
            Container::Mobi(mobi) => Ok(mobi.preferred_covers()),
            Container::Fb2(fb2) => Ok(fb2.covers()),
        }
    }

    /// Compute fallbacks only when declared artwork was unusable. EPUB may
    /// inspect spine markup; MOBI only walks the record directory.
    pub fn fallback_cover_resources(&mut self) -> io::Result<Vec<String>> {
        match &mut self.container {
            Container::Epub(epub) => epub.image_paths_in_reading_order(),
            Container::Mobi(mobi) => Ok(mobi.image_resources()),
            Container::Fb2(fb2) => Ok(fb2.images()),
        }
    }

    pub async fn read_resource(&mut self, uri: &str, limit: u64) -> io::Result<Vec<u8>> {
        let uri = local_resource_uri("", uri)?;
        match &mut self.container {
            Container::Epub(epub) => epub.read_bytes_bounded_async(&uri, limit).await,
            Container::Mobi(mobi) => mobi.read_resource(&uri, limit).await,
            Container::Fb2(fb2) => fb2.read_resource(&uri, limit),
        }
    }

    pub fn read_resource_sync(&mut self, uri: &str, limit: u64) -> io::Result<Vec<u8>> {
        block_on(self.read_resource(uri, limit))
    }

    /// Inspect logical HTML documents from either format. EPUB keeps archive
    /// reads lazy; MOBI decodes and maps its text into virtual documents.
    pub fn inspect_documents(&mut self) -> io::Result<DocumentInspection<'_>> {
        block_on(async {
            match &mut self.container {
                Container::Epub(epub) => {
                    let documents = epub.package.document_uris()?;
                    let toc = epub.toc().await?.unwrap_or_default();
                    Ok(DocumentInspection::from_epub(epub, documents, toc))
                }
                Container::Mobi(mobi) => Ok(DocumentInspection::from_prepared(mobi.prepare_text().await?)),
                Container::Fb2(fb2) => Ok(DocumentInspection::from_prepared(fb2.prepare_text()?)),
            }
        })
    }

    pub async fn prepare_reading(self) -> io::Result<ReadingBook> {
        self.prepare_reading_inner::<false>(&mut PreparationProfile::default()).await
    }

    /// Runs the same preparation with per-stage and per-resource measurements.
    /// EPUB resources include wall time, active polling, and suspended time.
    /// The ordinary `prepare_reading` path does not sample clocks.
    pub async fn prepare_reading_profiled(self) -> io::Result<(ReadingBook, PreparationProfile)> {
        let mut profile = PreparationProfile::default();
        let (reading, total) = measure::<true, _>(self.prepare_reading_inner::<true>(&mut profile)).await;
        profile.total = total;
        reading.map(|reading| (reading, profile))
    }

    async fn prepare_reading_inner<const PROFILE: bool>(self, profile: &mut PreparationProfile) -> io::Result<ReadingBook> {
        let (epub, toc) = match self.container {
            Container::Mobi(mobi) => return mobi.prepare_reading().await,
            Container::Fb2(fb2) => return fb2.prepare_reading(),
            Container::Epub(mut epub) => {
                let (toc, timing) = measure::<PROFILE, _>(epub.toc()).await;
                if PROFILE {
                    profile.record("toc", timing);
                }
                let toc = toc?.unwrap_or_default();
                let (epub, timing) = measure::<PROFILE, _>(epub.into_provider()).await;
                if PROFILE {
                    profile.record("source_promotion", timing);
                }
                (Arc::new(epub?), toc)
            }
        };
        let started = PROFILE.then(Instant::now);
        let mut documents = epub.package.document_uris()?;
        if documents.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "EPUB contains no readable documents"));
        }
        profile.record_sync("spine_and_weights", started);
        let (bytes, timing) = measure::<PROFILE, _>(epub.prepare_text_resources_async::<PROFILE>(&documents, profile)).await;
        if PROFILE {
            profile.record("text_resources", timing);
        }
        let bytes = bytes?;
        documents.retain(|uri| bytes.get(uri).is_some_and(|bytes| !bytes.is_empty()));
        if documents.is_empty() {
            documents = epub.package.fallback_documents().into_iter().filter(|uri| bytes.get(uri).is_some_and(|bytes| !bytes.is_empty())).collect();
        }
        if documents.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "EPUB contains no readable documents"));
        }
        let weights = documents.iter().map(|uri| bytes[uri].len().max(1) as u64).collect();
        let started = PROFILE.then(Instant::now);
        let types = epub.package.resource_media_types(&bytes);
        profile.record_sync("media_types", started);
        let started = PROFILE.then(Instant::now);
        let reading = PreparedText { resources: PreparedResources::new(bytes, Some(types), documents), document_weights: weights, toc }.into_reading(ResourceSource::Epub(epub));
        profile.record_sync("finish", started);
        Ok(reading)
    }
}

/// The common output of format-specific document preparation.
pub(crate) struct PreparedText {
    pub(crate) resources: PreparedResources,
    pub(crate) document_weights: Vec<u64>,
    pub(crate) toc: Vec<TocEntry>,
}

impl PreparedText {
    pub(crate) fn into_reading(self, source: ResourceSource) -> ReadingBook {
        ReadingBook { resources: self.resources, document_weights: self.document_weights, source, toc: self.toc }
    }
}

/// Both formats retain immutable containers for on-demand binary resources.
#[derive(Clone)]
pub(crate) enum ResourceSource {
    Epub(Arc<EpubProvider>),
    Mobi(Arc<MobiResources>),
    Fb2(Arc<Fb2Resources>),
}

/// Immutable documents and CSS for synchronous consumers, with asynchronous
/// access to remaining container resources. No rendering engine dependency.
pub struct ReadingBook {
    pub resources: PreparedResources,
    pub document_weights: Vec<u64>,
    source: ResourceSource,
    pub toc: Vec<TocEntry>,
}

impl ReadingBook {
    /// Language codes declared by the container; an empty list means unspecified.
    pub fn languages(&self) -> Vec<String> {
        match &self.source {
            ResourceSource::Epub(epub) => epub.languages(),
            ResourceSource::Mobi(mobi) => mobi.languages(),
            ResourceSource::Fb2(fb2) => fb2.languages(),
        }
    }

    pub fn document_uris(&self) -> &[String] {
        self.resources.documents()
    }

    pub fn read_resource(&self, uri: String) -> impl Future<Output = io::Result<Vec<u8>>> + Send + 'static {
        let prepared = self.resources.read_bytes(&uri);
        let source = self.source.clone();
        async move {
            match prepared {
                Ok(bytes) => Ok(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let uri = local_resource_uri("", &uri)?;
                    match source {
                        ResourceSource::Epub(epub) => epub.read_bytes_async(&uri).await,
                        ResourceSource::Mobi(mobi) => mobi.read_bytes(&uri).await,
                        ResourceSource::Fb2(fb2) => fb2.read_bytes(&uri, crate::resources::MAX_BINARY_RESOURCE_BYTES),
                    }
                }
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(test)]
mod language_tests {
    use super::*;
    use std::io::{Cursor, Write};

    #[test]
    fn profiled_preparation_preserves_documents_css_and_auxiliary_html() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let package = "<package><manifest><item id='a' href='a.xhtml' media-type='application/xhtml+xml'/><item id='b' href='b.xhtml' media-type='application/xhtml+xml'/><item id='css' href='style.css' media-type='text/css'/></manifest><spine><itemref idref='a'/></spine></package>";
        let resources = [("a.xhtml", "<html><body>Main chapter</body></html>"), ("b.xhtml", "<html><body>Auxiliary notes</body></html>"), ("style.css", "body { color: black; }")];
        for (name, text) in [("META-INF/container.xml", "<container><rootfiles><rootfile full-path='book.opf'/></rootfiles></container>"), ("book.opf", package)].into_iter().chain(resources) {
            archive.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            archive.write_all(text.as_bytes()).unwrap();
        }
        let bytes = archive.finish().unwrap().into_inner();
        let plain = block_on(Book::open_reader(BookFormat::Epub, Cursor::new(bytes.clone())).unwrap().prepare_reading()).unwrap();
        let (profiled, profile) = block_on(Book::open_reader(BookFormat::Epub, Cursor::new(bytes)).unwrap().prepare_reading_profiled()).unwrap();
        assert_eq!(plain.document_uris(), profiled.document_uris());
        assert_eq!(plain.document_weights, profiled.document_weights);
        assert_eq!(profile.resources.len(), resources.len());
        assert!(profile.total.active <= profile.total.elapsed);
        for (uri, expected) in resources {
            assert_eq!(plain.resources.read_bytes(uri).unwrap(), profiled.resources.read_bytes(uri).unwrap());
            assert_eq!(profiled.resources.read_bytes(uri).unwrap(), expected.as_bytes());
            assert_eq!(plain.resources.media_type(uri).unwrap(), profiled.resources.media_type(uri).unwrap());
            let resource = profile.resources.iter().find(|resource| resource.uri == uri).unwrap();
            assert_eq!(resource.timing.output_bytes, expected.len());
            assert!(resource.timing.decode.active <= resource.timing.decode.elapsed);
        }
    }

    #[test]
    fn reading_languages_come_from_epub_package_metadata() {
        for languages in [vec![], vec!["en-GB"], vec!["sv"], vec!["fr", "en-US"]] {
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let metadata = languages.iter().map(|language| format!("<dc:language>{language}</dc:language>")).collect::<String>();
            let package = format!(
                "<package xmlns:dc='http://purl.org/dc/elements/1.1/'><metadata>{metadata}</metadata><manifest><item id='chapter' href='chapter.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='chapter'/></spine></package>"
            );
            for (name, text) in
                [("META-INF/container.xml", "<container><rootfiles><rootfile full-path='content.opf'/></rootfiles></container>"), ("content.opf", package.as_str()), ("chapter.xhtml", "<html><body><p>A chapter.</p></body></html>")]
            {
                archive.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
                archive.write_all(text.as_bytes()).unwrap();
            }
            let book = Book::open_reader(BookFormat::Epub, archive.finish().unwrap()).unwrap();
            let reading = block_on(book.prepare_reading()).unwrap();
            assert_eq!(reading.languages(), languages);
        }
    }
}
