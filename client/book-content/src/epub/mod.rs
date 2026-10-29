use crate::profile::{PreparationProfile, ResourcePreparation};
use crate::resource_path::resolve_resource_href;
use crate::resource_path::{is_container_resource, normalize_path as normalize_epub_path};
use crate::resources::{MAX_BINARY_RESOURCE_BYTES, MAX_PREPARED_TEXT_BYTES};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub(crate) mod archive;
use archive::{Archive, ArchiveMetadata, ArchiveReader};
pub(crate) use inspection::EpubSession;
mod inspection;
pub(crate) mod opening;
#[cfg(test)]
mod tolerance_tests;

const MAX_CONTAINER_XML_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MARKUP_RESOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 65_536;

use crate::TocEntry;

use crate::metadata::*;

#[derive(Clone, Debug)]
struct ManifestItem {
    id: String,
    href: String,
    media_type: Option<String>,
    properties: Option<String>,
}

#[derive(Clone, Debug)]
struct ParsedManifest {
    items: Vec<ManifestItem>,
    spine: Vec<String>,
    nav_href: Option<String>,
    ncx_href: Option<String>,
}

/// An immutable async source for interactive reading.
pub(crate) struct EpubProvider {
    archive: Archive,
    pub(crate) package: Arc<EpubPackage>,
}

/// Parsed package and directory metadata shared by owned inspection sessions
/// and asynchronous reading sources.
pub(crate) struct EpubPackage {
    archive: Arc<ArchiveMetadata>,
    opf_path: String,
    opf_xml: String,
    manifest_cache: OnceLock<Arc<ParsedManifest>>,
}

impl EpubPackage {
    async fn open(reader: &mut ArchiveReader) -> io::Result<Self> {
        let mut candidates = Vec::new();
        if let Ok(container) = reader.read("META-INF/container.xml", MAX_CONTAINER_XML_BYTES).await
            && let Some(path) = Self::parse_container_opf_path(&decode_package_xml(&container))
            && let Some(path) = reader.metadata.resolve(&path)
        {
            candidates.push(path);
        }
        for name in reader.metadata.names() {
            if name.to_ascii_lowercase().ends_with(".opf") && !candidates.contains(name) {
                candidates.push(name.clone());
            }
        }
        for opf_path in candidates {
            if let Ok(opf) = reader.read(&opf_path, MAX_PACKAGE_DOCUMENT_BYTES).await {
                return Ok(Self { archive: reader.metadata.clone(), opf_path, opf_xml: decode_package_xml(&opf), manifest_cache: OnceLock::new() });
            }
        }
        // Package metadata is useful, but readable HTML can stand on its own.
        Ok(Self { archive: reader.metadata.clone(), opf_path: String::new(), opf_xml: String::new(), manifest_cache: OnceLock::new() })
    }

    /// Returns the package-declared media type for an archive resource.
    /// EPUB content type comes from the manifest, not the resource filename.
    #[cfg(test)]
    fn resource_media_type(&self, uri: &str) -> Option<&str> {
        let path = normalize_epub_path(uri.split(['?', '#']).next().unwrap_or(uri));
        self.get_or_parse_manifest().items.iter().find(|item| is_container_resource(&item.href) && resolve_resource_href(&self.opf_path, &item.href) == path).and_then(|item| item.media_type.as_deref())
    }

    /// Snapshot container-local manifest metadata without retaining the archive.
    pub(crate) fn resource_media_types(&self, resources: &HashMap<String, Vec<u8>>) -> HashMap<String, String> {
        let mut types: HashMap<_, _> = self.get_or_parse_manifest()
            .items
            .iter()
            .filter(|item| is_container_resource(&item.href))
            .filter_map(|item| {
                let path = self.archive.resolve(&resolve_resource_href(&self.opf_path, &item.href))?;
                item.media_type.as_ref().map(|media_type| (path, media_type.clone()))
            })
            .collect();
        for name in self.archive.names() {
            let inferred = if name.to_ascii_lowercase().ends_with(".css") {
                "text/css"
            } else if name.to_ascii_lowercase().ends_with(".svg") {
                "image/svg+xml"
            } else if Self::is_html_entry(name) {
                "text/html"
            } else {
                continue;
            };
            let prefix = resources.get(name).map(|bytes| decode_package_xml(&bytes[..bytes.len().min(1024)]));
            let detected = prefix.as_deref().and_then(|prefix| {
                let mut prefix = prefix.trim_start_matches('\u{feff}').trim_start();
                loop {
                    let terminator = if prefix.starts_with("<!--") { "-->" } else if prefix.starts_with("<?xml") { "?>" } else { break };
                    let Some((_, rest)) = prefix.split_once(terminator) else { return None };
                    prefix = rest.trim_start();
                }
                if prefix.starts_with('<') {
                    Some(if prefix.starts_with("<svg") { "image/svg+xml" } else { "text/html" })
                } else if inferred == "text/css" && (prefix.contains('{') || prefix.starts_with('@')) {
                    Some("text/css")
                } else {
                    None
                }
            });
            if let Some(detected) = detected {
                let keep_xhtml = detected == "text/html" && types.get(name).is_some_and(|kind| kind.eq_ignore_ascii_case("application/xhtml+xml"));
                if !keep_xhtml {
                    types.insert(name.clone(), detected.to_owned());
                }
                continue;
            }
            let usable = types.get(name).is_some_and(|kind| ["text/css", "text/html", "application/xhtml+xml", "image/svg+xml"].iter().any(|expected| kind.eq_ignore_ascii_case(expected)));
            if !usable {
                types.insert(name.clone(), inferred.to_owned());
            }
        }
        types
    }

    fn exists(&self, uri: &str) -> bool {
        self.archive.contains(uri)
    }

    fn is_html_entry(name: &str) -> bool {
        name.rsplit_once('.').is_some_and(|(_, extension)| ["html", "xhtml", "htm", "svg"].iter().any(|expected| extension.eq_ignore_ascii_case(expected)))
    }

    fn parse_container_opf_path(container_xml: &str) -> Option<String> {
        let mut reader = quick_xml::Reader::from_str(container_xml);
        reader.check_end_names(false);
        reader.trim_text(true);
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Empty(e)) | Ok(quick_xml::events::Event::Start(e)) if e.name().as_ref().ends_with(b"rootfile") => {
                    return read_xml_attr(e.attributes(), b"full-path");
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        None
    }

    fn get_or_parse_manifest(&self) -> &Arc<ParsedManifest> {
        self.manifest_cache.get_or_init(|| Arc::new(Self::parse_opf_manifest(&self.opf_xml)))
    }

    fn parse_opf_manifest(opf_xml: &str) -> ParsedManifest {
        let mut reader = quick_xml::Reader::from_str(opf_xml);
        reader.check_end_names(false);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut items = Vec::new();
        let mut spine = Vec::new();
        let mut nav_href: Option<String> = None;
        let mut toc_id: Option<String> = None;
        let mut in_manifest = false;
        let mut in_spine = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                    if e.name().as_ref().ends_with(b"manifest") {
                        in_manifest = true;
                    } else if e.name().as_ref().ends_with(b"spine") {
                        in_spine = true;
                        toc_id = read_xml_attr(e.attributes(), b"toc");
                    } else if in_manifest && e.name().as_ref().ends_with(b"item") {
                        let mut id = None;
                        let mut href = None;
                        let mut media_type = None;
                        let mut properties = None;

                        for attr in e.attributes().flatten() {
                            let key = attr.key.as_ref();
                            if key.ends_with(b"id") {
                                id = Some(String::from_utf8_lossy(&attr.value).into_owned());
                            } else if key.ends_with(b"href") {
                                href = Some(String::from_utf8_lossy(&attr.value).into_owned());
                            } else if key.ends_with(b"media-type") {
                                media_type = Some(String::from_utf8_lossy(&attr.value).into_owned());
                            } else if key.ends_with(b"properties") {
                                properties = Some(String::from_utf8_lossy(&attr.value).into_owned());
                            }
                        }

                        if let (Some(id), Some(href)) = (id, href) {
                            if let Some(ref props) = properties
                                && props.split_whitespace().any(|v| v == "nav")
                            {
                                nav_href = Some(href.clone());
                            }
                            items.push(ManifestItem { id, href, media_type, properties });
                        }
                    } else if in_spine
                        && e.name().as_ref().ends_with(b"itemref")
                        && let Some(idref) = read_xml_attr(e.attributes(), b"idref")
                    {
                        spine.push(idref);
                    }
                }
                Ok(quick_xml::events::Event::End(e)) => {
                    if e.name().as_ref().ends_with(b"manifest") {
                        in_manifest = false;
                    } else if e.name().as_ref().ends_with(b"spine") {
                        in_spine = false;
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        let ncx_href = toc_id.and_then(|id| items.iter().find(|item| item.id == id).map(|item| item.href.clone()));

        ParsedManifest { items, spine, nav_href, ncx_href }
    }

    fn resolve_spine_entries(&self) -> io::Result<Option<Vec<String>>> {
        let manifest = self.get_or_parse_manifest();

        if manifest.spine.is_empty() {
            return Ok(None);
        }

        let mut out = Vec::new();
        let mut seen = HashSet::new();

        for idref in &manifest.spine {
            let Some(item) = manifest.items.iter().find(|item| &item.id == idref) else {
                continue;
            };
            if !is_container_resource(&item.href) {
                continue;
            }
            let normalized = resolve_resource_href(&self.opf_path, &item.href);
            let Some(path) = self.archive.resolve(&normalized) else { continue };
            if (item.media_type.as_deref().is_some_and(|kind| ["application/xhtml+xml", "text/html"].iter().any(|expected| kind.eq_ignore_ascii_case(expected))) || Self::is_html_entry(&path)) && seen.insert(path.clone()) {
                out.push(path);
            }
        }

        if out.is_empty() { Ok(None) } else { Ok(Some(out)) }
    }

    /// Returns the documents the reader can present, preferring the package
    /// spine and falling back to HTML archive entries for older books.
    pub(crate) fn document_uris(&self) -> io::Result<Vec<String>> {
        if let Some(spine) = self.resolve_spine_entries()? {
            return Ok(spine);
        }

        Ok(self.fallback_documents())
    }

    pub(crate) fn fallback_documents(&self) -> Vec<String> {
        self.archive.names().iter().filter(|name| Self::is_html_entry(name)).cloned().collect()
    }
}

fn decode_package_xml(bytes: &[u8]) -> String {
    let (mut encoding, bom) = Encoding::for_bom(bytes).unwrap_or((UTF_8, 0));
    if bom == 0 {
        if bytes.starts_with(b"<\0?\0") {
            encoding = UTF_16LE;
        } else if bytes.starts_with(b"\0<\0?") {
            encoding = UTF_16BE;
        } else {
            let mut declaration = quick_xml::Reader::from_reader(bytes);
            if let Ok(quick_xml::events::Event::Decl(value)) = declaration.read_event()
                && let Some(Ok(label)) = value.encoding()
                && let Some(declared) = Encoding::for_label(&label)
            {
                encoding = declared;
            } else if std::str::from_utf8(bytes).is_err() {
                encoding = WINDOWS_1252;
            }
        }
    }
    encoding.decode_without_bom_handling(&bytes[bom..]).0.into_owned()
}

fn read_xml_attr(attrs: quick_xml::events::attributes::Attributes, key: &[u8]) -> Option<String> {
    attrs.flatten().find(|a| a.key.as_ref().ends_with(key)).map(|a| a.unescape_value().map_or_else(|_| String::from_utf8_lossy(&a.value).into_owned(), std::borrow::Cow::into_owned))
}

impl EpubProvider {
    pub(crate) fn languages(&self) -> Vec<String> {
        inspection::parse_opf_metadata(&self.package.opf_xml).languages
    }

    /// Load a lazy resource using its own cursor and async decompressor.
    pub(crate) async fn read_bytes_async(&self, uri: &str) -> io::Result<Vec<u8>> {
        self.archive.read(uri, MAX_BINARY_RESOURCE_BYTES).await
    }

    pub(crate) async fn prepare_text_resources_async<const PROFILE: bool>(&self, documents: &[String], profile: &mut PreparationProfile) -> io::Result<HashMap<String, Vec<u8>>> {
        let started = PROFILE.then(Instant::now);
        let mut paths = Vec::new();
        let mut seen = HashSet::new();
        // Reserve the preparation budget for readable chapters first.
        for uri in documents {
            if is_container_resource(uri)
                && let Some(path) = self.package.archive.resolve(uri)
                && seen.insert(path.clone())
            {
                paths.push((path, MAX_BINARY_RESOURCE_BYTES));
            }
        }
        for item in &self.package.get_or_parse_manifest().items {
            if !is_container_resource(&item.href) {
                continue;
            }
            let limit = match item.media_type.as_deref() {
                Some(media_type) if media_type.eq_ignore_ascii_case("text/css") => MAX_MARKUP_RESOURCE_BYTES,
                Some(media_type) if media_type.eq_ignore_ascii_case("application/xhtml+xml") || media_type.eq_ignore_ascii_case("text/html") => MAX_BINARY_RESOURCE_BYTES,
                _ => continue,
            };
            if let Some(path) = self.package.archive.resolve(&resolve_resource_href(&self.package.opf_path, &item.href))
                && seen.insert(path.clone())
            {
                paths.push((path, limit));
            }
        }
        for name in self.package.archive.names() {
            let limit = if name.to_ascii_lowercase().ends_with(".css") {
                MAX_MARKUP_RESOURCE_BYTES
            } else if EpubPackage::is_html_entry(name) {
                MAX_BINARY_RESOURCE_BYTES
            } else {
                continue;
            };
            if seen.insert(name.clone()) {
                paths.push((name.clone(), limit));
            }
        }
        profile.record_sync("resource_plan", started);
        let mut resources = HashMap::new();
        let mut total = 0;
        let mut reader = self.archive.reader();
        for (path, limit) in paths {
            if total >= MAX_PREPARED_TEXT_BYTES {
                break;
            }
            let Ok((bytes, timing)) = reader.read_measured::<PROFILE>(&path, limit.min(MAX_PREPARED_TEXT_BYTES - total)).await else { continue };
            total += bytes.len() as u64;
            if PROFILE {
                profile.resources.push(ResourcePreparation { uri: path.clone(), timing });
            }
            resources.insert(path, bytes);
        }
        Ok(resources)
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, SeekFrom, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingReader {
        cursor: Cursor<Vec<u8>>,
        bytes_read: Arc<AtomicUsize>,
    }

    impl Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.cursor.read(buffer)?;
            self.bytes_read.fetch_add(count, Ordering::Relaxed);
            Ok(count)
        }
    }

    impl Seek for CountingReader {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    // Build a tiny EPUB and replace only central-directory extra fields.
    // No user book or copyrighted contents are embedded in the fixture.
    fn zip64_epub(sentinels: u8) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, text) in [
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#),
            ("OPS/book.opf", r#"<package><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#),
            ("OPS/chapter.xhtml", "<html><body>ZIP64 regression chapter</body></html>"),
        ] {
            writer.start_file(name, stored).unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        let end = bytes.len() - 22;
        assert_eq!(&bytes[end..end + 4], b"PK\x05\x06");
        let start = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut output = bytes[..start].to_vec();
        let mut cursor = start;
        while cursor < end {
            assert_eq!(&bytes[cursor..cursor + 4], b"PK\x01\x02");
            let name_len = u16::from_le_bytes(bytes[cursor + 28..cursor + 30].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(bytes[cursor + 30..cursor + 32].try_into().unwrap()) as usize;
            let comment_len = u16::from_le_bytes(bytes[cursor + 32..cursor + 34].try_into().unwrap()) as usize;
            let mut header = bytes[cursor..cursor + 46].to_vec();
            let mut payload = Vec::new();
            for (mask, offset) in [(1, 24), (2, 20), (4, 42)] {
                if sentinels & mask != 0 {
                    let value = u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
                    payload.extend_from_slice(&(value as u64).to_le_bytes());
                    header[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                }
            }
            if sentinels == 0 {
                // Broken redundant data must not replace valid sizes/offsets.
                payload.resize(32, 0xff);
            }
            header[30..32].copy_from_slice(&((payload.len() + 4) as u16).to_le_bytes());
            output.extend_from_slice(&header);
            output.extend_from_slice(&bytes[cursor + 46..cursor + 46 + name_len]);
            output.extend_from_slice(&1u16.to_le_bytes());
            output.extend_from_slice(&(payload.len() as u16).to_le_bytes());
            output.extend_from_slice(&payload);
            output.extend_from_slice(&bytes[cursor + 46 + name_len + extra_len..cursor + 46 + name_len + extra_len + comment_len]);
            cursor += 46 + name_len + extra_len + comment_len;
        }
        let size = (output.len() - start) as u32;
        let mut footer = bytes[end..].to_vec();
        footer[12..16].copy_from_slice(&size.to_le_bytes());
        output.extend_from_slice(&footer);
        output
    }

    #[test]
    fn zip64_redundant_fields_preserve_valid_offsets_and_sizes() {
        let mut provider = EpubSession::try_from_reader(Cursor::new(zip64_epub(0))).unwrap();
        assert_eq!(provider.package.document_uris().unwrap(), vec!["OPS/chapter.xhtml"]);
        assert!(provider.read_string("OPS/chapter.xhtml").unwrap().contains("ZIP64 regression chapter"));
    }

    #[test]
    fn zip64_sentinel_fields_still_use_extended_values() {
        for mask in 1..=7 {
            let mut provider = EpubSession::try_from_reader(Cursor::new(zip64_epub(mask))).unwrap();
            assert!(provider.read_string("OPS/chapter.xhtml").unwrap().contains("ZIP64 regression chapter"), "sentinel mask {mask}");
        }
    }

    #[test]
    fn zip64_recovery_allows_readable_data_despite_crc_mismatch() {
        let mut bytes = zip64_epub(0);
        let position = bytes.windows(b"ZIP64 regression chapter".len()).position(|window| window == b"ZIP64 regression chapter").unwrap();
        bytes[position] ^= 1;
        let mut provider = EpubSession::try_from_reader(Cursor::new(bytes)).unwrap();
        assert!(provider.read_string("OPS/chapter.xhtml").is_ok());
    }

    #[test]
    fn spine_uses_declared_xhtml_type_for_xml_filenames() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in [
            ("META-INF/container.xml", "<container><rootfiles><rootfile full-path=\"OPS/book.opf\"/></rootfiles></container>"),
            (
                "OPS/book.opf",
                r#"<package><manifest><item id="chapter" href="chapter.xml" media-type="application/xhtml+xml"/><item id="image" href="image.svg" media-type="image/svg+xml"/></manifest><spine><itemref idref="chapter"/><itemref idref="image"/></spine></package>"#,
            ),
            ("OPS/chapter.xml", "<html><body>Chapter</body></html>"),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(bytes.as_bytes()).unwrap();
        }
        let mut provider = EpubSession::try_from_reader(archive.finish().unwrap()).unwrap();
        assert_eq!(provider.package.document_uris().unwrap(), vec!["OPS/chapter.xml"]);
        assert!(provider.read_string("OPS/chapter.xml").unwrap().contains("Chapter"));
    }

    #[test]
    fn manifest_type_is_available_for_xhtml_with_html_filename() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in [
            ("META-INF/container.xml", "<container><rootfiles><rootfile full-path=\"OPS/book.opf\"/></rootfiles></container>"),
            ("OPS/book.opf", r#"<package><manifest><item id="chapter" href="Text/chapter.html" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#),
            ("OPS/Text/chapter.html", "<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><a id=\"page1\"/>Text</body></html>"),
        ] {
            archive.start_file(name, options).unwrap();
            archive.write_all(bytes.as_bytes()).unwrap();
        }
        let provider = EpubSession::try_from_reader(archive.finish().unwrap()).unwrap();
        assert_eq!(provider.package.document_uris().unwrap(), vec!["OPS/Text/chapter.html"]);
        assert_eq!(provider.package.resource_media_type("OPS/Text/chapter.html"), Some("application/xhtml+xml"));
    }

    #[test]
    fn opening_and_reading_a_chapter_does_not_read_the_whole_epub() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        archive.start_file("mimetype", stored).unwrap();
        archive.write_all(b"application/epub+zip").unwrap();
        archive.start_file("META-INF/container.xml", stored).unwrap();
        archive.write_all(br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#).unwrap();
        archive.start_file("OEBPS/content.opf", stored).unwrap();
        archive
            .write_all(br#"<package><metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">Test</dc:title></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/><itemref idref="notes" linear="no"/></spine></package>"#)
            .unwrap();
        archive.start_file("OEBPS/chapter.xhtml", stored).unwrap();
        archive.write_all(b"<html><body>Only this chapter</body></html>").unwrap();
        archive.start_file("OEBPS/notes.xhtml", stored).unwrap();
        archive.write_all(b"<html><body>Auxiliary notes</body></html>").unwrap();
        archive.start_file("OEBPS/unused.bin", stored).unwrap();
        archive.write_all(&vec![7_u8; 4 * 1024 * 1024]).unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let bytes_read = Arc::new(AtomicUsize::new(0));
        let mut provider = EpubSession::try_from_reader(CountingReader { cursor: Cursor::new(bytes.clone()), bytes_read: Arc::clone(&bytes_read) }).unwrap();
        let documents = provider.package.document_uris().unwrap();
        assert_eq!(documents, vec!["OEBPS/chapter.xhtml", "OEBPS/notes.xhtml"]);
        assert_eq!(provider.read_string("OEBPS/chapter.xhtml").unwrap(), "<html><body>Only this chapter</body></html>");
        assert!(bytes_read.load(Ordering::Relaxed) < bytes.len() / 2, "EPUB provider read the unused archive payload");
    }
}
