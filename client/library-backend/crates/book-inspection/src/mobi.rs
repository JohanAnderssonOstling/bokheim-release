//! Metadata first; bounded native MOBI text inspection is an enrichment fallback.
use super::*;
use ::mobi::{
    headers::{Encryption, ExthRecord},
    Mobi, MobiMetadata,
};
use regex::Regex;
use std::sync::OnceLock;

pub fn inspect_mobi_metadata_reader(path: &Path, reader: impl Read) -> MetadataResult<BookRecord> {
    metadata(path, &MobiMetadata::from_read(reader)?)
}

fn metadata(path: &Path, raw: &MobiMetadata) -> MetadataResult<BookRecord> {
    if raw.encryption() != Encryption::No {
        return Err("DRM-protected MOBI books are not supported".into());
    }
    let mut book = fallback_metadata(path);
    let title = raw.title();
    if !title.trim().is_empty() {
        book.title = title;
    }
    book.description = raw.description().unwrap_or_default();
    for (key, values) in raw.exth.records() {
        // These are textual bibliographic fields. Binary layout/DRM records
        // must not become ISBNs merely because some bytes resemble digits.
        if !matches!(
            key,
            ExthRecord::Title
                | ExthRecord::Author
                | ExthRecord::Publisher
                | ExthRecord::Imprint
                | ExthRecord::Description
                | ExthRecord::Isbn
                | ExthRecord::Asin
                | ExthRecord::Subject
                | ExthRecord::Subjectcode
                | ExthRecord::Source
                | ExthRecord::Rights
                | ExthRecord::PublishDate
                | ExthRecord::Language
                | ExthRecord::Contributor
        ) {
            continue;
        }
        for value in values.into_iter().filter(|value| !value.trim().is_empty()) {
            let source = format!("mobi:exth:{}", key.position());
            if let Some(isbn) = book_model::from_metadata_value(&value) {
                book_model::push_isbn(&mut book.book.identifiers, isbn, Scope::Book)?;
            } else if *key == ExthRecord::Asin {
                let asin = value.trim().to_ascii_uppercase();
                if asin.len() == 10 && asin.starts_with('B') && asin.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    book.book.identifiers.push(Identifier::new(asin, Scheme::Asin, Scope::Book)?);
                }
            }
            match key {
                ExthRecord::Author => book.contributors.extend(author_names(&value).into_iter().filter_map(author_from_field)),
                ExthRecord::Publisher | ExthRecord::Imprint => {
                    if let Ok(publisher) = PublisherCredit::new(value) {
                        book.book.publishers.push(publisher);
                    }
                }
                ExthRecord::PublishDate => {
                    if let Ok(date) = BookDate::new(None, value, None, &source) {
                        book.book.dates.push(date);
                    }
                }
                ExthRecord::Language => {
                    if let Ok(language) = LanguageTag::parse(value) {
                        book.book.languages.push(language);
                    }
                }
                ExthRecord::Subject | ExthRecord::Subjectcode => {
                    let authority = if evidence::usable_lcc(&value) {
                        Some("lcc")
                    } else if !subject_projection::unified_subject_paths(subject_projection::BISAC_SYSTEM_ID, &value).is_empty() {
                        Some("bisac")
                    } else {
                        None
                    };
                    if let Ok(subject) = BookSubject::new(None, &value, &source, authority.map(str::to_owned), authority.map(|_| value.clone())) {
                        book.book.subjects.push(subject);
                    }
                }
                _ => {}
            }
        }
    }
    book.normalize_title();
    Ok(book)
}

pub fn inspect_mobi_pages_reader(path: &Path, reader: impl Read) -> MetadataResult<BookRecord> {
    // Bound input and decompression independently; inspection never expands the
    // entire book. The existing decoder is retained, including its encoding support.
    const MAX_INPUT: usize = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    reader.take((MAX_INPUT + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INPUT {
        return Err("MOBI exceeds the 16 MiB inspection limit".into());
    }
    let mut mobi = Mobi::new(&bytes)?;
    let mut book = metadata(path, &mobi.metadata)?;
    let start = mobi.readable_records_range().start;
    let end = mobi.readable_records_range().end.min(start + 32);
    if start >= end || end > mobi.metadata.records.records.len() {
        return Err("Invalid MOBI text record range".into());
    }
    let offsets = &mobi.metadata.records.records;
    if offsets.iter().any(|record| record.offset as usize > mobi.content.len()) {
        return Err("Invalid MOBI record offsets".into());
    }
    if offsets[start..end].windows(2).any(|pair| pair[1].offset - pair[0].offset > 64 * 1024) {
        return Err("Oversized MOBI text record".into());
    }
    mobi.metadata.mobi.first_non_book_index = end as u32;
    let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mobi.content_as_string_lossy())).map_err(|_| "MOBI text decoder rejected malformed content")?;
    inspect_markup(&mut book, &text);
    Ok(book)
}

fn inspect_markup(book: &mut BookRecord, markup: &str) {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    static BLOCKS: OnceLock<Regex> = OnceLock::new();
    static PAGES: OnceLock<Regex> = OnceLock::new();
    static HIDDEN: OnceLock<Regex> = OnceLock::new();
    static ENTITIES: OnceLock<Regex> = OnceLock::new();
    static CIP: OnceLock<Regex> = OnceLock::new();
    let end = markup.char_indices().nth(128 * 1024).map_or(markup.len(), |(i, _)| i);
    let markup = &markup[..end];
    let visible = HIDDEN.get_or_init(|| Regex::new(r"(?is)<(script|style|head)\b[^>]*>.*?</(?:script|style|head)>").unwrap()).replace_all(markup, "");
    let pages = PAGES.get_or_init(|| Regex::new(r"(?is)<mbp:pagebreak\b[^>]*>").unwrap()).replace_all(&visible, "\x0c");
    let blocks = BLOCKS.get_or_init(|| Regex::new(r"(?is)</?(?:p|div|br|h[1-6])\b[^>]*>").unwrap()).replace_all(&pages, "\n");
    // Inline tags must not split digits or cutters in a printed code.
    let plain = TAGS.get_or_init(|| Regex::new(r"(?s)<[^>]*>").unwrap()).replace_all(&blocks, "");
    let plain = plain.replace("&nbsp;", " ").replace("&copy;", "©").replace("&#x00A9;", "©");
    // One malformed/unknown entity must not prevent numeric entities elsewhere
    // (including ISBN hyphens) from being decoded.
    let plain = ENTITIES
        .get_or_init(|| Regex::new(r"&(?:#[0-9]+|#x[0-9a-fA-F]+|amp|lt|gt|quot|apos);").unwrap())
        .replace_all(&plain, |captures: &regex::Captures<'_>| quick_xml::escape::unescape(&captures[0]).map(|value| value.into_owned()).unwrap_or_else(|_| captures[0].to_owned()));
    let mut sections = Vec::new();
    for (i, page) in plain.split('\x0c').take(64).enumerate() {
        if page.len() <= 16_384 {
            let section = evidence::inspect_section(book, &format!("mobi:frontmatter:{i}"), page);
            if section.accepted {
                sections.push(section);
            }
        }
        // Some files omit a page break between a series list and its CIP block.
        // Anchor an additional bounded section to the printed CIP heading.
        for marker in CIP.get_or_init(|| Regex::new(r"(?i)library of congress catalog(?:uing|ing)[ -]in[ -]book").unwrap()).find_iter(page).take(2) {
            if marker.start() < 2048 {
                continue;
            }
            let tail = &page[marker.start()..];
            let end = tail.char_indices().nth(6500).map_or(tail.len(), |(i, _)| i);
            let section = evidence::inspect_section(book, &format!("mobi:frontmatter:{i}:cip"), &tail[..end]);
            if section.accepted {
                sections.push(section);
            }
        }
    }
    evidence::apply_evidence(book, &sections, "mobi");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_all_authors_identifiers_and_book_fields() {
        let mut raw = MobiMetadata::default();
        for (key, value) in [
            (ExthRecord::Author, "First Author"),
            (ExthRecord::Author, "Second Author"),
            (ExthRecord::Publisher, "Oxford University Press"),
            (ExthRecord::PublishDate, "2002"),
            (ExthRecord::Isbn, "0-19-285421-6"),
            (ExthRecord::Asin, "B005DKR49W"),
            (ExthRecord::Subjectcode, "B74"),
            (ExthRecord::Language, "en"),
        ] {
            raw.exth.records.entry(key).or_default().push(value.as_bytes().to_vec());
        }
        let book = metadata(Path::new("Philosophy.mobi"), &raw).unwrap();
        assert_eq!(book.contributors.len(), 2);
        assert_eq!(book.book.publishers.len(), 1);
        assert_eq!(book.book.dates.len(), 1);
        assert!(book.book.identifiers.iter().any(|id| id.scheme() == &Scheme::Isbn));
        assert!(book.book.identifiers.iter().any(|id| id.scheme() == &Scheme::Asin));
        assert!(book.book.subjects.iter().any(|s| s.code() == Some("B74")));
    }
    #[test]
    fn invalid_asin_uuid_is_not_identity_and_mislabelled_isbn_is_recovered() {
        let mut raw = MobiMetadata::default();
        raw.exth.records.insert(ExthRecord::Asin, vec![b"9780192854216".to_vec(), b"814e491c-9ed1-43a5-b4ee-517c1a9b772c".to_vec()]);
        let book = metadata(Path::new("book.mobi"), &raw).unwrap();
        assert_eq!(book.book.identifiers.len(), 1);
        assert_eq!(book.book.identifiers[0].scheme(), &Scheme::Isbn);
    }
    #[test]
    fn decodes_isbn_entities_independently_and_accepts_cip_tables() {
        let mut book = fallback_metadata(Path::new("book.mobi"));
        inspect_markup(&mut book, "<p>Copyright &copy; 2010 &unknown;</p><p>ISBN 978&#8211;0&#8211;19&#8211;956051&#8211;6</p>");
        assert!(related_isbn::lookup_candidates(&book.book.identifiers).contains(&"9780199560516".to_owned()));
        let mut table = fallback_metadata(Path::new("book.mobi"));
        inspect_markup(&mut table, "<p>title : Hinduism author : Kim Knott publisher : Oxford isbn13 : 9780192853417 lcc : BL1202.K564 1998</p>");
        assert!(table.book.subjects.iter().any(|subject| subject.code().is_some_and(|code| code.starts_with("BL1202"))));
    }
    #[test]
    fn extracts_text_candidates_but_not_html_attributes() {
        let mut book = fallback_metadata(Path::new("book.mobi"));
        inspect_markup(
            &mut book,
            "<html><body><p>Copyright © 2002</p><p>Library of Congress Cataloging in Book Data</p><p><font>B</font><font>74</font> .C83 2002</p><p>ISBN 0–19–285421–6</p><mbp:pagebreak/><p>References</p></body></html>",
        );
        assert!(book.book.subjects.iter().any(|s| s.code() == Some("B74 .C83 2002")));
        assert!(!related_isbn::lookup_candidates(&book.book.identifiers).is_empty());
        let mut other = fallback_metadata(Path::new("book.mobi"));
        inspect_markup(&mut other, "<p>References</p><p>Copyright 2002</p><p>ISBN 0–19–285421–6</p>");
        assert!(!other.book.identifiers.is_empty());
        other.book.identifiers.clear();
        inspect_markup(&mut other, "<p>Copyright 2002</p><a filepos='9780192854216'>Read</a>");
        assert!(related_isbn::lookup_candidates(&other.book.identifiers).is_empty());
    }
}
