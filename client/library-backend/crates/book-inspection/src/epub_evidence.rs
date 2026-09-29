//! Bounded front/back matter inspection. TOC fragments delimit sections inside
//! combined XHTML documents; body chapters are never joined to copyright text.
use crate::evidence::{self, SectionEvidence};
use book_model::BookRecord;
use epub_provider::{EpubProvider, TocEntry};
use quick_xml::{events::Event, Reader};
use std::{
    collections::BTreeMap,
    io::{self, BufReader, Read},
};

const SCAN_BYTES: u64 = 4 * 1024 * 1024;
const BLOCK_BYTES: usize = 16_384;
const MAX_BLOCKS: usize = 32;

fn copyright(s: &str) -> bool {
    let s = s.to_lowercase();
    s.contains("copyright")
        || s.contains('©')
        || s.contains("(c)")
        || s.contains("cataloging-in-book")
        || s.contains("cataloguing-in-book")
        || s.contains("cataloging in book")
        || s.contains("cataloguing in book")
        || s.contains("cataloged the")
}
fn boundary(s: &str) -> bool {
    let s = s.trim().to_lowercase();
    ["contents", "table of contents", "prologue", "epilogue", "introduction", "preface", "foreword", "bibliography", "references", "works cited", "also by", "other books", "further reading", "index", "chapter", "part"]
        .iter()
        .any(|v| s.trim_end_matches(':') == *v || (*v != "references" && s.starts_with(&format!("{v} "))))
}

fn toc_links(entries: &[TocEntry], links: &mut BTreeMap<String, BTreeMap<String, bool>>) {
    for entry in entries.iter().take(2048) {
        let (uri, fragment) = entry.link.split_once('#').unwrap_or((&entry.link, ""));
        // Provider TOC links are archive-root-relative; document URIs omit the slash.
        links.entry(uri.trim_start_matches('/').to_owned()).or_default().insert(fragment.to_owned(), copyright(&entry.title));
        toc_links(&entry.children, links);
    }
}

pub(super) fn inspect(provider: &EpubProvider, book: &BookRecord) -> Vec<SectionEvidence> {
    let Ok(uris) = provider.document_uris() else { return Vec::new() };
    let mut links = BTreeMap::new();
    if let Ok(Some(toc)) = provider.toc() {
        toc_links(&toc, &mut links);
    }
    let mut selected: Vec<_> = uris.iter().enumerate().filter(|(i, _)| *i < evidence::EDGE_SECTIONS || *i + evidence::EDGE_SECTIONS >= uris.len()).map(|(_, uri)| uri.clone()).collect();
    for (uri, anchors) in &links {
        if anchors.values().any(|copyright| *copyright) && !selected.contains(uri) && selected.len() < 32 {
            selected.push(uri.clone());
        }
    }
    let mut sections = Vec::new();
    for uri in selected {
        let anchors = links.get(&uri).cloned().unwrap_or_default();
        if let Ok(found) = provider.with_document_reader(&uri, SCAN_BYTES, |reader| scan(reader, book, &uri, &anchors)) {
            sections.extend(found.into_iter().take(MAX_BLOCKS - sections.len()));
        }
        if sections.len() >= MAX_BLOCKS {
            break;
        }
    }
    sections
}

#[allow(dead_code)]
fn is_cover_image(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase().contains("cover")
}

#[allow(dead_code)]
fn ranked_images(reader: &mut dyn Read, uri: &str, anchors: &BTreeMap<String, bool>) -> io::Result<Vec<(String, bool)>> {
    let mut xml = Reader::from_reader(BufReader::new(reader));
    xml.expand_empty_elements(true);
    xml.check_end_names(false);
    let mut buffer = Vec::new();
    let mut hidden = 0usize;
    let mut priority = anchors.get("") == Some(&true);
    let mut images = Vec::new();
    let mut section_start = 0;
    loop {
        match xml.read_event_into(&mut buffer) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                if matches!(name.as_str(), "head" | "style" | "script") {
                    hidden += 1;
                }
                if hidden > 0 {
                    buffer.clear();
                    continue;
                }
                let attrs = e.attributes().flatten().filter_map(|a| a.unescape_value().ok().map(|v| (String::from_utf8_lossy(a.key.as_ref()).into_owned(), v.into_owned()))).collect::<Vec<_>>();
                let toc = attrs.iter().filter(|(k, _)| k == "id" || k == "name").find_map(|(_, v)| anchors.get(v));
                let typed = attrs.iter().any(|(k, v)| k == "epub:type" && v.split_whitespace().any(|v| v == "copyright-page"));
                if toc.is_some() || typed {
                    priority = typed || toc == Some(&true);
                    section_start = images.len();
                }
                if matches!(name.as_str(), "img" | "image") {
                    for (key, value) in &attrs {
                        if matches!(key.rsplit(':').next(), Some("src" | "href")) && images.len() < 128 {
                            let path = epub_provider::resolve_resource_href(uri, value);
                            if !is_cover_image(&path) {
                                images.push((path, priority));
                            }
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if matches!(e.local_name().as_ref(), b"head" | b"style" | b"script") {
                    hidden = hidden.saturating_sub(1);
                }
            }
            Ok(Event::Text(e)) if hidden == 0 => {
                let raw = String::from_utf8_lossy(e.as_ref()).replace("&copy;", "©").replace("&nbsp;", " ");
                if let Ok(text) = quick_xml::escape::unescape(&raw) {
                    if copyright(&text) {
                        priority = true;
                        for (_, value) in &mut images[section_start..] {
                            *value = true;
                        }
                    } else if text.len() < 160 && boundary(&text) {
                        priority = false;
                        section_start = images.len();
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(images)
}

struct Block<'a> {
    book: &'a BookRecord,
    uri: &'a str,
    location: String,
    text: String,
    active: bool,
    eligible: bool,
    opening_bytes: usize,
    results: Vec<SectionEvidence>,
}
impl Block<'_> {
    fn finish(&mut self) {
        if self.active && self.results.len() < MAX_BLOCKS {
            let evidence = evidence::inspect_section(self.book, &self.location, &self.text);
            if evidence.accepted {
                self.results.push(evidence);
            }
        }
        self.text.clear();
        self.active = false;
    }
    fn paragraph(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if text.len() < 160 && boundary(text) && !copyright(text) {
            self.finish();
            self.eligible = false;
            return;
        }
        if !self.eligible {
            return;
        }
        if !self.active {
            self.opening_bytes = self.opening_bytes.saturating_add(text.len());
            if self.opening_bytes > BLOCK_BYTES {
                self.eligible = false;
                return;
            }
        }
        if copyright(text) {
            self.active = true;
        }
        if self.text.len() + text.len() + 1 > BLOCK_BYTES {
            self.finish();
            self.eligible = false;
            return;
        }
        // Retain a little preceding context (ISBN may precede the copyright).
        if !self.active && self.text.len() > 1024 {
            self.text.clear();
        }
        self.text.push_str(text);
        self.text.push('\n');
    }
}

fn scan(reader: &mut dyn Read, book: &BookRecord, uri: &str, anchors: &BTreeMap<String, bool>) -> io::Result<Vec<SectionEvidence>> {
    let mut xml = Reader::from_reader(BufReader::new(reader));
    xml.expand_empty_elements(true);
    xml.check_end_names(false); // EPUBs in the wild include HTML-style void elements.
    let mut buf = Vec::new();
    let mut paragraph = String::new();
    let mut hidden = 0usize;
    let mut block = Block { book, uri, location: uri.to_owned(), text: String::new(), active: false, eligible: true, opening_bytes: 0, results: Vec::new() };
    if anchors.get("") == Some(&true) {
        block.text.push_str("Copyright\n");
        block.active = true;
    }
    loop {
        match xml.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                if matches!(name.as_str(), "head" | "script" | "style") {
                    hidden += 1;
                }
                if hidden > 0 {
                    buf.clear();
                    continue;
                }
                let attrs: Vec<_> = e.attributes().flatten().filter_map(|a| a.unescape_value().ok().map(|v| (String::from_utf8_lossy(a.key.as_ref()).into_owned(), v.into_owned()))).collect();
                let anchor = attrs.iter().find(|(k, _)| k == "id" || k == "name").map(|(_, v)| v);
                let toc_kind = anchor.and_then(|id| anchors.get(id));
                let copyright_type = attrs.iter().any(|(k, v)| k == "epub:type" && v.split_whitespace().any(|t| t == "copyright-page"));
                if toc_kind.is_some() || copyright_type {
                    block.paragraph(&paragraph);
                    paragraph.clear();
                    block.finish();
                    block.location = anchor.map_or_else(|| block.uri.to_owned(), |id| format!("{}#{id}", block.uri));
                    block.opening_bytes = 0;
                    block.eligible = toc_kind == Some(&true) || copyright_type;
                    if block.eligible {
                        block.text.push_str("Copyright\n");
                        block.active = true;
                    }
                }
                if matches!(name.as_str(), "p" | "div" | "br" | "li" | "h1" | "h2" | "h3" | "h4" | "section") {
                    block.paragraph(&paragraph);
                    paragraph.clear();
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                if matches!(name.as_str(), "head" | "script" | "style") {
                    hidden = hidden.saturating_sub(1);
                }
                if hidden == 0 && matches!(name.as_str(), "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "section" | "body") {
                    block.paragraph(&paragraph);
                    paragraph.clear();
                }
            }
            Ok(Event::Text(e)) if hidden == 0 => {
                // XML numeric entities are decoded by quick-xml. EPUB HTML named
                // copyright/nonbreaking-space entities need an explicit mapping.
                let raw = String::from_utf8_lossy(e.as_ref()).replace("&copy;", "©").replace("&nbsp;", " ");
                if let Ok(text) = quick_xml::escape::unescape(&raw) {
                    if paragraph.len() + text.len() <= BLOCK_BYTES {
                        paragraph.push_str(&text);
                    } else {
                        block.finish();
                        block.eligible = false;
                        paragraph.clear();
                    }
                }
            }
            Ok(Event::Eof) => {
                block.paragraph(&paragraph);
                block.finish();
                break;
            }
            Err(_) => break, // Only previously completed blocks survive malformed input.
            _ => {}
        }
        buf.clear();
    }
    Ok(block.results)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inspect(html: &str, anchors: &[(&str, bool)]) -> Vec<SectionEvidence> {
        let book = BookRecord { title: "wrong metadata".into(), subtitle: None, contributors: vec![], description: String::new(), book: Default::default() };
        scan(&mut html.as_bytes(), &book, "book.xhtml", &anchors.iter().map(|(k, v)| (k.to_string(), *v)).collect()).unwrap()
    }
    #[test]
    fn generic_index_filename_does_not_discard_copyright_isbns() {
        let book = BookRecord { title: "Book".into(), subtitle: None, contributors: vec![], description: String::new(), book: Default::default() };
        for (line, expected) in [("ISBN 978–0–19–921142–5", "9780199211425"), ("ISBN: 978-1-591-39782-3", "9781591397823")] {
            let html = format!("<html><body><p>Copyright 2007</p><p>{line}</p><h2>Contents</h2><p>ISBN 9780821338278</p></body></html>");
            let found = scan(&mut html.as_bytes(), &book, "index_split_000.html", &BTreeMap::new()).unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].isbns.len(), 1);
            assert_eq!(found[0].isbns[0].isbn, expected);
        }
    }
    #[test]
    fn large_combined_document_stops_before_body_isbns() {
        let html = format!("<html><body><p>Copyright © 2004</p><p>I. Title. E3002.6.H2C48 2004</p><h3>Contents</h3><p>ISBN 9780393243277</p>{}</body></html>", "<p>Chapter text</p>".repeat(20000));
        let found = inspect(&html, &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lcc, ["E3002.6.H2C48 2004"]);
        assert!(found[0].isbns.is_empty());
    }
    #[test]
    fn toc_fragment_reaches_copyright_after_large_section_and_stops_at_prologue() {
        let html = format!("<html><body><h1>Introduction</h1>{}<div id='rights'><p>ISBN 9780821338278</p></div><a id='start'/><p>ISBN 9780393243277</p></body></html>", "<p>Body</p>".repeat(20000));
        let found = inspect(&html, &[("rights", true), ("start", false)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].location, "book.xhtml#rights");
        assert_eq!(found[0].isbns.len(), 1);
        assert_eq!(found[0].isbns[0].isbn, "9780821338278");
    }
    #[test]
    fn unheaded_body_copyright_is_not_a_front_matter_block() {
        let html = format!("<html><body>{}<p>Copyright ISBN 9780821338278</p></body></html>", "<p>Body text without headings</p>".repeat(2000));
        assert!(inspect(&html, &[]).is_empty());
    }

    #[test]
    fn franco_copyright_sentence_does_not_stop_before_unlabelled_isbns() {
        let found = inspect("<html><body><p id='rights'>Copyright 2018</p><p>References to websites were correct at the time of writing.</p><p>978 1 78453 942 9</p><p>eISBN: 978 1 78672 300 0</p><p>ePDF: 978 1 78673 300 9</p><p id='dedication'>To Susana</p></body></html>", &[("rights", true), ("dedication", false)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].isbns.len(), 3);
    }

    #[test]
    fn copyright_variants_and_inline_markup() {
        for marker in ["©", "&copy;", "&#169;", "&#xA9;", "(c)", "Copyright"] {
            let found = inspect(&format!("<html><body><p>{marker} 2004</p><p>ISBN <b>9780821338278</b></p></body></html>"), &[]);
            assert_eq!(found.len(), 1, "{marker}");
        }
    }
    #[test]
    fn copyright_credits_are_not_chapter_boundaries_and_empty_scripts_are_hidden() {
        let found = inspect("<html><head><script src='kobo.js'/><style>hidden</style></head><body><p>Copyright 2004</p><p>Foreword © 2010 by Ron Chernow</p><p>Introduction copyright © 2007</p><p>ISBN 9780821338278</p></body></html>", &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].isbns[0].isbn, "9780821338278");
    }

    #[test]
    fn hidden_and_body_copyright_mentions_do_not_supply_identifiers() {
        assert!(inspect("<html><head><title>Copyright ISBN 9780821338278</title></head><body><h1>Prologue</h1><p>Copyright ISBN 9780821338278</p></body></html>", &[]).is_empty());
    }
}
