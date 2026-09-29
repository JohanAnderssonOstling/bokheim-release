use std::collections::HashMap;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use epub_provider::{EpubCreator, EpubMetadata, EpubProvider};
use html::resources::{ResourceMetadata, ResourceProvider};
use regex::{Captures, Regex};

/// A container-neutral book ready for the native HTML renderer.
///
/// Non-EPUB adapters expose stable virtual documents and resources without
/// creating or persisting an intermediate EPUB archive.
pub struct BookSource {
    pub provider: Arc<dyn ResourceProvider>,
    pub document_uris: Vec<String>,
    /// Relative whole-book progress weights aligned with `document_uris`.
    pub document_weights: Vec<u64>,
    pub start_index: usize,
    pub metadata: EpubMetadata,
    pub layout: BookLayout,
}

/// The source container whose adapter should open a book.
///
/// Callers already know this from library metadata. Keeping it explicit avoids
/// treating a filesystem name as authoritative, which also works for remote
/// and browser-backed sources that have no meaningful path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookSourceFormat {
    Epub,
    Mobi,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum BookLayout {
    #[default]
    Reflowable,
}

struct EpubHtmlProvider {
    epub: Arc<EpubProvider>,
}

impl EpubHtmlProvider {
    fn new(epub: Arc<EpubProvider>) -> Self {
        Self { epub }
    }
}

impl ResourceProvider for EpubHtmlProvider {
    fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        self.epub.read_bytes(uri)
    }

    fn metadata(&self, uri: &str) -> io::Result<ResourceMetadata> {
        Ok(ResourceMetadata { media_type: self.epub.resource_media_type(uri).map(str::to_owned), charset: None })
    }

    fn exists(&self, uri: &str) -> bool {
        self.epub.exists(uri)
    }

    fn resolve(&self, base: &str, href: &str) -> String {
        self.epub.resolve(base, href)
    }

    fn list_html_candidates(&self, _root: &str) -> io::Result<Vec<String>> {
        self.epub.document_uris()
    }
}

impl BookSource {
    /// Opens a book from a seekable source. EPUB retains the source and
    /// requests entries lazily; MOBI materializes its bytes.
    pub fn from_reader<R>(format: BookSourceFormat, mut reader: R) -> io::Result<Self>
    where
        R: Read + io::Seek + Send + 'static,
    {
        match format {
            BookSourceFormat::Epub => Self::from_epub_reader(reader),
            BookSourceFormat::Mobi => {
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes)?;
                Self::open(format, bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            }
        }
    }

    /// Opens any supported book while preserving its source format.
    pub fn open(format: BookSourceFormat, bytes: Vec<u8>) -> Result<Self, String> {
        match format {
            BookSourceFormat::Epub => Self::epub(bytes),
            BookSourceFormat::Mobi => Self::mobi("Untitled", bytes),
        }
    }

    /// Opens a supported book from disk.
    pub fn from_path(format: BookSourceFormat, path: PathBuf) -> io::Result<Self> {
        let bytes = std::fs::read(&path)?;
        let fallback_title = fallback_title(&path);
        match format {
            BookSourceFormat::Epub => Self::epub(bytes),
            BookSourceFormat::Mobi => Self::mobi(&fallback_title, bytes),
        }
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn from_epub_bytes(bytes: Vec<u8>) -> io::Result<Self> {
        Self::epub(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn from_epub_reader<R>(reader: R) -> io::Result<Self>
    where
        R: Read + io::Seek + Send + 'static,
    {
        let provider = Arc::new(EpubProvider::try_from_reader(reader)?);
        Self::from_epub_provider(provider).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn try_new(provider: Arc<dyn ResourceProvider>, document_uris: Vec<String>, start_index: usize) -> io::Result<Self> {
        if document_uris.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "book contains no HTML documents"));
        }
        if start_index >= document_uris.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "book start index is outside its document list"));
        }
        let document_weights = vec![1; document_uris.len()];
        Ok(Self { provider, document_uris, document_weights, start_index, metadata: EpubMetadata::default(), layout: BookLayout::Reflowable })
    }

    pub fn provider(&self) -> Arc<dyn ResourceProvider> {
        self.provider.clone()
    }

    pub fn document_uris(&self) -> &[String] {
        &self.document_uris
    }

    pub fn start_index(&self) -> usize {
        self.start_index
    }

    pub fn into_parts(self) -> (Arc<dyn ResourceProvider>, Vec<String>, usize) {
        (self.provider, self.document_uris, self.start_index)
    }

    fn epub(bytes: Vec<u8>) -> Result<Self, String> {
        let provider = Arc::new(EpubProvider::try_from_reader(Cursor::new(bytes)).map_err(|error| error.to_string())?);
        Self::from_epub_provider(provider)
    }

    /// Builds a book from an already-open EPUB archive. Callers that
    /// also need cover extraction can retain another `Arc` and avoid parsing
    /// or copying the complete archive a second time.
    pub fn from_epub_provider(provider: Arc<EpubProvider>) -> Result<Self, String> {
        let metadata = provider.metadata().map_err(|error| error.to_string())?;
        let document_uris = provider.document_uris().map_err(|error| error.to_string())?;
        if document_uris.is_empty() {
            return Err("EPUB contains no readable documents".into());
        }
        let document_weights = provider.document_progress_weights(&document_uris).map_err(|error| error.to_string())?;
        let provider = Arc::new(EpubHtmlProvider::new(provider));
        Ok(Self { provider, document_uris, document_weights, start_index: 0, metadata, layout: BookLayout::Reflowable })
    }

    fn mobi(fallback_title: &str, bytes: Vec<u8>) -> Result<Self, String> {
        let book = mobi::Mobi::new(&bytes).map_err(|error| error.to_string())?;
        Self::from_mobi_book(fallback_title, &book, &bytes)
    }

    /// Builds a book from an already-parsed MOBI. This lets browser
    /// imports share one parse between metadata, resources, and cover work.
    pub fn from_mobi_book(fallback_title: &str, book: &mobi::Mobi, source_bytes: &[u8]) -> Result<Self, String> {
        if !matches!(book.encryption(), mobi::headers::Encryption::No) {
            return Err("DRM-protected MOBI books are not supported".into());
        }
        let mut resources = HashMap::new();
        for (index, record) in book.image_records().iter().enumerate() {
            resources.insert(format!("mobi/resources/{}", index + 1), record.content.to_vec());
        }

        let source = mobi_content_as_string(book, source_bytes)?;
        let image_re = Regex::new(r#"(?i)<img([^>]*?)\srecindex\s*=\s*[\"']?([0-9a-v]+)[\"']?([^>]*)>"#).unwrap();
        let source = image_re.replace_all(&source, |captures: &Captures<'_>| {
            let index = usize::from_str_radix(&captures[2], 32).unwrap_or(0);
            format!("<img{} src=\"../resources/{index}\"{}>", &captures[1], &captures[3])
        });
        let pagebreak = Regex::new(r"(?i)<mbp:pagebreak\s*/?>").unwrap();
        let mut document_uris = Vec::new();
        let mut document_weights = Vec::new();
        for (index, section) in pagebreak.split(&source).filter(|section| !section.trim().is_empty()).enumerate() {
            let uri = format!("mobi/sections/{index}.html");
            let html = format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body>{section}</body></html>");
            let html = html.into_bytes();
            document_weights.push(html.len() as u64);
            resources.insert(uri.clone(), html);
            document_uris.push(uri);
        }
        if document_uris.is_empty() {
            return Err("MOBI contains no readable text".into());
        }
        let mut metadata = EpubMetadata {
            title: if book.title().trim().is_empty() { fallback_title.to_owned() } else { book.title() },
            publisher: book.publisher().unwrap_or_default(),
            description: book.description().unwrap_or_default(),
            date: book.publish_date().unwrap_or_default(),
            ..Default::default()
        };
        if let Some(author) = book.author().filter(|author| !author.trim().is_empty()) {
            metadata.creators.push(EpubCreator::author(author));
        }
        Ok(Self { provider: Arc::new(VirtualProvider::new(resources)), document_uris, document_weights, start_index: 0, metadata, layout: BookLayout::Reflowable })
    }
}

fn mobi_content_as_string(book: &mobi::Mobi, source_bytes: &[u8]) -> Result<String, String> {
    use mobi::headers::{Compression, TextEncoding};
    const MAX_MOBI_TEXT_BYTES: usize = 512 * 1024 * 1024;

    // The PalmDOC header owns the number of text records. The optional MOBI
    // first/last-content fields are commonly 0xffff in older files; treating
    // that sentinel as an array index is the panic in mobi 0.8.0.
    let records = book.raw_records();
    let all_records = records.records();
    let (start, end) = mobi_text_record_bounds(book.metadata.mobi.first_content_record, book.metadata.palmdoc.record_count, all_records.len())?;
    let expected_len = book.metadata.palmdoc.text_length as usize;
    if expected_len > MAX_MOBI_TEXT_BYTES {
        return Err(format!("MOBI declares an excessive uncompressed text size of {expected_len} bytes"));
    }
    let mut decoded = Vec::with_capacity(expected_len);
    let extra_data_flags = mobi_extra_record_data_flags(book, source_bytes)?;

    match book.compression() {
        Compression::No => {
            for record in &all_records[start..end] {
                let content = trim_mobi_trailing_data(record.content, extra_data_flags)?;
                let remaining = expected_len.saturating_sub(decoded.len());
                decoded.extend_from_slice(&content[..content.len().min(remaining)]);
                if decoded.len() == expected_len {
                    break;
                }
            }
        }
        Compression::PalmDoc => {
            for record in &all_records[start..end] {
                let content = trim_mobi_trailing_data(record.content, extra_data_flags)?;
                palmdoc_decompress_into(content, &mut decoded, expected_len)?;
                if decoded.len() == expected_len {
                    break;
                }
            }
        }
        Compression::Huff => {
            // mobi keeps its HUFF/CDIC implementation private. Preserve support
            // for it, but turn parser panics into a normal unsupported-file
            // error at our book boundary.
            let declared = book.readable_records_range();
            let huff_start = book.metadata.mobi.first_huff_record as usize;
            let huff_end = huff_start.checked_add(book.metadata.mobi.huff_record_count as usize);
            if declared.start >= declared.end || declared.end > all_records.len() || !huff_end.is_some_and(|end| end <= all_records.len()) {
                return Err("MOBI HUFF/CDIC record bounds are invalid".to_owned());
            }
            return std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| book.content_as_string())).map_err(|_| "MOBI HUFF/CDIC text record bounds are invalid".to_owned())?.map_err(|error| error.to_string());
        }
    }

    match book.text_encoding() {
        TextEncoding::CP1252 => Ok(encoding_rs::WINDOWS_1252.decode_without_bom_handling(&decoded).0.into_owned()),
        TextEncoding::UTF8 => String::from_utf8(decoded).map_err(|error| error.to_string()),
        TextEncoding::Unknown(_) => Ok(String::from_utf8_lossy(&decoded).into_owned()),
    }
}

fn mobi_extra_record_data_flags(book: &mobi::Mobi, source_bytes: &[u8]) -> Result<u32, String> {
    const PALMDOC_HEADER_LEN: usize = 16;
    const EXTRA_DATA_FLAGS_OFFSET: usize = 0xe0;
    let Some(record_zero) = book.metadata.records.records.first() else {
        return Err("MOBI contains no header record".to_owned());
    };
    if book.metadata.mobi.header_length < (EXTRA_DATA_FLAGS_OFFSET + 4) as u32 {
        return Ok(0);
    }
    let offset = usize::try_from(record_zero.offset).ok().and_then(|offset| offset.checked_add(PALMDOC_HEADER_LEN + EXTRA_DATA_FLAGS_OFFSET)).ok_or_else(|| "MOBI extra-data flags offset overflows".to_owned())?;
    let bytes = source_bytes.get(offset..offset + 4).ok_or_else(|| "MOBI extra-data flags are truncated".to_owned())?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

fn trim_mobi_trailing_data(mut data: &[u8], flags: u32) -> Result<&[u8], String> {
    let mut trailing_entries = flags >> 1;
    while trailing_entries != 0 {
        if trailing_entries & 1 != 0 {
            let size = mobi_trailing_entry_size(data)?;
            data = data.get(..data.len() - size).ok_or_else(|| "MOBI trailing-data entry exceeds its text record".to_owned())?;
        }
        trailing_entries >>= 1;
    }
    if flags & 1 != 0 {
        let last = *data.last().ok_or_else(|| "MOBI multibyte trailer is missing".to_owned())?;
        let size = usize::from(last & 0x03) + 1;
        if size > data.len() {
            return Err("MOBI multibyte trailer exceeds its text record".to_owned());
        }
        data = &data[..data.len() - size];
    }
    Ok(data)
}

fn mobi_trailing_entry_size(data: &[u8]) -> Result<usize, String> {
    let mut result = 0usize;
    let mut bit_position = 0u32;
    for byte in data.iter().rev().take(4) {
        result |= usize::from(byte & 0x7f) << bit_position;
        if byte & 0x80 != 0 {
            return (result > 0 && result <= data.len()).then_some(result).ok_or_else(|| "invalid MOBI trailing-data entry size".to_owned());
        }
        bit_position += 7;
    }
    Err("unterminated MOBI trailing-data entry size".to_owned())
}

fn mobi_text_record_bounds(first_content_record: u16, record_count: u16, total_records: usize) -> Result<(usize, usize), String> {
    let record_count = usize::from(record_count);
    if record_count == 0 {
        return Err("MOBI contains no text records".to_owned());
    }
    let declared_start = usize::from(first_content_record);
    let declared_is_usable = first_content_record != u16::MAX && declared_start > 0 && declared_start.checked_add(record_count).is_some_and(|end| end <= total_records);
    let start = if declared_is_usable { declared_start } else { 1 };
    let end = start.checked_add(record_count).ok_or_else(|| "MOBI text record range overflows".to_owned())?;
    if end > total_records {
        return Err(format!("MOBI declares {record_count} text records but contains only {} data records", total_records.saturating_sub(start)));
    }
    Ok((start, end))
}

fn palmdoc_decompress_into(data: &[u8], output: &mut Vec<u8>, output_limit: usize) -> Result<(), String> {
    let mut position = 0;
    while position < data.len() && output.len() < output_limit {
        let byte = data[position];
        position += 1;
        match byte {
            0 | 0x09..=0x7f => output.push(byte),
            1..=8 => {
                let literal_count = usize::from(byte);
                let end = position.checked_add(literal_count).filter(|end| *end <= data.len()).ok_or_else(|| "truncated PalmDOC literal run".to_owned())?;
                let remaining = output_limit - output.len();
                output.extend_from_slice(&data[position..end][..literal_count.min(remaining)]);
                position = end;
            }
            0x80..=0xbf => {
                let next = *data.get(position).ok_or_else(|| "truncated PalmDOC back-reference".to_owned())?;
                position += 1;
                let pair = u16::from_be_bytes([byte, next]);
                let distance = usize::from((pair >> 3) & 0x07ff);
                let length = usize::from((pair & 0x0007) + 3);
                if distance == 0 || distance > output.len() {
                    return Err("invalid PalmDOC back-reference distance".to_owned());
                }
                for _ in 0..length {
                    if output.len() == output_limit {
                        break;
                    }
                    let source = output.len() - distance;
                    output.push(output[source]);
                }
            }
            _ => {
                if output.len() < output_limit {
                    output.push(b' ');
                }
                if output.len() < output_limit {
                    output.push(byte ^ 0x80);
                }
            }
        }
    }
    Ok(())
}

struct VirtualProvider {
    resources: HashMap<String, Vec<u8>>,
}

impl VirtualProvider {
    fn new(resources: HashMap<String, Vec<u8>>) -> Self {
        Self { resources }
    }
}

impl ResourceProvider for VirtualProvider {
    fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        self.resources.get(uri).cloned().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, uri.to_owned()))
    }
    fn exists(&self, uri: &str) -> bool {
        self.resources.contains_key(uri)
    }
    fn resolve(&self, base: &str, href: &str) -> String {
        resolve_virtual(base, href)
    }
    fn list_html_candidates(&self, _root: &str) -> io::Result<Vec<String>> {
        let mut docs = self.resources.keys().filter(|uri| uri.ends_with(".html") || uri.ends_with(".xhtml")).cloned().collect::<Vec<_>>();
        docs.sort();
        Ok(docs)
    }
}

fn resolve_virtual(base: &str, href: &str) -> String {
    let href = href.split(['#', '?']).next().unwrap_or(href);
    if href.starts_with('/') {
        return href.trim_start_matches('/').to_owned();
    }
    let mut parts = base.rsplit_once('/').map(|(parent, _)| parent.split('/').collect::<Vec<_>>()).unwrap_or_default();
    for part in href.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn fallback_title(path: &Path) -> String {
    path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("Untitled").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write;
    use std::time::{Duration, Instant};

    #[test]
    fn mobi_unset_content_bounds_fall_back_to_palmdoc_record_count() {
        assert_eq!(mobi_text_record_bounds(u16::MAX, 174, 185), Ok((1, 175)));
        assert_eq!(mobi_text_record_bounds(2, 3, 8), Ok((2, 5)));
        assert!(mobi_text_record_bounds(u16::MAX, 8, 4).is_err());
    }

    #[test]
    fn palmdoc_decoder_handles_literals_back_references_and_space_codes() {
        let mut output = Vec::new();
        palmdoc_decompress_into(b"abc\x80\x18\xc1", &mut output, 32).unwrap();
        assert_eq!(output, b"abcabc A");
    }

    #[test]
    fn palmdoc_decoder_rejects_invalid_back_references() {
        let mut output = Vec::new();
        assert!(palmdoc_decompress_into(b"\x80\x18", &mut output, 32).is_err());
    }

    #[test]
    fn mobi_text_record_trailers_are_removed_before_decompression() {
        assert_eq!(trim_mobi_trailing_data(b"text\0\x81", 0b11).unwrap(), b"text");
        assert_eq!(trim_mobi_trailing_data(b"text", 0).unwrap(), b"text");
        assert!(trim_mobi_trailing_data(b"\x81", 0b11).is_err());
    }

    #[test]
    fn virtual_resolution_stays_inside_book_namespace() {
        assert_eq!(resolve_virtual("mobi/sections/4.html", "../resources/4"), "mobi/resources/4");
    }

    #[test]
    fn epub_adapter_resolves_inline_svg_image_resources() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        archive.start_file("mimetype", stored).unwrap();
        archive.write_all(b"application/epub+zip").unwrap();
        archive.start_file("META-INF/container.xml", stored).unwrap();
        archive.write_all(br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#).unwrap();
        archive.start_file("OEBPS/content.opf", stored).unwrap();
        archive.write_all(br#"<package><manifest><item id="cover" href="Text/titlepage.xhtml" media-type="application/xhtml+xml"/><item id="image" href="Images/cover.jpeg" media-type="image/jpeg"/></manifest><spine><itemref idref="cover"/></spine></package>"#).unwrap();
        archive.start_file("OEBPS/Text/titlepage.xhtml", stored).unwrap();
        archive.write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 1 1"><image width="1" height="1" xlink:href="../Images/cover.jpeg"/></svg></body></html>"#).unwrap();
        archive.start_file("OEBPS/Images/cover.jpeg", stored).unwrap();
        archive.write_all(&[0xff, 0xd8, 0xff, 0xd9]).unwrap();

        let epub = Arc::new(EpubProvider::try_from_reader(archive.finish().unwrap()).unwrap());
        let book = BookSource::from_epub_provider(epub).unwrap();
        let provider = book.provider();
        let document_uri = "OEBPS/Text/titlepage.xhtml";
        assert_eq!(provider.metadata(document_uri).unwrap().media_type.as_deref(), Some("application/xhtml+xml"));
        let source = provider.read_string(document_uri).unwrap();
        assert!(source.contains(r#"xlink:href="../Images/cover.jpeg""#), "adapter must return unmodified book bytes");

        let mut factory = html::pipeline::DocumentFactory::new();
        factory.set_resource_context(provider.clone(), document_uri);
        let document = factory.parse_to_dom(&source);
        let (image_idx, image) = document.images().iter().enumerate().find(|(_, image)| matches!(&image.source, html::document::ImageSource::InlineSvg { .. })).expect("inline SVG image resource");
        assert!(matches!(&image.source, html::document::ImageSource::InlineSvg { base_uri, .. } if base_uri == document_uri));

        let mut pipeline = html::resources::ImagePipeline::new(Arc::new(document.images().to_vec()), provider);
        pipeline.ensure_window(&HashSet::from([image_idx as u32]));
        let deadline = Instant::now() + Duration::from_secs(2);
        while pipeline.get_decoded(image_idx as u32).is_none() && Instant::now() < deadline {
            pipeline.poll();
            std::thread::yield_now();
        }
        let html::resources::DecodedImage::Svg { bytes, .. } = pipeline.get_decoded(image_idx as u32).expect("decoded inline SVG") else { panic!("inline SVG must remain a vector resource") };
        let svg = std::str::from_utf8(bytes).expect("hydrated SVG remains UTF-8");
        assert!(svg.contains(r#"xlink:href="data:image/jpeg;base64,/9j/2Q==""#));
    }

    #[test]
    fn epub_adapter_exposes_xhtml_manifest_type_for_html_named_chapter() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in [
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#),
            ("OEBPS/content.opf", r#"<package><manifest><item id="chapter" href="chapter.html" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#),
            ("OEBPS/chapter.html", r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p><a id="page1"/>Text</p></body></html>"#),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(bytes.as_bytes()).unwrap();
        }
        let book = BookSource::from_epub_provider(Arc::new(EpubProvider::try_from_reader(archive.finish().unwrap()).unwrap())).unwrap();
        let uri = &book.document_uris[0];
        assert_eq!(uri, "OEBPS/chapter.html");
        assert_eq!(book.provider().metadata(uri).unwrap().media_type.as_deref(), Some("application/xhtml+xml"));
    }

    #[test]
    #[ignore = "set BOKHEIM_MOBI_FIXTURE to a DRM-free real-world MOBI file"]
    fn probes_real_mobi_fixture() {
        let path = std::env::var_os("BOKHEIM_MOBI_FIXTURE").map(PathBuf::from).expect("BOKHEIM_MOBI_FIXTURE is set");
        let bytes = std::fs::read(&path).unwrap();
        let book = BookSource::mobi(&fallback_title(&path), bytes).unwrap();
        assert!(!book.document_uris.is_empty());
        let first = book.provider.read_string(&book.document_uris[0]).unwrap();
        assert!(first.contains("<body>"));
        assert!(first.len() > 100);
    }
}
