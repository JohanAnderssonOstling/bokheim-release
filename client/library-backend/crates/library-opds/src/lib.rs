//! OPDS feeds, read on the device: OPDS 1.2 (Atom) and OPDS 2 (JSON) parsed
//! into one model. Everything above the parsers sees only `Feed`. See
//! docs/opds-sources-plan.md.
use url::Url;

mod browse;
#[cfg(not(target_arch = "wasm32"))]
pub mod browser;
mod fetch;
mod opds1;
mod opds2;
mod pages;
mod publications;

pub use browse::{browse, feed_title};
pub use opds1::opensearch_template;
pub use publications::{buy_page, card_cover, cover, description, download, evict_cover_cache};

/// One page of a feed: what browsing it shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// The page's own title; a feed's root names the feed.
    pub title: String,
    /// Links to other pages: subfolders, shelves, OPDS 2 groups, and (in
    /// feeds such as Gutenberg's search results) a book's own page.
    pub navigation: Vec<NavigationLink>,
    pub publications: Vec<Publication>,
    pub search: Option<Search>,
    /// The page that continues this one.
    pub next: Option<Url>,
    /// The feed's own sort and filter choices, by group.
    pub facets: Vec<FacetGroup>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FacetGroup {
    pub title: String,
    pub facets: Vec<Facet>,
}

/// One choice: following `href` shows the feed sorted or filtered that way.
#[derive(Clone, Debug, PartialEq)]
pub struct Facet {
    pub title: String,
    pub href: Url,
    pub active: bool,
}

/// Adds `facet` to its group, keeping the groups in the order first seen.
fn push_facet(groups: &mut Vec<FacetGroup>, group: &str, facet: Facet) {
    match groups.iter_mut().find(|existing| existing.title == group) {
        Some(existing) => existing.facets.push(facet),
        None => groups.push(FacetGroup { title: group.to_owned(), facets: vec![facet] }),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationLink {
    pub title: String,
    pub href: Url,
    pub count: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Publication {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    /// What Add to library downloads; none for a book offered only to buy,
    /// borrow or behind DRM.
    pub download: Option<Download>,
    /// The store page that sells the book, opened in the store window.
    pub buy: Option<Url>,
    /// The cover image, the full-size one where the feed offers both.
    pub cover: Option<Url>,
    /// The feed's description of the book, as HTML or plain text; empty
    /// when it gives none.
    pub description: String,
}

pub const EPUB: &str = "application/epub+zip";
pub const PDF: &str = "application/pdf";
pub const FB2: &str = "application/x-fictionbook+xml";
pub const FB2_ZIP: &str = "application/x-zip-compressed-fb2";
pub const CBZ: &str = "application/vnd.comicbook+zip";
pub const CBZ_LEGACY: &str = "application/x-cbz";

/// Whether acquisition links' media types, their own and those of their
/// indirect acquisitions, name a DRM license file the book comes through:
/// Adobe's or Readium LCP's. A book offered only so is not listed, as
/// Bokheim cannot read it.
fn protected<'a>(types: impl IntoIterator<Item = &'a str>) -> bool {
    types.into_iter().any(|kind| matches!(bare_media_type(kind).as_str(), "application/vnd.adobe.adept+xml" | "application/vnd.readium.lcp.license.v1.0+json"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub href: Url,
    /// A supported acquisition media type, including FB2 and CBZ.
    pub media_type: &'static str,
    /// The file's size in bytes, where the feed gives it.
    pub length: Option<u64>,
}

/// The open-access link to download among `links` (href, media type,
/// length), preferring reflowable books before fixed-page formats.
fn choose_download<'a>(links: impl IntoIterator<Item = (Url, &'a str, Option<u64>)>) -> Option<Download> {
    let formats = [EPUB, FB2, FB2_ZIP, PDF, CBZ, CBZ_LEGACY];
    links
        .into_iter()
        .filter_map(|(href, kind, length)| {
            let kind = bare_media_type(kind);
            let preference = formats.iter().position(|media_type| *media_type == kind)?;
            Some((preference, Download { href, media_type: formats[preference], length }))
        })
        .min_by_key(|(preference, _)| *preference)
        .map(|(_, download)| download)
}

/// How a feed is searched: a URL template with `{searchTerms}`, or an
/// OpenSearch description to fetch first (`opensearch_template`).
#[derive(Clone, Debug, PartialEq)]
pub enum Search {
    Template(String),
    Description(Url),
}

impl Search {
    /// The page for `query` under a template.
    /// OpenSearch templates use `{searchTerms}`; OPDS 2 templated links use
    /// `{?query}`.
    pub fn url(template: &str, query: &str) -> Result<Url, String> {
        let encoded = url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
        let filled = template.replace("{searchTerms}", &encoded).replace("{?query}", &format!("?query={encoded}"));
        let filled = strip_placeholders(&filled);
        Url::parse(&filled).map_err(|error| format!("invalid search URL {filled}: {error}"))
    }
}

/// Drops the placeholders this client does not fill, such as OpenSearch's
/// optional `{startPage?}`; their parameters are left empty.
fn strip_placeholders(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = rest[start..].find('}').map_or("", |end| &rest[start + end + 1..]);
    }
    out.push_str(rest);
    out
}

/// Parses a page fetched from `base`, choosing the format by its content
/// type, or by its first character when the server does not say.
pub fn parse(bytes: &[u8], content_type: Option<&str>, base: &Url) -> Result<Feed, String> {
    let json = match content_type.map(|value| value.to_ascii_lowercase()) {
        Some(value) if value.contains("json") => true,
        Some(value) if value.contains("xml") => false,
        _ => bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'),
    };
    let mut feed = if json { opds2::parse(bytes, base)? } else { opds1::parse(std::str::from_utf8(bytes).map_err(|_| "the feed is not UTF-8")?, base)? };
    merge_editions(&mut feed.publications);
    Ok(feed)
}

/// Editions of one book listed as entries of their own, such as Project
/// Gutenberg's with and without images, are one card: the edition with the
/// largest download, which is the complete one. Entries are the same book
/// when their titles and authors are.
fn merge_editions(publications: &mut Vec<Publication>) {
    let size = |publication: &Publication| publication.download.as_ref().and_then(|download| download.length).unwrap_or(0);
    let mut kept: Vec<Publication> = Vec::with_capacity(publications.len());
    for publication in publications.drain(..) {
        match kept.iter_mut().find(|other| !publication.title.is_empty() && other.title == publication.title && other.authors == publication.authors) {
            Some(other) if size(&publication) > size(other) => *other = publication,
            Some(_) => {}
            None => kept.push(publication),
        }
    }
    *publications = kept;
}

/// The media type without parameters, lowercase.
fn bare_media_type(value: &str) -> String {
    value.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
}
