use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

const MAX_CONTAINER_XML_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_DOCUMENT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MARKUP_RESOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BINARY_RESOURCE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 65_536;

/// Book navigation entry, available even in lightweight builds that
/// do not link an HTML renderer (for example the browser metadata WASM).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    pub link: String,
    pub children: Vec<TocEntry>,
}

mod definitions;
pub use definitions::{CreatorRole, IdentifierScheme, parse_creator_role, parse_identifier_scheme};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubIdentifier {
    pub value: String,
    pub scheme: IdentifierScheme,
    /// Raw EPUB 3 `identifier-type` refinement. Keeping this evidence avoids
    /// losing proprietary or future identifier systems that Bokheim does not
    /// yet understand.
    pub identifier_types: Vec<EpubIdentifierType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubIdentifierType {
    pub value: String,
    pub scheme: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EpubCreator {
    pub id: Option<String>,
    pub name: String,            // Display name (from text content)
    pub file_as: Option<String>, // Sort name (from opf:file-as)
    pub role: CreatorRole,
}

impl EpubCreator {
    pub fn author(name: String) -> Self {
        Self { id: None, name, file_as: None, role: CreatorRole::author() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct EpubMetadata {
    pub title: String,
    pub alternate_titles: Vec<EpubTitle>,
    pub identifiers: Vec<EpubIdentifier>,
    pub creators: Vec<EpubCreator>,
    pub contributors: Vec<EpubCreator>,
    pub description: String,
    pub publisher: String,
    pub publishers: Vec<String>,
    pub language: String,
    pub languages: Vec<String>,
    pub date: String,
    pub dates: Vec<EpubDate>,
    pub subjects: Vec<EpubSubject>,
    pub rights: Vec<String>,
    pub sources: Vec<String>,
    pub types: Vec<String>,
    pub formats: Vec<String>,
    pub relations: Vec<String>,
    pub coverage: Vec<String>,
    pub properties: Vec<EpubMetadataProperty>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubMetadataProperty {
    pub id: Option<String>,
    pub property: String,
    pub value: String,
    pub refines: Option<String>,
    pub scheme: Option<String>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubDate {
    pub id: Option<String>,
    pub value: String,
    pub event: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubSubject {
    pub id: Option<String>,
    pub value: String,
    /// Exact package metadata element or property that declared this value.
    pub source: String,
    pub authority: Option<String>,
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpubTitle {
    pub id: Option<String>,
    pub value: String,
    pub title_type: Option<String>,
    pub sort_as: Option<String>,
}

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

trait ReadSeekSend: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeekSend for T {}

pub struct EpubProvider {
    entry_names: std::collections::HashSet<String>,
    archive: Arc<Mutex<zip::ZipArchive<Box<dyn ReadSeekSend>>>>,
    opf_path: String,
    opf_dir: String,
    opf_xml: String,
    manifest_cache: OnceLock<ParsedManifest>,
}

impl EpubProvider {
    pub fn try_new(epub_path: PathBuf) -> io::Result<Self> {
        let file = File::open(&epub_path)?;
        Self::try_from_reader(file)
    }

    pub fn try_from_reader<R>(reader: R) -> io::Result<Self>
    where
        R: Read + Seek + Send + 'static,
    {
        let mut archive = zip::ZipArchive::new(Box::new(reader) as Box<dyn ReadSeekSend>).map_err(io::Error::other)?;
        if archive.len() > MAX_ARCHIVE_ENTRIES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "EPUB archive contains too many entries"));
        }

        // Read and parse container.xml to get OPF path
        let container_xml = {
            let mut entry = archive.by_name("META-INF/container.xml").map_err(|err| io::Error::new(io::ErrorKind::NotFound, err))?;
            if entry.size() > MAX_CONTAINER_XML_BYTES {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "EPUB container.xml exceeds the size limit"));
            }
            let bytes = read_bounded_entry(&mut entry, MAX_CONTAINER_XML_BYTES, "EPUB container.xml")?;
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let opf_path = Self::parse_container_opf_path(&container_xml).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Missing OPF path in container.xml"))?;

        // Read OPF XML
        let opf_xml = {
            let mut entry = archive.by_name(&opf_path).map_err(|err| io::Error::new(io::ErrorKind::NotFound, err))?;
            if entry.size() > MAX_PACKAGE_DOCUMENT_BYTES {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "EPUB package document exceeds the size limit"));
            }
            let bytes = read_bounded_entry(&mut entry, MAX_PACKAGE_DOCUMENT_BYTES, "EPUB package document")?;
            String::from_utf8_lossy(&bytes).into_owned()
        };

        // Pre-compute OPF directory for path resolution
        let opf_dir = opf_path.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string();

        let entry_names = archive.file_names().map(str::to_owned).collect();
        Ok(Self { entry_names, archive: Arc::new(Mutex::new(archive)), opf_path, opf_dir, opf_xml, manifest_cache: OnceLock::new() })
    }

    pub fn new(epub_path: PathBuf) -> Self {
        Self::try_new(epub_path).expect("Failed to open EPUB file")
    }

    pub fn metadata(&self) -> io::Result<EpubMetadata> {
        let mut metadata = parse_opf_metadata(&self.opf_xml);
        add_metadata_isbns(&mut metadata, self.ncx_property_values());
        Ok(metadata)
    }

    /// Complete property values from package metadata and the bounded NCX head.
    /// No spine documents or images are opened by this metadata-only stage.
    pub fn bibliographic_property_values(&self) -> Vec<String> {
        let mut values = metadata_block_values(&self.opf_xml, "metadata");
        values.extend(self.ncx_property_values());
        values
    }

    fn ncx_property_values(&self) -> Vec<String> {
        let mut values = Vec::new();
        if let Some(href) = &self.get_or_parse_manifest().ncx_href {
            let path = normalize_epub_path(&resolve_href(&self.opf_dir, href));
            if let Ok(extra) = self.with_document_reader(&path, 65_536, |reader| {
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes)?;
                Ok(metadata_block_values(&String::from_utf8_lossy(&bytes), "head"))
            }) {
                values.extend(extra);
            }
        }
        values
    }

    /// Archive-relative package-document path declared by container.xml.
    /// Bibliographic inspection may use its file name as weak identifier
    /// evidence, but only after validating the identifier itself.
    pub fn package_document_path(&self) -> &str {
        &self.opf_path
    }

    pub fn is_fixed_layout(&self) -> bool {
        let package = self.opf_xml.to_ascii_lowercase();
        (package.contains("rendition:layout") && package.contains("pre-paginated")) || (package.contains("fixed-layout") && (package.contains("content=\"true\"") || package.contains("content='true'")))
    }

    pub fn cover_image_path(&self) -> io::Result<Option<String>> {
        let Some(href) = self.find_cover_href() else {
            return Ok(None);
        };
        let direct = normalize_epub_path(&href);
        if self.exists(&direct) {
            return Ok(Some(direct));
        }

        let resolved = resolve_href(&self.opf_dir, &href);
        Ok(Some(normalize_epub_path(&resolved)))
    }

    /// Resolves only package-declared and cover-named artwork. Unlike
    /// [`Self::cover_image_path`], this does not guess from arbitrary spine or
    /// manifest images, allowing callers to apply their own geometry checks.
    pub fn declared_cover_image_path(&self) -> io::Result<Option<String>> {
        let Some(href) = self.find_declared_cover_href() else {
            return Ok(None);
        };
        let direct = normalize_epub_path(&href);
        if self.exists(&direct) {
            return Ok(Some(direct));
        }

        let resolved = resolve_href(&self.opf_dir, &href);
        Ok(Some(normalize_epub_path(&resolved)))
    }

    /// Returns image resources in reading order for cover fallback heuristics.
    /// Images referenced by spine documents come first, in DOM order, followed
    /// by any remaining manifest images in package order.
    pub fn image_paths_in_reading_order(&self) -> io::Result<Vec<String>> {
        let manifest = self.get_or_parse_manifest();
        let mut paths = Vec::new();
        let mut seen = HashSet::new();

        for idref in &manifest.spine {
            let Some(item) = manifest.items.iter().find(|item| &item.id == idref) else { continue };
            if !is_manifest_item_html(item) {
                if is_manifest_item_image(item) {
                    let path = normalize_epub_path(&resolve_href(&self.opf_dir, &item.href));
                    if seen.insert(path.clone()) {
                        paths.push(path);
                    }
                }
                continue;
            }
            let document_path = normalize_epub_path(&resolve_href(&self.opf_dir, &item.href));
            let document = match self.read_entry_string(&document_path) {
                Ok(document) => document,
                Err(_) => continue,
            };
            for href in parse_image_hrefs_from_html(&document) {
                let path = resolve_toc_href(&document_path, &href);
                if seen.insert(path.clone()) {
                    paths.push(path);
                }
            }
        }

        for item in &manifest.items {
            if !is_manifest_item_image(item) {
                continue;
            }
            let path = normalize_epub_path(&resolve_href(&self.opf_dir, &item.href));
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    pub fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        let mut archive = self.archive.lock().unwrap();
        let mut entry = archive.by_name(uri).map_err(|err| io::Error::new(io::ErrorKind::NotFound, err))?;
        read_bounded_entry(&mut entry, MAX_BINARY_RESOURCE_BYTES, "EPUB resource")
    }

    pub fn read_string(&self, uri: &str) -> io::Result<String> {
        self.read_entry_string(uri)
    }

    /// Returns the package-declared media type for an archive resource.
    /// EPUB content type comes from the manifest, not the resource filename.
    pub fn resource_media_type(&self, uri: &str) -> Option<&str> {
        let path = normalize_epub_path(uri.split(['?', '#']).next().unwrap_or(uri));
        self.get_or_parse_manifest().items.iter().find(|item| normalize_epub_path(&resolve_href(&self.opf_dir, &item.href)) == path).and_then(|item| item.media_type.as_deref())
    }

    /// Inspect a bounded stream without allocating the complete document.
    /// The callback must not call back into this provider while its archive is locked.
    pub fn with_document_reader<T>(&self, uri: &str, max_bytes: u64, visit: impl FnOnce(&mut dyn Read) -> io::Result<T>) -> io::Result<T> {
        let mut archive = self.archive.lock().map_err(|_| io::Error::other("EPUB archive lock is poisoned"))?;
        let entry = archive.by_name(uri).map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
        visit(&mut entry.take(max_bytes))
    }

    /// Returns cheap book-progress weights for the resolved documents.
    /// ZIP central-directory metadata provides the uncompressed sizes without
    /// reading or parsing every spine document.
    pub fn document_progress_weights(&self, documents: &[String]) -> io::Result<Vec<u64>> {
        let mut archive = self.archive.lock().map_err(|_| io::Error::other("EPUB archive lock is poisoned"))?;
        documents
            .iter()
            .map(|uri| {
                let entry = archive.by_name(uri).map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
                Ok(entry.size().max(1))
            })
            .collect()
    }

    pub fn resolve(&self, base: &str, href: &str) -> String {
        resolve_resource_href(base, href)
    }

    pub fn list_html_candidates(&self, _root: &str) -> io::Result<Vec<String>> {
        self.document_uris()
    }

    pub fn exists(&self, uri: &str) -> bool {
        self.entry_names.contains(uri)
    }

    fn find_declared_cover_href(&self) -> Option<String> {
        let manifest = self.get_or_parse_manifest();

        // Look for cover-image property
        for item in &manifest.items {
            if let Some(ref props) = item.properties
                && props.contains("cover-image")
            {
                return Some(item.href.clone());
            }
        }

        // Try to find cover by metadata tag in OPF.
        if let Some(cover_id) = parse_opf_cover_id(&self.opf_xml) {
            for item in &manifest.items {
                if item.id == cover_id {
                    if is_manifest_item_html(item)
                        && let Some(found) = self.find_cover_from_html(&item.href)
                    {
                        return Some(found);
                    }
                    return Some(item.href.clone());
                }
            }
        }

        // EPUB2 guide reference can point to either image or cover XHTML.
        if let Some(guide_href) = parse_opf_guide_cover_href(&self.opf_xml) {
            if let Some(found) = self.find_cover_from_html(&guide_href) {
                return Some(found);
            }
            if let Some(item) = self.find_manifest_item_by_href(&guide_href)
                && is_manifest_item_image(item)
            {
                return Some(item.href.clone());
            }
            return Some(guide_href);
        }

        // Fallback: try parsing likely cover HTML pages.
        for item in &manifest.items {
            let lower_href = item.href.to_ascii_lowercase();
            let lower_id = item.id.to_ascii_lowercase();
            if (lower_href.contains("cover") || lower_id.contains("cover"))
                && is_manifest_item_html(item)
                && let Some(found) = self.find_cover_from_html(&item.href)
            {
                return Some(found);
            }
        }

        // Final fallback: find image item by cover-like name.
        for item in &manifest.items {
            let lower_href = item.href.to_ascii_lowercase();
            let lower_id = item.id.to_ascii_lowercase();
            if (lower_href.contains("cover") || lower_id.contains("cover")) && is_manifest_item_image(item) {
                return Some(item.href.clone());
            }
        }

        None
    }

    fn find_cover_href(&self) -> Option<String> {
        if let Some(href) = self.find_declared_cover_href() {
            return Some(href);
        }

        let manifest = self.get_or_parse_manifest();
        // Preserve the broad legacy API's conventional EPUB2 fallback. More
        // selective consumers use declared_cover_image_path followed by the
        // dimension-checked reading-order candidates.
        if let Some(first_spine_idref) = manifest.spine.first()
            && let Some(item) = manifest.items.iter().find(|item| &item.id == first_spine_idref)
        {
            if is_manifest_item_html(item) {
                if let Some(found) = self.find_cover_from_html(&item.href) {
                    return Some(found);
                }
            } else if is_manifest_item_image(item) {
                return Some(item.href.clone());
            }
        }

        pick_best_cover_image_item(&manifest.items).map(|item| item.href.clone())
    }

    fn find_cover_from_html(&self, html_href: &str) -> Option<String> {
        let html_href = html_href.split('#').next().unwrap_or(html_href);
        let html_href = html_href.split('?').next().unwrap_or(html_href);
        let resolved = resolve_href(&self.opf_dir, html_href);
        let html_path = normalize_epub_path(&resolved);
        let html = self.read_entry_string(&html_path).ok()?;
        let image_href = parse_first_image_href_from_html(&html)?;
        Some(resolve_toc_href(&html_path, &image_href))
    }

    fn find_manifest_item_by_href(&self, href: &str) -> Option<&ManifestItem> {
        let manifest = self.get_or_parse_manifest();
        let target = normalize_epub_path(&resolve_href(&self.opf_dir, href));
        manifest.items.iter().find(|item| {
            let item_path = normalize_epub_path(&resolve_href(&self.opf_dir, &item.href));
            item_path == target
        })
    }

    pub fn toc(&self) -> io::Result<Option<Vec<TocEntry>>> {
        let manifest = self.get_or_parse_manifest();

        if let Some(ref nav_href) = manifest.nav_href {
            let resolved = resolve_href(&self.opf_dir, nav_href);
            let nav_path = normalize_epub_path(&resolved);
            if let Ok(nav_xml) = self.read_entry_string(&nav_path)
                && let Some(mut entries) = Self::parse_nav_toc(&nav_xml)
            {
                resolve_toc_links(&nav_path, &mut entries);
                return Ok(Some(entries));
            }
        }

        if let Some(ref ncx_href) = manifest.ncx_href {
            let resolved = resolve_href(&self.opf_dir, ncx_href);
            let ncx_path = normalize_epub_path(&resolved);
            if let Ok(ncx_xml) = self.read_entry_string(&ncx_path)
                && let Some(mut entries) = Self::parse_ncx_toc(&ncx_xml)
            {
                resolve_toc_links(&ncx_path, &mut entries);
                return Ok(Some(entries));
            }
        }

        Ok(None)
    }

    fn is_html_entry(name: &str) -> bool {
        let len = name.len();
        if len >= 5 && name[len - 5..].eq_ignore_ascii_case(".html") {
            return true;
        }
        if len >= 6 && name[len - 6..].eq_ignore_ascii_case(".xhtml") {
            return true;
        }
        if len >= 4 && name[len - 4..].eq_ignore_ascii_case(".htm") {
            return true;
        }
        if len >= 4 && name[len - 4..].eq_ignore_ascii_case(".svg") {
            return true;
        }
        false
    }

    fn read_entry_string(&self, name: &str) -> io::Result<String> {
        let mut archive = self.archive.lock().unwrap();
        let mut entry = archive.by_name(name).map_err(|err| io::Error::new(io::ErrorKind::NotFound, err))?;
        let bytes = read_bounded_entry(&mut entry, MAX_MARKUP_RESOURCE_BYTES, "EPUB markup resource")?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn parse_container_opf_path(container_xml: &str) -> Option<String> {
        let mut reader = quick_xml::Reader::from_str(container_xml);
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

    fn get_or_parse_manifest(&self) -> &ParsedManifest {
        self.manifest_cache.get_or_init(|| Self::parse_opf_manifest(&self.opf_xml))
    }

    fn parse_opf_manifest(opf_xml: &str) -> ParsedManifest {
        let mut reader = quick_xml::Reader::from_str(opf_xml);
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

    fn parse_nav_toc(nav_xml: &str) -> Option<Vec<TocEntry>> {
        let mut reader = quick_xml::Reader::from_str(nav_xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut in_toc_nav = false;
        let mut toc_nav_depth = 0usize;
        let mut in_root_ol = false;
        let mut list_stack: Vec<Vec<TocEntry>> = vec![Vec::new()];
        let mut parent_idx_stack: Vec<usize> = Vec::new();
        let mut in_a = false;
        let mut current_href: Option<String> = None;
        let mut current_text = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    if e.name().as_ref().ends_with(b"nav") {
                        if let Some(type_val) = read_xml_attr(e.attributes(), b"type") {
                            if type_val.split_whitespace().any(|v| v == "toc") {
                                in_toc_nav = true;
                                toc_nav_depth = 1;
                            }
                        } else if in_toc_nav {
                            toc_nav_depth += 1;
                        }
                    } else if in_toc_nav && e.name().as_ref().ends_with(b"ol") {
                        if !in_root_ol {
                            in_root_ol = true;
                        } else if let Some(parent_idx) = list_stack.last().and_then(|list| list.len().checked_sub(1)) {
                            parent_idx_stack.push(parent_idx);
                            list_stack.push(Vec::new());
                        }
                    } else if in_toc_nav && e.name().as_ref().ends_with(b"a") {
                        in_a = true;
                        current_href = read_xml_attr(e.attributes(), b"href");
                        current_text.clear();
                    }
                }
                Ok(quick_xml::events::Event::Empty(e)) => {
                    if in_toc_nav
                        && e.name().as_ref().ends_with(b"a")
                        && let Some(href) = read_xml_attr(e.attributes(), b"href")
                        && let Some(list) = list_stack.last_mut()
                    {
                        list.push(TocEntry { title: String::new(), link: href, children: Vec::new() });
                    }
                }
                Ok(quick_xml::events::Event::Text(e)) if in_a => {
                    current_text.push_str(&e.unescape().unwrap_or_default());
                }
                Ok(quick_xml::events::Event::End(e)) => {
                    if e.name().as_ref().ends_with(b"a") && in_a {
                        in_a = false;
                        if let Some(href) = current_href.take() {
                            let title = current_text.trim().to_string();
                            if let Some(list) = list_stack.last_mut() {
                                list.push(TocEntry { title, link: href, children: Vec::new() });
                            }
                        }
                        current_text.clear();
                    } else if in_toc_nav && e.name().as_ref().ends_with(b"ol") {
                        if list_stack.len() > 1 {
                            let children = list_stack.pop().unwrap_or_default();
                            if let Some(parent_idx) = parent_idx_stack.pop()
                                && let Some(parent_list) = list_stack.last_mut()
                                && let Some(parent) = parent_list.get_mut(parent_idx)
                            {
                                parent.children = children;
                            }
                        } else {
                            in_root_ol = false;
                        }
                    } else if e.name().as_ref().ends_with(b"nav") && in_toc_nav {
                        toc_nav_depth = toc_nav_depth.saturating_sub(1);
                        if toc_nav_depth == 0 {
                            in_toc_nav = false;
                        }
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        let root = list_stack.pop().unwrap_or_default();
        if root.is_empty() { None } else { Some(root) }
    }

    fn parse_ncx_toc(ncx_xml: &str) -> Option<Vec<TocEntry>> {
        let mut reader = quick_xml::Reader::from_str(ncx_xml);
        reader.trim_text(true);
        let mut buf = Vec::new();
        let mut stack: Vec<TocEntry> = Vec::new();
        let mut root: Vec<TocEntry> = Vec::new();
        let mut in_text = false;
        let mut text_buf = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(e)) => {
                    if e.name().as_ref().ends_with(b"navPoint") {
                        stack.push(TocEntry::default());
                    } else if e.name().as_ref().ends_with(b"text") {
                        in_text = true;
                        text_buf.clear();
                    } else if e.name().as_ref().ends_with(b"content") {
                        for attr in e.attributes().flatten() {
                            if attr.key.as_ref().ends_with(b"src")
                                && let Some(entry) = stack.last_mut()
                            {
                                entry.link = String::from_utf8_lossy(&attr.value).into_owned();
                            }
                        }
                    }
                }
                Ok(quick_xml::events::Event::Empty(e)) if e.name().as_ref().ends_with(b"content") => {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref().ends_with(b"src")
                            && let Some(entry) = stack.last_mut()
                        {
                            entry.link = String::from_utf8_lossy(&attr.value).into_owned();
                        }
                    }
                }
                Ok(quick_xml::events::Event::Text(e)) if in_text => {
                    text_buf.push_str(&e.unescape().unwrap_or_default());
                }
                Ok(quick_xml::events::Event::End(e)) => {
                    if e.name().as_ref().ends_with(b"text") && in_text {
                        in_text = false;
                        if let Some(entry) = stack.last_mut() {
                            entry.title = text_buf.trim().to_string();
                        }
                    } else if e.name().as_ref().ends_with(b"navPoint")
                        && let Some(entry) = stack.pop()
                    {
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(entry);
                        } else {
                            root.push(entry);
                        }
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        if root.is_empty() { None } else { Some(root) }
    }
    fn resolve_spine_entries(&self) -> io::Result<Option<Vec<String>>> {
        let manifest = self.get_or_parse_manifest();

        if manifest.spine.is_empty() {
            return Ok(None);
        }

        let mut out = Vec::new();

        for idref in &manifest.spine {
            let Some(item) = manifest.items.iter().find(|item| &item.id == idref) else {
                continue;
            };
            let resolved = resolve_href(&self.opf_dir, &item.href);
            let normalized = normalize_epub_path(&resolved);
            if matches!(item.media_type.as_deref(), Some("application/xhtml+xml" | "text/html")) || (item.media_type.is_none() && Self::is_html_entry(&normalized)) {
                out.push(normalized);
            }
        }

        if out.is_empty() { Ok(None) } else { Ok(Some(out)) }
    }

    /// Returns the documents the reader can present, preferring the package
    /// spine and falling back to HTML archive entries for older books.
    pub fn document_uris(&self) -> io::Result<Vec<String>> {
        if let Some(spine) = self.resolve_spine_entries()? {
            return Ok(spine);
        }

        let mut archive = self.archive.lock().unwrap();
        let mut candidates = Vec::new();
        for i in 0..archive.len() {
            let name = archive.by_index(i).map_err(io::Error::other)?.name().to_string();
            if Self::is_html_entry(&name) {
                candidates.push(name);
            }
        }
        Ok(candidates)
    }
}

fn read_bounded_entry(reader: &mut impl Read, declared_limit: u64, label: &str) -> io::Result<Vec<u8>> {
    let capacity = usize::try_from(declared_limit.min(256 * 1024)).unwrap_or(256 * 1024);
    let mut bytes = Vec::with_capacity(capacity);
    reader.take(declared_limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > declared_limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{label} exceeds the size limit")));
    }
    Ok(bytes)
}

fn normalize_epub_path(path: &str) -> String {
    let mut parts = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn read_xml_attr(attrs: quick_xml::events::attributes::Attributes, key: &[u8]) -> Option<String> {
    attrs.flatten().find(|a| a.key.as_ref().ends_with(key)).map(|a| a.unescape_value().map_or_else(|_| String::from_utf8_lossy(&a.value).into_owned(), std::borrow::Cow::into_owned))
}

fn resolve_href(base_dir: &str, href: &str) -> String {
    if base_dir.is_empty() { href.to_string() } else { format!("{}/{}", base_dir.trim_end_matches('/'), href) }
}

fn resolve_toc_links(base_path: &str, entries: &mut [TocEntry]) {
    for entry in entries {
        entry.link = book_root_href(resolve_toc_href(base_path, entry.link.as_str()));
        resolve_toc_links(base_path, &mut entry.children);
    }
}

/// TOC entries are resolved relative to the nav/NCX document when they are
/// parsed. Mark the resulting archive path as root-relative so opening it does
/// not resolve it a second time against whichever spine document is visible.
fn book_root_href(href: String) -> String {
    if href.is_empty() || href.starts_with('/') || href.split('#').next().is_some_and(|path| path.contains(':')) { href } else { format!("/{href}") }
}

pub fn resolve_resource_href(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let href = href.split('?').next().unwrap_or(href);
    if let Some(root_relative) = href.strip_prefix('/') {
        return normalize_epub_path(root_relative);
    }
    let base_dir = base.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let combined = if base_dir.is_empty() { href.to_string() } else { format!("{}/{}", base_dir.trim_end_matches('/'), href) };
    normalize_epub_path(&combined)
}

fn resolve_toc_href(base_path: &str, href: &str) -> String {
    if href.is_empty() {
        return href.to_string();
    }
    if href.starts_with("http:") || href.starts_with("https:") {
        return href.to_string();
    }
    let (path_part, fragment) = href.split_once('#').unwrap_or((href, ""));
    let path_part = path_part.trim();
    let resolved_path = if path_part.is_empty() {
        base_path.to_string()
    } else if path_part.starts_with('/') {
        normalize_epub_path(path_part.trim_start_matches('/'))
    } else if path_part.contains(':') {
        return href.to_string();
    } else {
        let base_dir = base_path.rsplit_once('/').map(|x| x.0).unwrap_or("");
        let combined = if base_dir.is_empty() { path_part.to_string() } else { format!("{}/{}", base_dir.trim_end_matches('/'), path_part) };
        normalize_epub_path(&combined)
    };
    if fragment.is_empty() { resolved_path } else { format!("{}#{}", resolved_path, fragment) }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use std::io::{Cursor, SeekFrom, Write};
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
        let provider = EpubProvider::try_from_reader(Cursor::new(zip64_epub(0))).unwrap();
        assert_eq!(provider.document_uris().unwrap(), vec!["OPS/chapter.xhtml"]);
        assert!(provider.read_string("OPS/chapter.xhtml").unwrap().contains("ZIP64 regression chapter"));
    }

    #[test]
    fn zip64_sentinel_fields_still_use_extended_values() {
        for mask in 1..=7 {
            let provider = EpubProvider::try_from_reader(Cursor::new(zip64_epub(mask))).unwrap();
            assert!(provider.read_string("OPS/chapter.xhtml").unwrap().contains("ZIP64 regression chapter"), "sentinel mask {mask}");
        }
    }

    #[test]
    fn zip64_recovery_does_not_disable_crc_validation() {
        let mut bytes = zip64_epub(0);
        let position = bytes.windows(b"ZIP64 regression chapter".len()).position(|window| window == b"ZIP64 regression chapter").unwrap();
        bytes[position] ^= 1;
        let provider = EpubProvider::try_from_reader(Cursor::new(bytes)).unwrap();
        assert!(provider.read_string("OPS/chapter.xhtml").is_err());
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
        let provider = EpubProvider::try_from_reader(archive.finish().unwrap()).unwrap();
        assert_eq!(provider.document_uris().unwrap(), vec!["OPS/chapter.xml"]);
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
        let provider = EpubProvider::try_from_reader(archive.finish().unwrap()).unwrap();
        assert_eq!(provider.document_uris().unwrap(), vec!["OPS/Text/chapter.html"]);
        assert_eq!(provider.resource_media_type("OPS/Text/chapter.html"), Some("application/xhtml+xml"));
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
        let provider = EpubProvider::try_from_reader(CountingReader { cursor: Cursor::new(bytes.clone()), bytes_read: Arc::clone(&bytes_read) }).unwrap();
        let documents = provider.list_html_candidates("").unwrap();
        assert_eq!(documents, vec!["OEBPS/chapter.xhtml", "OEBPS/notes.xhtml"]);
        assert_eq!(provider.document_progress_weights(&documents).unwrap(), vec![43, 41]);
        assert_eq!(provider.read_string("OEBPS/chapter.xhtml").unwrap(), "<html><body>Only this chapter</body></html>");
        assert!(bytes_read.load(Ordering::Relaxed) < bytes.len() / 2, "EPUB provider read the unused archive payload");
    }

    #[test]
    fn book_root_hrefs_do_not_depend_on_the_current_chapter() {
        assert_eq!(resolve_resource_href("OEBPS/Text/current.xhtml", "/OEBPS/Text/chapter.xhtml#section"), "OEBPS/Text/chapter.xhtml");
        assert_eq!(resolve_resource_href("OPS/another.xhtml", "/OEBPS/Text/chapter.xhtml#section"), "OEBPS/Text/chapter.xhtml");
    }

    #[test]
    fn toc_links_are_marked_as_book_root_hrefs() {
        let mut entries = vec![TocEntry { title: "Chapter".to_owned(), link: "../Text/chapter.xhtml#section".to_owned(), children: Vec::new() }];
        resolve_toc_links("OEBPS/Navigation/nav.xhtml", &mut entries);
        assert_eq!(entries[0].link, "/OEBPS/Text/chapter.xhtml#section");
    }

    #[test]
    fn ordinary_document_links_remain_relative_to_the_current_chapter() {
        assert_eq!(resolve_resource_href("OEBPS/Text/current.xhtml", "next.xhtml#section"), "OEBPS/Text/next.xhtml");
    }
}

fn infer_identifier_scheme(value: &str) -> IdentifierScheme {
    let value = value.trim().to_ascii_lowercase();
    let compact = value.chars().filter(|character| !character.is_ascii_whitespace() && *character != '-').collect::<String>();
    let looks_like_bare_isbn = (compact.len() == 13 && compact.bytes().all(|byte| byte.is_ascii_digit()))
        || (compact.len() == 10 && compact.bytes().take(9).all(|byte| byte.is_ascii_digit()) && (compact.as_bytes()[9].is_ascii_digit() || compact.as_bytes()[9] == b'x'));
    if value.starts_with("urn:isbn:") || value.starts_with("isbn:") || looks_like_bare_isbn {
        IdentifierScheme::ISBN
    } else if value.starts_with("urn:uuid:") || value.starts_with("uuid:") {
        IdentifierScheme::UUID
    } else if value.starts_with("urn:asin:") || value.starts_with("asin:") {
        IdentifierScheme::ASIN
    } else if value.starts_with("doi:") || value.starts_with("https://doi.org/") || value.starts_with("http://dx.doi.org/") {
        IdentifierScheme::DOI
    } else if value.starts_with("urn:issn:") || value.starts_with("issn:") {
        IdentifierScheme::ISSN
    } else if value.starts_with("oclc:") || value.starts_with("urn:oclc:") {
        IdentifierScheme::OCLC
    } else if value.starts_with("lccn:") || value.starts_with("urn:lccn:") {
        IdentifierScheme::LCCN
    } else if value.starts_with("calibre:") || value.starts_with("urn:calibre:") {
        IdentifierScheme::CALIBRE
    } else {
        IdentifierScheme::Unspecified
    }
}

fn identifier_scheme_from_type(identifier_type: &EpubIdentifierType) -> Option<IdentifierScheme> {
    let type_scheme = identifier_type.scheme.as_deref().unwrap_or_default().to_ascii_lowercase();
    if type_scheme.contains("onix") && type_scheme.contains("codelist5") {
        return match identifier_type.value.trim() {
            "02" | "15" | "24" => Some(IdentifierScheme::ISBN),
            "06" => Some(IdentifierScheme::DOI),
            "13" => Some(IdentifierScheme::LCCN),
            "23" => Some(IdentifierScheme::OCLC),
            _ => None,
        };
    }
    let parsed = parse_identifier_scheme(&identifier_type.value);
    (!matches!(parsed, IdentifierScheme::Unspecified | IdentifierScheme::Other(_))).then_some(parsed)
}

fn parse_opf_metadata(opf_xml: &str) -> EpubMetadata {
    let mut reader = quick_xml::Reader::from_str(opf_xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut metadata = EpubMetadata::default();
    let mut current = None;
    let mut current_scheme = IdentifierScheme::Unspecified;
    let mut current_identifier_id: Option<String> = None;
    let mut current_identifier_type_target: Option<String> = None;
    let mut current_identifier_type_scheme: Option<String> = None;
    let mut current_property: Option<(Option<String>, String, Option<String>, Option<String>, Option<String>)> = None;
    let mut current_value_id: Option<String> = None;
    let mut current_subject_source: Option<String> = None;
    let mut current_subject_authority: Option<String> = None;
    let mut current_date_event: Option<String> = None;
    let mut current_role: Option<CreatorRole> = None;
    let mut current_file_as: Option<String> = None;
    let mut parsed_identifiers = Vec::<(Option<String>, EpubIdentifier)>::new();
    let mut identifier_type_refinements = Vec::<(String, EpubIdentifierType)>::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"title") {
                    current = Some(MetaField::Title);
                    current_value_id = read_xml_attr(e.attributes(), b"id");
                } else if name.as_ref().ends_with(b"identifier") {
                    current = Some(MetaField::Identifier);
                    current_scheme = read_xml_attr(e.attributes(), b"scheme").map(|scheme| parse_identifier_scheme(&scheme)).unwrap_or(IdentifierScheme::Unspecified);
                    current_identifier_id = read_xml_attr(e.attributes(), b"id");
                } else if name.as_ref().ends_with(b"meta") && read_xml_attr(e.attributes(), b"property").as_deref().is_some_and(|property| property.rsplit(':').next() == Some("identifier-type")) {
                    current_identifier_type_target = read_xml_attr(e.attributes(), b"refines").and_then(|target| target.strip_prefix('#').map(str::to_owned));
                    current_identifier_type_scheme = read_xml_attr(e.attributes(), b"scheme");
                    current = current_identifier_type_target.as_ref().map(|_| MetaField::IdentifierType);
                } else if name.as_ref().ends_with(b"meta") {
                    if let Some(property) = read_xml_attr(e.attributes(), b"property") {
                        current_property = Some((read_xml_attr(e.attributes(), b"id"), property, read_xml_attr(e.attributes(), b"refines"), read_xml_attr(e.attributes(), b"scheme"), read_xml_attr(e.attributes(), b"lang")));
                        current = Some(MetaField::Property);
                    }
                } else if name.as_ref().ends_with(b"creator") {
                    current = Some(MetaField::Creator);
                    // Extract role attribute (opf:role), default to Author if not present
                    current_role = Some(read_xml_attr(e.attributes(), b"role").map(|r| parse_creator_role(&r)).unwrap_or_else(CreatorRole::author));
                    current_value_id = read_xml_attr(e.attributes(), b"id");
                    // Extract file-as attribute (opf:file-as)
                    current_file_as = read_xml_attr(e.attributes(), b"file-as");
                } else if name.as_ref().ends_with(b"contributor") {
                    current_role = Some(read_xml_attr(e.attributes(), b"role").map(|r| parse_creator_role(&r)).unwrap_or_else(CreatorRole::contributor));
                    current_value_id = read_xml_attr(e.attributes(), b"id");
                    current = Some(MetaField::Contributor);
                    current_file_as = read_xml_attr(e.attributes(), b"file-as");
                } else if name.as_ref().ends_with(b"description") {
                    current = Some(MetaField::Description);
                } else if name.as_ref().ends_with(b"publisher") {
                    current = Some(MetaField::Publisher);
                } else if name.as_ref().ends_with(b"language") {
                    current = Some(MetaField::Language);
                } else if name.as_ref().ends_with(b"date") {
                    current = Some(MetaField::Date);
                    current_value_id = read_xml_attr(e.attributes(), b"id");
                    current_date_event = read_xml_attr(e.attributes(), b"event");
                } else if name.as_ref().ends_with(b"subject") {
                    current = Some(MetaField::Subject);
                    current_value_id = read_xml_attr(e.attributes(), b"id");
                    current_subject_source = Some(String::from_utf8_lossy(name.as_ref()).into_owned());
                    current_subject_authority = read_xml_attr(e.attributes(), b"scheme");
                } else if name.as_ref().ends_with(b"rights") {
                    current = Some(MetaField::Rights);
                } else if name.as_ref().ends_with(b"source") {
                    current = Some(MetaField::Source);
                } else if name.as_ref().ends_with(b"type") {
                    current = Some(MetaField::Type);
                } else if name.as_ref().ends_with(b"format") {
                    current = Some(MetaField::Format);
                } else if name.as_ref().ends_with(b"relation") {
                    current = Some(MetaField::Relation);
                } else if name.as_ref().ends_with(b"coverage") {
                    current = Some(MetaField::Coverage);
                }
            }
            Ok(quick_xml::events::Event::Empty(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"meta") && read_xml_attr(e.attributes(), b"property").as_deref().is_some_and(|property| property.rsplit(':').next() == Some("identifier-type")) {
                    if let (Some(target), Some(value)) = (read_xml_attr(e.attributes(), b"refines").and_then(|target| target.strip_prefix('#').map(str::to_owned)), read_xml_attr(e.attributes(), b"content")) {
                        let value = value.trim();
                        if !value.is_empty() {
                            identifier_type_refinements.push((target, EpubIdentifierType { value: value.to_owned(), scheme: read_xml_attr(e.attributes(), b"scheme") }));
                        }
                    }
                } else if name.as_ref().ends_with(b"meta") {
                    let property = read_xml_attr(e.attributes(), b"property").or_else(|| read_xml_attr(e.attributes(), b"name"));
                    if let (Some(property), Some(value)) = (property, read_xml_attr(e.attributes(), b"content")) {
                        let value = value.trim();
                        if !value.is_empty() {
                            metadata.properties.push(EpubMetadataProperty {
                                id: read_xml_attr(e.attributes(), b"id"),
                                property,
                                value: value.to_owned(),
                                refines: read_xml_attr(e.attributes(), b"refines"),
                                scheme: read_xml_attr(e.attributes(), b"scheme"),
                                language: read_xml_attr(e.attributes(), b"lang"),
                            });
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"title")
                    || name.as_ref().ends_with(b"identifier")
                    || name.as_ref().ends_with(b"creator")
                    || name.as_ref().ends_with(b"contributor")
                    || name.as_ref().ends_with(b"description")
                    || name.as_ref().ends_with(b"publisher")
                    || name.as_ref().ends_with(b"language")
                    || name.as_ref().ends_with(b"date")
                    || name.as_ref().ends_with(b"subject")
                    || name.as_ref().ends_with(b"rights")
                    || name.as_ref().ends_with(b"source")
                    || name.as_ref().ends_with(b"type")
                    || name.as_ref().ends_with(b"format")
                    || name.as_ref().ends_with(b"relation")
                    || name.as_ref().ends_with(b"coverage")
                    || name.as_ref().ends_with(b"meta")
                {
                    current = None;
                    current_scheme = IdentifierScheme::Unspecified;
                    current_identifier_id = None;
                    current_identifier_type_target = None;
                    current_identifier_type_scheme = None;
                    current_role = None;
                    current_file_as = None;
                    current_property = None;
                    current_value_id = None;
                    current_subject_source = None;
                    current_subject_authority = None;
                    current_date_event = None;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) => {
                if let Some(field) = current {
                    let text = t.unescape().unwrap_or_default().into_owned();
                    if text.is_empty() {
                        continue;
                    }
                    match field {
                        MetaField::Title => {
                            if metadata.title.is_empty() {
                                metadata.title = text;
                            } else {
                                metadata.alternate_titles.push(EpubTitle { id: current_value_id.clone(), value: text, title_type: None, sort_as: None });
                            }
                        }
                        MetaField::Identifier => {
                            let value = text.trim();
                            if !value.is_empty() {
                                let scheme = if matches!(current_scheme, IdentifierScheme::Unspecified) { infer_identifier_scheme(value) } else { current_scheme.clone() };
                                parsed_identifiers.push((current_identifier_id.clone(), EpubIdentifier { value: value.to_owned(), scheme, identifier_types: Vec::new() }));
                            }
                        }
                        MetaField::IdentifierType => {
                            if let Some(target) = current_identifier_type_target.clone() {
                                let value = text.trim();
                                if !value.is_empty() {
                                    identifier_type_refinements.push((target, EpubIdentifierType { value: value.to_owned(), scheme: current_identifier_type_scheme.clone() }));
                                }
                            }
                        }
                        MetaField::Creator => {
                            // unwrap is safe here because we always set current_role for creators
                            metadata.creators.push(EpubCreator { id: current_value_id.clone(), name: text, file_as: current_file_as.clone(), role: current_role.clone().unwrap() });
                        }
                        MetaField::Contributor => {
                            // unwrap is safe here because we only set current=Contributor if role exists
                            metadata.contributors.push(EpubCreator { id: current_value_id.clone(), name: text, file_as: current_file_as.clone(), role: current_role.clone().unwrap() });
                        }
                        MetaField::Description if metadata.description.is_empty() => {
                            metadata.description = text;
                        }
                        MetaField::Publisher => {
                            if metadata.publisher.is_empty() {
                                metadata.publisher = text.clone();
                            }
                            metadata.publishers.push(text);
                        }
                        MetaField::Language => {
                            if metadata.language.is_empty() {
                                metadata.language = text.clone();
                            }
                            metadata.languages.push(text);
                        }
                        MetaField::Date => {
                            if metadata.date.is_empty() {
                                metadata.date = text.clone();
                            }
                            metadata.dates.push(EpubDate { id: current_value_id.clone(), value: text, event: current_date_event.clone() });
                        }
                        MetaField::Subject => {
                            let code = current_subject_authority.as_deref().filter(|authority| authority.eq_ignore_ascii_case("bisac")).filter(|_| looks_like_bisac_code(&text)).map(|_| text.trim().to_ascii_uppercase());
                            metadata.subjects.push(EpubSubject {
                                id: current_value_id.clone(),
                                value: text,
                                source: current_subject_source.clone().unwrap_or_else(|| "dc:subject".to_owned()),
                                authority: current_subject_authority.clone(),
                                code,
                            });
                        }
                        MetaField::Rights => metadata.rights.push(text),
                        MetaField::Source => metadata.sources.push(text),
                        MetaField::Type => metadata.types.push(text),
                        MetaField::Format => metadata.formats.push(text),
                        MetaField::Relation => metadata.relations.push(text),
                        MetaField::Coverage => metadata.coverage.push(text),
                        MetaField::Property => {
                            if let Some((id, property, refines, scheme, language)) = current_property.clone() {
                                metadata.properties.push(EpubMetadataProperty { id, property, value: text, refines, scheme, language });
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    for (source_id, mut identifier) in parsed_identifiers {
        if let Some(source_id) = source_id {
            identifier.identifier_types.extend(identifier_type_refinements.iter().filter(|(target, _)| target == &source_id).map(|(_, identifier_type)| identifier_type.clone()));
        }
        if matches!(identifier.scheme, IdentifierScheme::Unspecified) {
            if let Some(scheme) = identifier.identifier_types.iter().find_map(identifier_scheme_from_type) {
                identifier.scheme = scheme;
            }
        }
        // A checksum-valid whole value outranks the producer's scheme,
        // including EPUB 3 identifier-type refinements.
        if let Some(isbn) = book_model::from_metadata_value(&identifier.value) {
            identifier.value = isbn;
            identifier.scheme = IdentifierScheme::ISBN;
        } else if matches!(identifier.scheme, IdentifierScheme::ISBN) {
            identifier.scheme = IdentifierScheme::Unspecified;
        }
        metadata.identifiers.push(identifier);
    }

    for property in &metadata.properties {
        if property.property.rsplit(':').next() != Some("role") {
            continue;
        }
        let Some(target) = property.refines.as_deref().and_then(|target| target.strip_prefix('#')) else {
            continue;
        };
        if let Some(credit) = metadata.creators.iter_mut().chain(metadata.contributors.iter_mut()).find(|credit| credit.id.as_deref() == Some(target)) {
            credit.role = parse_creator_role(&property.value);
        }
    }

    let refinement = |id: &Option<String>, property_name: &str| {
        let target = id.as_deref().map(|id| format!("#{id}"));
        metadata.properties.iter().find(|property| property.refines == target && property.property.rsplit(':').next() == Some(property_name)).map(|property| property.value.clone())
    };
    for title in &mut metadata.alternate_titles {
        title.title_type = refinement(&title.id, "title-type");
        title.sort_as = refinement(&title.id, "file-as");
    }
    for subject in &mut metadata.subjects {
        subject.authority = refinement(&subject.id, "authority").or_else(|| subject.authority.take());
        subject.code = refinement(&subject.id, "term").or_else(|| subject.code.take());
        if subject.code.is_none() && subject.authority.as_deref().is_some_and(|authority| authority.eq_ignore_ascii_case("bisac")) && looks_like_bisac_code(&subject.value) {
            subject.code = Some(subject.value.trim().to_ascii_uppercase());
        }
    }

    let property_subjects = metadata.properties.iter().flat_map(subjects_from_property).collect::<Vec<_>>();
    metadata.subjects.extend(property_subjects);

    add_metadata_isbns(&mut metadata, metadata_block_values(opf_xml, "metadata"));
    metadata
}

fn add_metadata_isbns(metadata: &mut EpubMetadata, values: impl IntoIterator<Item = String>) {
    for value in values {
        let Some(isbn) = book_model::from_metadata_value(&value) else { continue };
        if metadata.identifiers.iter().any(|id| matches!(id.scheme, IdentifierScheme::ISBN) && book_model::from_metadata_value(&id.value).as_ref() == Some(&isbn)) {
            continue;
        }
        metadata.identifiers.push(EpubIdentifier { value: isbn, scheme: IdentifierScheme::ISBN, identifier_types: Vec::new() });
    }
}

fn looks_like_bisac_code(value: &str) -> bool {
    let value = value.trim().as_bytes();
    value.len() == 9 && value[..3].iter().all(u8::is_ascii_alphabetic) && value[3..].iter().all(u8::is_ascii_digit)
}

fn subjects_from_property(property: &EpubMetadataProperty) -> Vec<EpubSubject> {
    let local_name = property.property.rsplit(':').next().unwrap_or(&property.property);
    if property.property.eq_ignore_ascii_case("calibre:user_metadata") {
        return calibre_user_metadata_subjects(&property.property, &property.value);
    }
    if !matches_subject_property(local_name) {
        return Vec::new();
    }
    if local_name.eq_ignore_ascii_case("subject") {
        return subject_values(&property.property, &serde_json::Value::String(property.value.clone()));
    }
    match serde_json::from_str::<serde_json::Value>(&property.value) {
        Ok(serde_json::Value::Object(object)) => object.get("#value#").map(|value| subject_values(&property.property, value)).unwrap_or_default(),
        Ok(value @ serde_json::Value::Array(_)) | Ok(value @ serde_json::Value::String(_)) => subject_values(&property.property, &value),
        _ => subject_values(&property.property, &serde_json::Value::String(property.value.clone())),
    }
}

fn calibre_user_metadata_subjects(source: &str, raw: &str) -> Vec<EpubSubject> {
    let Ok(serde_json::Value::Object(properties)) = serde_json::from_str(raw) else { return Vec::new() };
    let mut subjects = Vec::new();
    for (key, value) in properties {
        let serde_json::Value::Object(property) = value else { continue };
        let identity = format!("{} {} {}", key, property.get("name").and_then(serde_json::Value::as_str).unwrap_or_default(), property.get("label").and_then(serde_json::Value::as_str).unwrap_or_default());
        if !identity.split(|character: char| !character.is_alphanumeric()).any(matches_subject_property) {
            continue;
        }
        if let Some(value) = property.get("#value#") {
            subjects.extend(subject_values(&format!("{source}/{key}"), value));
        }
    }
    subjects
}

fn matches_subject_property(value: &str) -> bool {
    value.eq_ignore_ascii_case("subject") || value.eq_ignore_ascii_case("genre") || value.eq_ignore_ascii_case("tag") || value.eq_ignore_ascii_case("tags")
}

fn subject_values(source: &str, value: &serde_json::Value) -> Vec<EpubSubject> {
    match value {
        serde_json::Value::String(value) => {
            let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if value.is_empty() { Vec::new() } else { vec![EpubSubject { id: None, value, source: source.to_owned(), authority: None, code: None }] }
        }
        serde_json::Value::Array(values) => values.iter().flat_map(|value| subject_values(source, value)).collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy)]
enum MetaField {
    Title,
    Identifier,
    IdentifierType,
    Creator,
    Contributor,
    Description,
    Publisher,
    Language,
    Date,
    Subject,
    Rights,
    Source,
    Type,
    Format,
    Relation,
    Coverage,
    Property,
}

fn parse_opf_cover_id(opf_xml: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(opf_xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut in_metadata = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"metadata") {
                    in_metadata = true;
                } else if in_metadata && name.as_ref().ends_with(b"meta") {
                    let mut meta_name: Option<String> = None;
                    let mut meta_content: Option<String> = None;
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref().ends_with(b"name") {
                            meta_name = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        } else if attr.key.as_ref().ends_with(b"content") {
                            meta_content = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                    if meta_name.as_deref() == Some("cover") {
                        return meta_content;
                    }
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"metadata") {
                    break;
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

fn parse_opf_guide_cover_href(opf_xml: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(opf_xml);
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut in_guide = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                let name = e.name();
                if name.as_ref().ends_with(b"guide") {
                    in_guide = true;
                } else if in_guide && name.as_ref().ends_with(b"reference") {
                    let mut ref_type: Option<String> = None;
                    let mut href: Option<String> = None;
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref().ends_with(b"type") {
                            ref_type = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        } else if attr.key.as_ref().ends_with(b"href") {
                            href = Some(String::from_utf8_lossy(&attr.value).into_owned());
                        }
                    }
                    if ref_type.as_deref().map(|s| s.eq_ignore_ascii_case("cover")).unwrap_or(false) {
                        return href;
                    }
                }
            }
            Ok(quick_xml::events::Event::End(e)) if e.name().as_ref().ends_with(b"guide") => {
                break;
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

fn parse_first_image_href_from_html(html: &str) -> Option<String> {
    parse_image_hrefs_from_html(html).into_iter().next()
}

fn parse_image_hrefs_from_html(html: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(html);
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut images = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) | Ok(quick_xml::events::Event::Empty(e)) => {
                let tag = e.name();
                if tag.as_ref().ends_with(b"img") {
                    if let Some(src) = read_xml_attr(e.attributes(), b"src") {
                        images.push(src);
                    }
                } else if tag.as_ref().ends_with(b"image")
                    && let Some(href) = read_xml_attr(e.attributes(), b"href").or_else(|| read_xml_attr(e.attributes(), b"xlink:href"))
                {
                    images.push(href);
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    images
}

fn is_manifest_item_html(item: &ManifestItem) -> bool {
    if let Some(media_type) = item.media_type.as_deref() {
        let media = media_type.to_ascii_lowercase();
        if media == "application/xhtml+xml" || media == "text/html" {
            return true;
        }
    }
    let href = item.href.to_ascii_lowercase();
    href.ends_with(".xhtml") || href.ends_with(".html") || href.ends_with(".htm")
}

fn is_manifest_item_image(item: &ManifestItem) -> bool {
    if let Some(media_type) = item.media_type.as_deref() {
        let media = media_type.to_ascii_lowercase();
        if media.starts_with("image/") {
            return true;
        }
    }
    let href = item.href.to_ascii_lowercase();
    href.ends_with(".jpg") || href.ends_with(".jpeg") || href.ends_with(".png") || href.ends_with(".webp") || href.ends_with(".gif")
}

fn pick_best_cover_image_item(items: &[ManifestItem]) -> Option<&ManifestItem> {
    let mut best: Option<(&ManifestItem, i32)> = None;
    for item in items {
        if !is_manifest_item_image(item) {
            continue;
        }
        let text = format!("{} {}", item.id.to_ascii_lowercase(), item.href.to_ascii_lowercase());
        let mut score = 0i32;
        if text.contains("cover") {
            score += 100;
        }
        if text.contains("front") {
            score += 60;
        }
        if text.contains("title") {
            score += 20;
        }
        if text.contains("pub") {
            score += 12;
        }
        if text.contains("image-z") {
            score += 8;
        }
        if text.contains("back") {
            score -= 40;
        }
        if text.contains("logo") {
            score -= 30;
        }
        if text.contains("icon") {
            score -= 25;
        }

        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((item, score));
        }
    }
    best.map(|(item, _)| item)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::Path;

    #[test]
    fn bounded_entry_reader_rejects_more_than_the_limit() {
        let mut input = std::io::Cursor::new(vec![7_u8; 17]);
        let error = read_bounded_entry(&mut input, 16, "test resource").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("size limit"));
    }

    #[test]
    fn opf_metadata_preserves_every_book_identifier_and_its_type_evidence() {
        let metadata = parse_opf_metadata(
            r##"<package xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
                <metadata>
                    <dc:title>Identifiers</dc:title>
                    <dc:identifier id="isbn13">9781234567897</dc:identifier>
                    <meta refines="#isbn13" property="identifier-type" scheme="onix:codelist5">15</meta>
                    <dc:identifier opf:scheme="UUID">urn:uuid:12345678-1234-1234-1234-123456789abc</dc:identifier>
                    <dc:identifier>publisher-private-42</dc:identifier>
                    <dc:identifier opf:scheme="VendorCase">vendor-value</dc:identifier>
                    <dc:identifier id="doi">10.1000/example</dc:identifier>
                    <meta refines="#doi" property="identifier-type" scheme="onix:codelist5" content="06"/>
                    <dc:identifier opf:scheme="CALIBRE">calibre:legacy-value</dc:identifier>
                    <dc:identifier>   </dc:identifier>
                </metadata>
            </package>"##,
        );

        assert_eq!(metadata.identifiers.len(), 6);
        assert_eq!(
            metadata.identifiers[0],
            EpubIdentifier { value: "9781234567897".to_owned(), scheme: IdentifierScheme::ISBN, identifier_types: vec![EpubIdentifierType { value: "15".to_owned(), scheme: Some("onix:codelist5".to_owned()) }] }
        );
        assert_eq!(metadata.identifiers[1].scheme, IdentifierScheme::UUID);
        assert_eq!(metadata.identifiers[2].scheme, IdentifierScheme::Unspecified);
        assert_eq!(metadata.identifiers[2].value, "publisher-private-42");
        assert_eq!(metadata.identifiers[3].scheme, IdentifierScheme::Other("VendorCase".to_owned()));
        assert_eq!(
            metadata.identifiers[4],
            EpubIdentifier { value: "10.1000/example".to_owned(), scheme: IdentifierScheme::DOI, identifier_types: vec![EpubIdentifierType { value: "06".to_owned(), scheme: Some("onix:codelist5".to_owned()) }] }
        );
        assert_eq!(metadata.identifiers[5].scheme, IdentifierScheme::CALIBRE);
    }

    #[test]
    fn opf_metadata_applies_epub3_role_refinements_and_keeps_untyped_contributors() {
        let metadata = parse_opf_metadata(
            r##"<package xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
                <metadata>
                    <dc:title>Credits</dc:title>
                    <dc:creator id="translator">A Translator</dc:creator>
                    <meta refines="#translator" property="role" scheme="marc:relators">trl</meta>
                    <dc:creator opf:role="aut">An Author</dc:creator>
                    <dc:contributor id="editor">An Editor</dc:contributor>
                    <meta refines="#editor" property="role" scheme="marc:relators" content="edt"/>
                    <dc:contributor>Untyped Credit</dc:contributor>
                </metadata>
            </package>"##,
        );

        assert_eq!(metadata.creators[0].id.as_deref(), Some("translator"));
        assert_eq!(metadata.creators[0].role.as_str(), "trl");
        assert_eq!(metadata.creators[1].role.as_str(), "aut");
        assert_eq!(metadata.contributors[0].role.as_str(), "edt");
        assert_eq!(metadata.contributors[1].role.as_str(), "ctb");
    }

    #[test]
    fn opf_metadata_preserves_useful_dublin_core_values_and_epub_refinements() {
        let metadata = parse_opf_metadata(
            r##"<package xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
                <metadata>
                    <dc:title>Main title</dc:title>
                    <dc:title>Alternate title</dc:title>
                    <dc:publisher>North Wind Press</dc:publisher>
                    <dc:publisher>South Wind Imprint</dc:publisher>
                    <dc:language>sv-SE</dc:language>
                    <dc:language>en</dc:language>
                    <dc:date opf:event="book">2026-08-04</dc:date>
                    <dc:date>2026</dc:date>
                    <dc:subject>Libraries</dc:subject>
                    <dc:rights>Copyright example</dc:rights>
                    <dc:source>urn:isbn:9781234567897</dc:source>
                    <dc:type>Text</dc:type>
                    <dc:format>application/epub+zip</dc:format>
                    <dc:relation>urn:example:related</dc:relation>
                    <dc:coverage>Sweden</dc:coverage>
                    <meta id="series" property="belongs-to-collection">Bokheim Studies</meta>
                    <meta refines="#series" property="collection-type">series</meta>
                    <meta refines="#series" property="group-position">2</meta>
                    <meta property="schema:accessMode" content="textual"/>
                </metadata>
            </package>"##,
        );

        assert_eq!(metadata.title, "Main title");
        assert_eq!(metadata.alternate_titles, vec![EpubTitle { id: None, value: "Alternate title".to_owned(), title_type: None, sort_as: None }],);
        assert_eq!(metadata.publishers, vec!["North Wind Press", "South Wind Imprint"]);
        assert_eq!(metadata.languages, vec!["sv-SE", "en"]);
        assert_eq!(metadata.dates, vec![EpubDate { id: None, value: "2026-08-04".to_owned(), event: Some("book".to_owned()) }, EpubDate { id: None, value: "2026".to_owned(), event: None },],);
        assert_eq!(metadata.subjects, vec![EpubSubject { id: None, value: "Libraries".to_owned(), source: "dc:subject".to_owned(), authority: None, code: None }],);
        assert_eq!(metadata.rights, vec!["Copyright example"]);
        assert_eq!(metadata.sources, vec!["urn:isbn:9781234567897"]);
        assert_eq!(metadata.types, vec!["Text"]);
        assert_eq!(metadata.formats, vec!["application/epub+zip"]);
        assert_eq!(metadata.relations, vec!["urn:example:related"]);
        assert_eq!(metadata.coverage, vec!["Sweden"]);
        assert!(metadata.properties.iter().any(|property| { property.id.as_deref() == Some("series") && property.property == "belongs-to-collection" && property.value == "Bokheim Studies" }));
        assert!(metadata.properties.iter().any(|property| { property.property == "schema:accessMode" && property.value == "textual" }));
    }

    #[test]
    fn opf_metadata_preserves_embedded_lcc_values_and_refined_terms() {
        let metadata = parse_opf_metadata(
            r##"<package xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf"><metadata>
            <dc:subject>QA76.73.R87</dc:subject>
            <dc:subject opf:scheme="LCC">HD69.C6 R38 1999</dc:subject>
            <dc:subject id="topic">Business consultants</dc:subject>
            <meta refines="#topic" property="authority">LCC</meta>
            <meta refines="#topic" property="term">HD69.C6</meta>
        </metadata></package>"##,
        );
        assert_eq!(metadata.subjects.len(), 3);
        assert_eq!(metadata.subjects[0].value, "QA76.73.R87");
        assert_eq!(metadata.subjects[0].source, "dc:subject");
        assert!(metadata.subjects[0].authority.is_none());
        assert_eq!(metadata.subjects[1].authority.as_deref(), Some("LCC"));
        assert_eq!(metadata.subjects[1].value, "HD69.C6 R38 1999");
        assert_eq!(metadata.subjects[2].authority.as_deref(), Some("LCC"));
        assert_eq!(metadata.subjects[2].code.as_deref(), Some("HD69.C6"));
    }

    #[test]
    fn opf_metadata_promotes_explicit_subject_genre_and_calibre_properties() {
        let metadata = parse_opf_metadata(
            r##"<package xmlns:dc="http://purl.org/dc/elements/1.1/">
                <metadata>
                    <dc:title>Subjects</dc:title>
                    <dc:subject id="bisac">History / United States / 20th Century</dc:subject>
                    <meta refines="#bisac" property="authority">BISAC</meta>
                    <meta refines="#bisac" property="term">HIS036060</meta>
                    <meta property="se:subject">Biography</meta>
                    <meta name="genre" content='{"#value#":["Historical","Political"]}'/>
                    <meta name="calibre:user_metadata" content='{"#fast":{"name":"FAST subject","#value#":["Presidents &amp; Heads of State"]},"#rating":{"name":"Rating","#value#":5}}'/>
                </metadata>
            </package>"##,
        );

        assert_eq!(metadata.subjects[0].source, "dc:subject");
        assert_eq!(metadata.subjects[0].authority.as_deref(), Some("BISAC"));
        assert_eq!(metadata.subjects[0].code.as_deref(), Some("HIS036060"));
        assert!(metadata.subjects.iter().any(|subject| subject.source == "se:subject" && subject.value == "Biography"));
        assert!(metadata.subjects.iter().any(|subject| subject.source == "genre" && subject.value == "Historical"));
        assert!(metadata.subjects.iter().any(|subject| subject.source == "genre" && subject.value == "Political"));
        assert!(metadata.subjects.iter().any(|subject| subject.source == "calibre:user_metadata/#fast" && subject.value == "Presidents & Heads of State"));
        assert!(!metadata.subjects.iter().any(|subject| subject.source.contains("rating")));
    }

    #[test]
    #[ignore] // Run with: cargo test -- --ignored --nocapture
    fn test_recursive_metadata_extraction() {
        // Set the directory to scan - change this to your test directory
        let test_dir = std::env::var("EPUB_TEST_DIR").unwrap_or_else(|_| ".".to_string());

        //println!("\nScanning directory: {}", test_dir);
        //println!("{}", "=".repeat(80));

        extract_metadata_recursive(Path::new(&test_dir));
    }

    fn extract_metadata_recursive(dir: &Path) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("Failed to read directory {:?}: {}", dir, e);
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                extract_metadata_recursive(&path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("epub") {
                // println!("\n📚 File: {}", path.display());
                // println!("{}", "-".repeat(80));

                match EpubProvider::try_new(path.clone()) {
                    Ok(provider) => match provider.metadata() {
                        Ok(metadata) => {
                            for creator in &metadata.contributors {
                                if creator.role.as_str() != "bkp" {
                                    println!("Contributor role: {}", creator.role.as_str());
                                }
                            }
                        }
                        Err(e) => eprintln!("  ❌ Failed to extract metadata: {}", e),
                    },
                    Err(e) => eprintln!("  ❌ Failed to open EPUB: {}", e),
                }
            }
        }
    }
}

/// Keep element text intact across XML text events so a valid substring in a
/// larger value cannot masquerade as a complete ISBN.
fn metadata_block_values(xml: &str, block: &str) -> Vec<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.expand_empty_elements(true);
    let mut inside = false;
    let mut stack: Vec<String> = Vec::new();
    let mut values = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if !inside {
                    if e.local_name().as_ref() == block.as_bytes() {
                        inside = true;
                    }
                    continue;
                }
                stack.push(String::new());
                for attribute in e.attributes().flatten() {
                    if let Ok(value) = attribute.unescape_value() {
                        values.push(value.into_owned());
                    }
                }
            }
            Ok(Event::Text(e)) if inside => {
                if let Ok(text) = e.unescape() {
                    for value in &mut stack {
                        value.push_str(&text);
                    }
                }
            }
            Ok(Event::CData(e)) if inside => {
                for value in &mut stack {
                    value.push_str(&String::from_utf8_lossy(&e));
                }
            }
            Ok(Event::End(e)) if inside => {
                if stack.is_empty() && e.local_name().as_ref() == block.as_bytes() {
                    break;
                }
                if let Some(value) = stack.pop() {
                    values.push(value);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        if values.len() >= 4096 || stack.len() > 64 {
            break;
        }
    }
    values
}

#[cfg(test)]
mod bibliographic_property_tests {
    use super::metadata_block_values;
    #[test]
    fn scans_values_not_digit_fragments_and_stops_at_head_end() {
        let values =
            metadata_block_values(r#"<ncx><head><meta name="dtb:uid" content="978-1-934356-55-5"/><meta name="other">prefix<![CDATA[9781934356555]]>suffix</meta></head><navMap><meta content="9780191507052"/></navMap></ncx>"#, "head");
        assert!(values.iter().any(|v| v == "978-1-934356-55-5"));
        assert!(values.iter().any(|v| v == "prefix9781934356555suffix"));
        assert!(!values.iter().any(|v| v == "9781934356555" || v == "9780191507052"));
    }
}

#[cfg(test)]
mod mislabeled_isbn_tests {
    use super::*;
    #[test]
    fn whole_values_override_schemes_and_include_arbitrary_properties() {
        let metadata = parse_opf_metadata(
            r#"<package><metadata>
          <identifier scheme="ASIN">978-1-934356-55-5</identifier>
          <identifier scheme="ASIN">B005E835K0</identifier>
          <meta name="unknown" content="9780191507052"/>
          <source>9780191085048</source>
          <meta name="uuid" content="7872d3e9-8394-4530-8a39-773505e19c9b"/>
          <meta name="cover" content="9780199588503.jpg"/>
          <identifier scheme="ISBN">0000000000</identifier>
        </metadata></package>"#,
        );
        let isbns: Vec<_> = metadata.identifiers.iter().filter(|id| matches!(id.scheme, IdentifierScheme::ISBN)).map(|id| id.value.as_str()).collect();
        assert_eq!(isbns, ["9781934356555", "9780191507052", "9780191085048"]);
        assert!(metadata.identifiers.iter().any(|id| id.value == "B005E835K0" && matches!(id.scheme, IdentifierScheme::ASIN)));
    }
}
