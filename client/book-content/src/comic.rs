//! CBZ directory, ComicInfo metadata and bounded, on-demand page reads.

use crate::epub::archive::{Archive, ArchiveReader};
use crate::source::{ReaderFactory, Source};
use crate::xml::{self, invalid};
use crate::{BookReader, CreatorRole, DeclaredMetadata, MetadataCredit, MetadataDate, MetadataProperty};
use futures_lite::future::block_on;
use futures_util::future::BoxFuture;
use std::{
    cmp::Ordering,
    future::Future,
    io::{self, Read, Seek},
    sync::Arc,
};

pub const MAX_PAGE_BYTES: u64 = 24 * 1024 * 1024;
const MAX_COMIC_INFO_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PAGES: usize = 20_000;
type Prefetch = Arc<dyn Fn(Vec<(u64, usize)>) -> BoxFuture<'static, io::Result<()>> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ComicPage {
    pub name: String,
    pub bookmark: Option<String>,
    pub double_page: bool,
}

pub struct ComicBook {
    archive: ArchiveReader,
    pub pages: Vec<ComicPage>,
    pub metadata: DeclaredMetadata,
    pub right_to_left: bool,
    pub cover_page: usize,
    prefetch: Option<Prefetch>,
}

#[derive(Clone)]
pub struct ComicReader {
    archive: Arc<Archive>,
    pub pages: Arc<[ComicPage]>,
    pub right_to_left: bool,
    prefetch: Option<Prefetch>,
}

impl ComicBook {
    pub async fn open<F, R>(factory: F) -> io::Result<Self>
    where
        F: Fn() -> R + Send + Sync + 'static,
        R: BookReader + 'static,
    {
        Self::open_source(Source::reopenable(ReaderFactory::new(factory)), 0, None).await
    }

    /// Fetch optional metadata and the requested page together when their ZIP
    /// spans are known. Deleted-page metadata is applied before actual reading.
    pub async fn open_with_prefetch<F, R, P, PF>(factory: F, initial_page: usize, prefetch: P) -> io::Result<Self>
    where
        F: Fn() -> R + Send + Sync + 'static,
        R: BookReader + 'static,
        P: Fn(Vec<(u64, usize)>) -> PF + Send + Sync + 'static,
        PF: Future<Output = io::Result<()>> + Send + 'static,
    {
        let prefetch: Prefetch = Arc::new(move |ranges| Box::pin(prefetch(ranges)));
        Self::open_source(Source::reopenable(ReaderFactory::new(factory)), initial_page, Some(prefetch)).await
    }

    pub fn open_reader(reader: impl Read + Seek + Send + 'static) -> io::Result<Self> {
        block_on(Self::open_source(Source::from_reader(reader), 0, None))
    }

    async fn open_source(source: Source, initial_page: usize, prefetch: Option<Prefetch>) -> io::Result<Self> {
        let mut archive = ArchiveReader::open_with_limit(source, MAX_PAGE_BYTES).await?;
        let mut pages: Vec<_> = archive.metadata.names().iter().filter(|name| is_page(name)).map(|name| ComicPage { name: name.clone(), bookmark: None, double_page: false }).collect();
        if pages.is_empty() || pages.len() > MAX_PAGES {
            return Err(invalid("CBZ contains no page images or too many pages"));
        }
        pages.sort_by(|left, right| natural_order(&left.name, &right.name));
        let info_name = archive.metadata.names().iter().find(|name| name.eq_ignore_ascii_case("ComicInfo.xml")).cloned();
        if let Some(prefetch) = &prefetch {
            let mut names = vec![pages[initial_page.min(pages.len() - 1)].name.clone()];
            if let Some(name) = &info_name {
                if archive.metadata.size(name).is_ok_and(|size| size <= MAX_COMIC_INFO_BYTES) {
                    names.push(name.clone());
                }
            }
            // Warming must not replace the reader's ordinary error/retry path.
            if let Ok(ranges) = archive.metadata.record_ranges(&names) {
                let _ = prefetch(ranges).await;
            }
        }
        let mut metadata = DeclaredMetadata::default();
        let mut right_to_left = false;
        let mut cover_page = 0;
        if let Some(name) = info_name.filter(|name| archive.metadata.size(name).is_ok_and(|size| size <= MAX_COMIC_INFO_BYTES)) {
            let bytes = archive.read(&name, MAX_COMIC_INFO_BYTES).await?;
            // Optional metadata must not veto otherwise readable page images.
            if let Some(root) = xml::parse(&bytes, MAX_COMIC_INFO_BYTES as usize).ok().filter(|root| root.name == "ComicInfo") {
                metadata.title = root.field("Title");
                metadata.description = root.field("Summary");
                for (field, role) in [("Writer", "aut"), ("Penciller", "art"), ("Inker", "art"), ("Colorist", "clr"), ("Letterer", "ctb"), ("CoverArtist", "cov"), ("Editor", "edt"), ("Translator", "trl")] {
                    for name in root.field(field).split(',').map(str::trim).filter(|value| !value.is_empty()) {
                        let credit = MetadataCredit { id: None, name: name.into(), file_as: None, role: CreatorRole::new(role) };
                        if role == "aut" {
                            metadata.creators.push(credit);
                        } else {
                            metadata.contributors.push(credit);
                        }
                    }
                }
                for (field, values) in [("Publisher", &mut metadata.publishers), ("LanguageISO", &mut metadata.languages)] {
                    let value = root.field(field);
                    if !value.is_empty() {
                        values.push(value);
                    }
                }
                let year = root.field("Year");
                if !year.is_empty() {
                    metadata.dates.push(MetadataDate { source: "comicinfo:Year".into(), id: None, value: year, event: Some("publication".into()) });
                }
                for field in ["Series", "Number", "Volume", "Count", "Genre", "Tags", "GTIN", "Web"] {
                    let value = root.field(field);
                    if !value.is_empty() {
                        metadata.properties.push(MetadataProperty { id: None, property: format!("comicinfo:{field}"), value, refines: None, scheme: None, language: None });
                    }
                }
                right_to_left = root.field("Manga").eq_ignore_ascii_case("YesAndRightToLeft");
                let mut deleted = std::collections::HashSet::new();
                if let Some(info) = root.child("Pages") {
                    for page in info.children().filter(|node| node.name == "Page") {
                        let Some(index) = page.attr("Image").and_then(|value| value.parse::<usize>().ok()).filter(|index| *index < pages.len()) else { continue };
                        pages[index].bookmark = page.attr("Bookmark").map(str::trim).filter(|value| !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)).map(str::to_owned);
                        pages[index].double_page = page.attr("DoublePage").is_some_and(|value| value.eq_ignore_ascii_case("true"));
                        if page.attr("Type").is_some_and(|value| value.eq_ignore_ascii_case("FrontCover")) {
                            cover_page = index;
                        }
                        if page.attr("Type").is_some_and(|value| value.eq_ignore_ascii_case("Delete") || value.eq_ignore_ascii_case("Deleted")) {
                            deleted.insert(index);
                        }
                    }
                }
                let cover_name = pages[cover_page].name.clone();
                pages = pages.into_iter().enumerate().filter_map(|(index, page)| (!deleted.contains(&index)).then_some(page)).collect();
                if pages.is_empty() {
                    return Err(invalid("CBZ contains no visible page images"));
                }
                cover_page = pages.iter().position(|page| page.name == cover_name).unwrap_or(0);
            }
        }
        Ok(Self { archive, pages, metadata, right_to_left, cover_page, prefetch })
    }

    pub fn read_page(&mut self, index: usize) -> io::Result<Vec<u8>> {
        let page = self.pages.get(index).ok_or_else(|| invalid("comic page index out of bounds"))?;
        block_on(self.archive.read(&page.name, MAX_PAGE_BYTES))
    }

    pub async fn into_reader(self) -> io::Result<ComicReader> {
        Ok(ComicReader { archive: Arc::new(self.archive.into_archive().await?), pages: self.pages.into(), right_to_left: self.right_to_left, prefetch: self.prefetch })
    }
}

impl ComicReader {
    pub async fn read_page(&self, index: usize) -> io::Result<Vec<u8>> {
        let page = self.pages.get(index).ok_or_else(|| invalid("comic page index out of bounds"))?;
        if let Some(prefetch) = &self.prefetch {
            if let Ok(ranges) = self.archive.reader().metadata.record_ranges(std::slice::from_ref(&page.name)) {
                let _ = prefetch(ranges).await;
            }
        }
        self.archive.read(&page.name, MAX_PAGE_BYTES).await
    }
}

fn is_page(name: &str) -> bool {
    if name.contains('\\') || name.split('/').any(|part| part.starts_with('.') || part == "__MACOSX") {
        return false;
    }
    name.rsplit_once('.').is_some_and(|(_, extension)| matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff"))
}

/// Numeric runs compare without integer parsing, including very long numbers.
fn natural_order(left: &str, right: &str) -> Ordering {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (start_i, start_j) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (mut x, mut y) = (start_i, start_j);
            while x + 1 < i && a[x] == b'0' {
                x += 1;
            }
            while y + 1 < j && b[y] == b'0' {
                y += 1;
            }
            let order = (i - x).cmp(&(j - y)).then_with(|| a[x..i].cmp(&b[y..j]));
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            if order != Ordering::Equal {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| left.cmp(right))
}
