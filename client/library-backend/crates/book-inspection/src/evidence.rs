//! Network-free bibliographic evidence rules shared by device and authority ingestion.
//! A checksum is necessary, but never sufficient, to identify the current book.
use book_model::{BookRecord, BookSubject, Identifier, Scheme, Scope};
use regex::Regex;
use std::sync::OnceLock;

pub const INSPECTION_VERSION: u32 = 10;
pub const EDGE_SECTIONS: usize = 8;

const LOCAL_CIP_SOURCE_PREFIX: &str = "inspection:local-cip:";

pub use book_enrichment::evidence::{IsbnEvidence, IsbnScope, SectionEvidence, MAX_SECTION_BYTES};

// Cross a line boundary only around an explicit hyphen, so printing-number
// lines cannot be absorbed into the ISBN. PDF extraction often isolates hyphens.
fn isbn_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(?P<isbn>(?:[0-9X](?:[\t \x{00a0}]*|\s*[-–‐‑]\s*)){9,}[0-9X])\b[\t \x{00a0}]*(?:\((?P<after>[^)\n]{1,40})\))?").unwrap())
}

fn complete_isbn(value: &str) -> Option<String> {
    book_model::from_content_candidate(value)
}

fn number_run_isbns(value: &str) -> Vec<String> {
    if let Some(isbn) = complete_isbn(value) {
        return vec![isbn];
    }
    // Some copyright pages print the ISBN-10 and ISBN-13 side by side.
    // Split only at whitespace and only if BOTH complete values validate.
    let digits = value.chars().filter(|c| c.is_ascii_digit() || matches!(c, 'X' | 'x')).count();
    if !matches!(digits, 20 | 23 | 26) {
        return Vec::new();
    }
    let parts: Vec<_> = value.split_whitespace().collect();
    for split in 1..parts.len() {
        if parts[split - 1].ends_with(['-', '–', '‐', '‑']) || parts[split].starts_with(['-', '–', '‐', '‑']) {
            continue;
        }
        if let (Some(left), Some(right)) = (complete_isbn(&parts[..split].concat()), complete_isbn(&parts[split..].concat())) {
            return vec![left, right];
        }
    }
    Vec::new()
}

const ISBN_SEARCH_CHARS: usize = 128;

fn isbn_marker_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(?:e-?)?I\s{0,4}S\s{0,4}B\s{0,4}N(?:[\t ]*[-:]?[\t ]*1[03]\b)?\b").unwrap())
}

/// Inspect a bounded bibliographic page independently of embedded identity fields.
pub fn inspect_section(_book: &BookRecord, location: &str, text: &str) -> SectionEvidence {
    let mut out = SectionEvidence { location: location.chars().take(512).collect(), ..Default::default() };
    if text.len() > MAX_SECTION_BYTES {
        out.reason = "section exceeds inspection limit".into();
        return out;
    }
    // Extract candidates independently of headings or copyright/CIP wording.
    // Edition scope keeps a discovered ISBN separate from the book's identity.
    out.accepted = true;
    out.reason = "local identifier candidates".into();
    for marker in isbn_marker_pattern().find_iter(text).take(16) {
        let tail = &text[marker.end()..];
        let limit = tail.char_indices().nth(ISBN_SEARCH_CHARS).map_or(tail.len(), |(i, _)| i);
        let end = isbn_marker_pattern().find(tail).map_or(limit, |next| next.start().min(limit));
        let window = &tail[..end];
        let mut matched = isbn_pattern().captures_iter(window).find_map(|c| {
            // Do not accept a number cut short by the bounded search window.
            if c.name("isbn").unwrap().end() == end && tail[end..].chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                return None;
            }
            complete_isbn(&c["isbn"]).map(|isbn| (c, isbn))
        });
        // PDF visual-line extraction can put each numeric group on its own line.
        // Try a whole whitespace-separated run only after the ordinary pass fails;
        // never guess digits or accept a prefix of a longer numeric run.
        static MULTILINE: OnceLock<Regex> = OnceLock::new();
        let multiline = MULTILINE.get_or_init(|| Regex::new(r"(?i)\b(?P<isbn>[0-9](?:[\s\-–‐‑]*[0-9X]){9,})\b(?:[\t ]*\((?P<after>[^)\n]{1,40})\))?").unwrap());
        if matched.is_none() {
            matched = multiline.captures_iter(window).find_map(|c| {
                if c.name("isbn")?.end() == end && tail[end..].chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                    return None;
                }
                complete_isbn(&c["isbn"]).map(|isbn| (c, isbn))
            });
        }
        let Some((c, isbn)) = matched else { continue };
        // Labels can appear anywhere between the marker and number, or in
        // parentheses immediately after it. They retain edition scope.
        let label = format!("{} {}", &window[..c.name("isbn").unwrap().start()], c.name("after").map_or("", |m| m.as_str())).to_lowercase();
        let scope = if marker.as_str().to_ascii_lowercase().starts_with('e') || ["ebook", "e-book", "epub", "electronic", "pdf"].iter().any(|s| label.contains(s)) {
            IsbnScope::ElectronicEdition
        } else if ["hardcover", "paperback", "pbk", "cloth", "paper", "print"].iter().any(|s| label.contains(s)) {
            IsbnScope::RelatedPrintEdition
        } else {
            IsbnScope::Unspecified
        };
        let format = if label.contains("pdf") {
            Some("pdf".into())
        } else if label.contains("epub") {
            Some("epub".into())
        } else {
            None
        };
        let evidence = IsbnEvidence { isbn, scope, format };
        if !out.isbns.contains(&evidence) {
            out.isbns.push(evidence);
        }
    }
    // Search the entire accepted block without requiring an ISBN label. Match
    // maximal number runs so a valid checksum substring in a longer number is
    // not accepted. Keep the labelled pass above for richer edition scope.
    static NUMBER_RUN: OnceLock<Regex> = OnceLock::new();
    let numbers = NUMBER_RUN.get_or_init(|| Regex::new(r"(?i)\b[0-9](?:(?:[\t \x{00a0}]*|\s*[-–‐‑]\s*)[0-9X]){9,}\b").unwrap());
    for number in numbers.find_iter(text) {
        if out.isbns.len() >= 16 {
            break;
        }
        let values = number_run_isbns(number.as_str());
        // Unlabelled values are lookup references, not electronic identities.
        // Preserve an explicit ePDF label so it cannot identify the EPUB edition.
        let prefix = text[..number.start()].rsplit('\n').next().unwrap_or_default().trim().trim_end_matches(':').trim().to_lowercase();
        if ["control number", "catalog card number", "lccn"].iter().any(|label| prefix.contains(label)) {
            continue;
        }
        let pdf = prefix == "epdf";
        for isbn in values {
            if out.isbns.len() >= 16 {
                break;
            }
            if out.isbns.iter().any(|e| e.isbn == isbn) || ["0123456789", "1234567890", "9876543210", "0987654321"].contains(&isbn.as_str()) {
                continue;
            }
            out.isbns.push(IsbnEvidence { isbn, scope: if pdf { IsbnScope::ElectronicEdition } else { IsbnScope::Unspecified }, format: pdf.then(|| "pdf".to_owned()) });
        }
    }
    {
        static LCC: OnceLock<Regex> = OnceLock::new();
        // Full CIP call numbers commonly include a Cutter. Some pages
        // concatenate a four-digit date directly onto the class
        // (HB37172008 .B55 2013); split that date back out before matching.
        let pattern = LCC.get_or_init(|| Regex::new(r"\b(?P<class>[A-Z]{1,3}[0-9]{1,4}(?:\.[0-9]+)?)(?P<class_year>(?:19|20)[0-9]{2})?\s*(?P<cutter>\.[A-Z][0-9]+(?:[A-Z][0-9]+)?)(?:\s+(?P<item_year>(?:19|20)[0-9]{2}))?\b").unwrap());
        for captures in pattern.captures_iter(text).take(16) {
            let class = captures.name("class").unwrap().as_str();
            let class_year = captures.name("class_year").map_or(String::new(), |year| format!(" {}", year.as_str()));
            let cutter = captures.name("cutter").unwrap().as_str();
            let item_year = captures.name("item_year").map_or(String::new(), |year| format!(" {}", year.as_str()));
            let code = format!("{class}{class_year} {cutter}{item_year}");
            // Publisher control numbers (e.g. QBI04-200396) are not LCC codes.
            if captures.get(0).is_some_and(|matched| text[matched.end()..].starts_with('-')) {
                continue;
            }
            if !out.lcc.contains(&code) {
                out.lcc.push(code);
            }
        }
        // A classification number need not have a Cutter. Accept class-only
        // values when explicitly labeled, including CIP layouts that put the
        // value on the following line. Also accept a code-only line in a
        // clearly identified CIP block, but only if no complete call number
        // was found in that block. This avoids treating code-like fragments
        // embedded in prose or publisher addresses as classifications.
        static LABELED_LCC: OnceLock<Regex> = OnceLock::new();
        let labeled = LABELED_LCC.get_or_init(|| Regex::new(r"(?im)^[\t ]*(?:classification[\t ]*:[\t ]*)?LCC[\t ]*:?[\t ]*(?:\r?\n[\t ]*)?(?P<class>[A-Z]{1,3}[0-9]{1,4}(?:\.[0-9]+)?)(?:[\t ]+(?P<year>(?:19|20)[0-9]{2}))?[\t ]*$").unwrap());
        for captures in labeled.captures_iter(text).take(16) {
            let class = captures.name("class").unwrap().as_str().to_ascii_uppercase();
            let year = captures.name("year").map_or(String::new(), |year| format!(" {}", year.as_str()));
            let code = format!("{class}{year}");
            if !out.lcc.contains(&code) {
                out.lcc.push(code);
            }
        }
        static CIP_HEADING: OnceLock<Regex> = OnceLock::new();
        let cip_heading = CIP_HEADING.get_or_init(|| Regex::new(r"(?i)\b(?:library of congress cataloging|cataloging[- ]in[- ]publication)\b").unwrap());
        if out.lcc.is_empty() && cip_heading.is_match(text) {
            static STANDALONE_LCC: OnceLock<Regex> = OnceLock::new();
            let standalone = STANDALONE_LCC.get_or_init(|| Regex::new(r"(?m)^[\t ]*(?P<class>[A-Z]{1,3}[0-9]{1,4}(?:\.[0-9]+)?)(?:[\t ]+(?P<year>(?:19|20)[0-9]{2}))?[\t ]*$").unwrap());
            if let Some(captures) = standalone.captures(text) {
                let class = captures.name("class").unwrap().as_str().to_ascii_uppercase();
                let year = captures.name("year").map_or(String::new(), |year| format!(" {}", year.as_str()));
                out.lcc.push(format!("{class}{year}"));
            }
        }
    }
    if out.isbns.is_empty() && out.lcc.is_empty() {
        out.accepted = false;
        out.reason = "no usable bibliographic identifiers or classification".into();
    }
    out
}

/// Extract identifier evidence from image-derived text.
pub fn inspect_isbn_image(book: &BookRecord, location: &str, text: &str) -> SectionEvidence {
    inspect_section(book, location, text)
}

/// Resolution is separate from extraction: retain rejected candidates for auditing,
/// but they must not displace metadata or suppress ISBN/authority fallback.
pub fn usable_lcc(code: &str) -> bool {
    subject_projection::unified_lcc_has_numeric_coverage(code) && !subject_projection::unified_lcc_match_report(code).assignments.is_empty()
}

fn promotable_lcc(_section: &SectionEvidence, code: &str) -> bool {
    usable_lcc(code)
}

/// Promote only locally established evidence. Every accepted ISBN becomes a
/// `Scope::Edition` identifier: a checksum-valid reference that may name a
/// different edition of this book, never a claim that it is the book itself.
pub fn apply_evidence(book: &mut BookRecord, evidence: &[SectionEvidence], format: &str) {
    // Rebuild our own prior page-derived classifications from current evidence.
    // Never let free-text inspection replace an LCC code from package or
    // catalog metadata, which is a higher-confidence source.
    book.book.subjects.retain(|subject| {
        !(subject.authority().is_some_and(|authority| authority.eq_ignore_ascii_case("lcc")) && subject.source().starts_with(LOCAL_CIP_SOURCE_PREFIX))
    });
    let has_metadata_lcc = book.book.subjects.iter().any(|subject| {
        subject.authority().is_some_and(|authority| authority.eq_ignore_ascii_case("lcc"))
            && subject.code().is_some_and(|code| !code.trim().is_empty())
    });
    for section in evidence.iter().filter(|e| e.accepted).take(32) {
        if !has_metadata_lcc {
            let source = format!("{LOCAL_CIP_SOURCE_PREFIX}{}", section.location);
            for code in &section.lcc {
                if !promotable_lcc(section, code) {
                    continue;
                }
                if book.book.subjects.iter().any(|subject| subject.authority().is_some_and(|authority| authority.eq_ignore_ascii_case("lcc")) && subject.code() == Some(code)) {
                    continue;
                }
                if let Ok(subject) = BookSubject::new(None, code, &source, Some("lcc".into()), Some(code.clone())) {
                    book.book.subjects.push(subject);
                }
            }
        }
        for candidate in &section.isbns {
            if candidate.scope != IsbnScope::ElectronicEdition || candidate.format.as_deref().is_some_and(|f| f != format) {
                continue;
            }
            // A different existing package ISBN is a conflict, not permission to replace it.
            let existing: Vec<_> = book.book.identifiers.iter().filter(|i| i.scheme() == &Scheme::Isbn).filter_map(Identifier::canonical_value).collect();
            if !existing.is_empty() {
                continue;
            }
            let electronic: Vec<_> = evidence.iter().filter(|s| s.accepted).flat_map(|s| &s.isbns).filter(|i| i.scope == IsbnScope::ElectronicEdition && i.format.as_deref().is_none_or(|f| f == format)).map(|i| &i.isbn).collect();
            if electronic.iter().any(|isbn| **isbn != candidate.isbn) {
                continue;
            }
            if let Ok(id) = Identifier::new(&candidate.isbn, Scheme::Isbn, Scope::Book) {
                book.book.identifiers.push(id);
            }
        }
    }
    // Every checksum-valid ISBN from an accepted section is a candidate for
    // enrichment lookups, whatever edition it names. Deduplicated against
    // both prior evidence and the book's own promoted identifiers above.
    let mut related_isbns: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for section in evidence.iter().filter(|e| e.accepted).take(32) {
        for candidate in &section.isbns {
            if let Some(isbn) = book_model::from_content_candidate(&candidate.isbn) {
                related_isbns.insert(isbn);
            }
        }
    }
    for isbn in related_isbns.into_iter().take(16) {
        if book.book.identifiers.iter().any(|existing| existing.scheme() == &Scheme::Isbn && existing.canonical_value().as_deref() == Some(isbn.as_str())) {
            continue;
        }
        if let Ok(id) = Identifier::new(&isbn, Scheme::Isbn, Scope::Edition) {
            book.book.identifiers.push(id);
        }
    }
}

/// Visible XHTML text, preserving block boundaries and excluding head/script/style.
/// Malformed markup is skipped rather than interpreted as trustworthy text.
pub fn xhtml_text(markup: &str) -> Option<String> {
    if markup.len() > MAX_SECTION_BYTES {
        return None;
    }
    let doc = roxmltree::Document::parse(markup).ok()?;
    let mut out = String::new();
    for node in doc.descendants().filter(|n| n.is_text()) {
        if node.ancestors().any(|n| n.is_element() && matches!(n.tag_name().name(), "head" | "script" | "style")) {
            continue;
        }
        out.push_str(node.text().unwrap_or_default());
        out.push(' ');
        if node.next_sibling().is_none() {
            out.push('\n');
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE};

    fn book(title: &str) -> BookRecord {
        BookRecord { title: title.into(), subtitle: None, contributors: vec![Contributor::new("Ada Author", AUTHOR_MARC_RELATOR_CODE).unwrap()], description: String::new(), book: Default::default() }
    }
    fn cip(title: &str) -> String {
        format!("Copyright © Ada Author\nLibrary of Congress Cataloging-in-Book Data\nTitle: {title} / Ada Author\nISBN 978-0-8213-3827-8 (hardcover)\nISBN 978-0-393-24327-7 (epub)\nClassification: LCC Q125 .A12 2023\n")
    }
    #[test]
    fn own_cip_preserves_print_scope_and_promotes_only_electronic_identity() {
        let mut b = book("Science History");
        let e = inspect_section(&b, "back/copyright.xhtml", &cip(&b.title));
        assert!(e.accepted);
        assert_eq!(e.isbns.len(), 2);
        assert_eq!(e.isbns[0].scope, IsbnScope::RelatedPrintEdition);
        apply_evidence(&mut b, &[e], "epub");
        assert_eq!(b.book.identifiers.len(), 2);
        assert_eq!(b.book.identifiers[0].canonical_value().as_deref(), Some("9780393243277"));
        assert_eq!(b.book.subjects[0].code(), Some("Q125 .A12 2023"));
        assert!(b.book.identifiers.iter().any(|id| id.scope() == Scope::Edition && id.canonical_value().as_deref() == Some("9780821338278")));
    }
    #[test]
    fn headings_and_missing_copyright_do_not_reject_isbns() {
        let b = book("Science History");
        for text in [format!("References\n{}", cip(&b.title)), format!("Also by Ada Author\n{}", cip(&b.title)), "Science History by Ada Author\nISBN 9780821338278".into()] {
            let e = inspect_section(&b, "last.xhtml", &text);
            assert!(e.accepted, "{text}");
            assert!(!e.isbns.is_empty());
        }
    }
    #[test]
    fn publication_and_mixed_promotional_pages_retain_identifiers() {
        let b = book("Oxford handbook");
        let oxford = inspect_section(&b, "pdf:page:2", "Library of Congress Cataloging in Publication Data\nISBN-13: 978–0–19–921932–2\nJZ1242.O94 2008");
        assert_eq!(oxford.isbns[0].isbn, "9780199219322");
        assert_eq!(oxford.lcc, ["JZ1242.O94 2008"]);
        let sutton = inspect_section(&b, "pdf:page:5", "Other Books by Author\n©1983 Antony Sutton\nISBN 0-9720207-4-8 Paper\nISBN 0-9720207-0-5 Hardcover");
        assert_eq!(sutton.isbns.len(), 2);
    }
    #[test]
    fn copyright_cip_takes_priority_over_embedded_volume_title() {
        let b = book("World History Volume 4");
        let text = format!("{}\n{}", b.title, cip("World History Volume 2"));
        assert!(inspect_section(&b, "copyright.xhtml", &text).accepted);
    }
    #[test]
    fn embedded_collection_title_does_not_veto_page_evidence() {
        let b = book("Complete Science History");
        assert!(inspect_section(&b, "volume/copyright.xhtml", &cip(&b.title)).accepted);
    }
    #[test]
    fn missing_or_wrong_embedded_authors_do_not_veto_page_evidence() {
        let mut b = book("Science History");
        let text = cip(&b.title).replace("Ada Author", "Alice Other");
        assert!(inspect_section(&b, "copyright", &text).accepted);
        b.contributors.clear();
        assert!(inspect_section(&b, "copyright", &cip(&b.title)).accepted);
    }
    #[test]
    fn invalid_checksum_and_pdf_isbn_are_not_book_identities() {
        let mut b = book("Science History");
        let text = cip(&b.title).replace("978-0-8213-3827-8", "978-0-8213-3827-9").replace("(epub)", "(pdf)");
        let e = inspect_section(&b, "copyright", &text);
        assert_eq!(e.isbns.len(), 1);
        apply_evidence(&mut b, &[e], "epub");
        assert!(b.book.identifiers.iter().all(|id| id.scope() != Scope::Book));
        assert!(b.book.identifiers.iter().any(|id| id.scope() == Scope::Edition && id.canonical_value().as_deref() == Some("9780393243277")));
    }
    #[test]
    fn conflicting_existing_isbn_keeps_book_identity_and_records_edition() {
        let mut b = book("Science History");
        b.book.identifiers.push(Identifier::new("9780821338278", Scheme::Isbn, Scope::Book).unwrap());
        let e = inspect_section(&b, "copyright", &cip(&b.title));
        let before = b.book.identifiers.clone();
        apply_evidence(&mut b, &[e], "epub");
        assert_eq!(b.book.identifiers[0], before[0]);
        assert!(b.book.identifiers.iter().filter(|id| id.scope() == Scope::Book).count() == 1);
        assert!(b.book.identifiers.iter().any(|id| id.scope() == Scope::Edition && id.canonical_value().as_deref() == Some("9780393243277")));
    }
    #[test]
    fn copyright_isbn_does_not_require_title_on_the_same_page() {
        let b = book("Science History");
        assert!(!inspect_section(&b, "page1", "Science History by Ada Author").accepted);
        assert!(inspect_section(&b, "page2", "Copyright\nISBN 9780821338278").accepted);
    }
    #[test]
    fn hidden_head_and_script_text_do_not_establish_identity() {
        let text = xhtml_text("<html><head><title>Science History by Ada Author</title></head><body><script>ISBN 9780821338278</script><p>Copyright</p></body></html>").unwrap();
        assert!(!text.contains("Science") && !text.contains("ISBN"));
        assert!(!inspect_section(&book("Science History"), "copyright", &text).accepted);
    }
    #[test]
    fn bad_xmp_cannot_veto_copyright_codes_and_page_lcc_replaces_metadata() {
        let mut b = book("0465002566-text.qxd:0465002566 text");
        b.contributors = vec![Contributor::new("Linda Mark", AUTHOR_MARC_RELATOR_CODE).unwrap()];
        b.book.subjects.push(BookSubject::new(None, "Wrong", "package", Some("lcc".into()), Some("PT2381".into())).unwrap());
        let text = "Copyright Aida D. Donald\nLibrary of Congress Cataloging-in-Book Data\nLion in the White House\nISBN 978-0-465-00213-9 (paperback)\nE757.D658 2007";
        let e = inspect_section(&b, "pdf:page:6", text);
        assert!(e.accepted);
        assert_eq!(e.isbns[0].isbn, "9780465002139");
        apply_evidence(&mut b, &[e], "pdf");
        assert_eq!(b.book.subjects.len(), 1);
        assert_eq!(b.book.subjects[0].code(), Some("E757.D658 2007"));
        assert_eq!(crate::related_isbn::lookup_candidates(&b.book.identifiers), ["9780465002139"]);
    }
    #[test]
    fn isbn_does_not_consume_the_following_printing_number_line() {
        let e = inspect_section(&book("Locke"), "Locke_split_004.html", "© John Dunn 1984\nLibrary of Congress Cataloging in Book Data\nISBN 0–19–280394–8\n3 5 7 9 10 8 6 4 2\nTypeset by RefineCatch");
        assert!(e.accepted);
        assert_eq!(e.isbns.len(), 1);
        assert_eq!(e.isbns[0].isbn, "0192803948");
    }
    #[test]
    fn pdf_isbn_fragments_join_across_hyphens_but_not_printing_lines() {
        for dash in ["-", "–", "‐", "‑"] {
            for separator in [format!(" \n{dash} \n"), format!("{dash}\n"), format!("\n{dash}")] {
                let isbn = ["0", "19", "280424", "3"].join(&separator);
                let e = inspect_section(&book("ATHEISM"), "pdf:page:2", &format!("© Julian Baggini, 2003\nISBN {isbn}\n\n3 5 7 9 10 8 6 4 2\nTypeset by RefineCatch"));
                assert!(e.accepted, "{separator:?}");
                assert_eq!(e.isbns.len(), 1);
                assert_eq!(e.isbns[0].isbn, "0192804243");
            }
        }
        let invalid = inspect_section(&book("ATHEISM"), "pdf:page:2", "© Julian Baggini\nISBN 0\n-\n19\n-\n280424\n-\n4\n3 5 7 9");
        assert!(invalid.isbns.is_empty(), "hyphen recovery must retain checksum validation");
    }
    #[test]
    fn format_labels_before_isbn_numbers_preserve_edition_scope() {
        let e = inspect_section(&book("UX for Developers"), "pdf:page:3", "UX for Developers\nISBN-13 (pbk): 978-1-4842-4226-1\tISBN-13 (electronic): 978-1-4842-4227-8\nCopyright © 2019 by Westley Knight");
        assert!(e.accepted);
        assert_eq!(e.isbns.len(), 2);
        assert_eq!(e.isbns[0].isbn, "9781484242261");
        assert_eq!(e.isbns[0].scope, IsbnScope::RelatedPrintEdition);
        assert_eq!(e.isbns[1].isbn, "9781484242278");
        assert_eq!(e.isbns[1].scope, IsbnScope::ElectronicEdition);
    }
    #[test]
    fn isbn_anchor_search_is_bounded_and_stops_at_next_marker() {
        let b = book("UX for Developers");
        let inspect = |s: &str| inspect_section(&b, "copyright", &format!("Copyright Westley Knight\n{s}"));
        let e = inspect("ISBN for this paperback edition is listed below:\n978-1-4842-4226-1\nISBN electronic edition: 978-1-4842-4227-8");
        assert_eq!(e.isbns.len(), 2);
        assert_eq!(e.isbns[0].scope, IsbnScope::RelatedPrintEdition);
        assert_eq!(e.isbns[1].scope, IsbnScope::ElectronicEdition);
        let split = inspect("IS BN-13 (electronic): 978-1-4842-4227-8");
        assert_eq!(split.isbns[0].isbn, "9781484242278");
        assert_eq!(split.isbns[0].scope, IsbnScope::ElectronicEdition);
        let e = inspect("ISBN paperback: unavailable. ISBN electronic: 9781484242278");
        assert_eq!(e.isbns.len(), 1);
        assert_eq!(e.isbns[0].scope, IsbnScope::ElectronicEdition);
        assert_eq!(inspect(&format!("ISBN {} 9781484242261", "x".repeat(ISBN_SEARCH_CHARS))).isbns[0].isbn, "9781484242261");
        let e = inspect("ISBN invalid 9781484242262; corrected: 9781484242261");
        assert_eq!(e.isbns.len(), 1);
        assert_eq!(e.isbns[0].isbn, "9781484242261");
    }
    #[test]
    fn unlabelled_numbers_require_complete_valid_isbns() {
        let b = book("Franco");
        let e = inspect_section(&b, "copyright", "Copyright 2018\nReferences to websites were correct at the time of writing.\n978 1 78453 942 9\n978-1-78672-300-0\nePDF: 978 1 78673 300 9\n0-8044-2957-X");
        assert_eq!(e.isbns.iter().map(|i| i.isbn.as_str()).collect::<Vec<_>>(), ["9781784539429", "9781786723000", "9781786733009", "080442957X"]);
        assert_eq!(e.isbns[2].format.as_deref(), Some("pdf"));
        for bad in ["9781784539428", "1239781784539429", "9781784539429123", "978 1 78453 942 9 123", "3 5 7 9 10 8 6 4 2", "1234567890128"] {
            assert!(inspect_section(&b, "copyright", &format!("Copyright\n{bad}")).isbns.is_empty(), "{bad}");
            assert!(inspect_section(&b, "copyright", &format!("Copyright\nISBN {bad}")).isbns.is_empty(), "labelled {bad}");
        }
        assert_eq!(inspect_section(&b, "chapter", "9781784539429").isbns[0].isbn, "9781784539429");
        assert_eq!(inspect_section(&b, "copyright", "Copyright\nReferences\n9781784539429").isbns[0].isbn, "9781784539429");
    }

    #[test]
    fn isbn_length_labels_and_non_isbn_numbers_are_distinguished() {
        let b = book("Emotion");
        let e = inspect_section(&b, "copyright", "Copyright\nISBN: 13 978–0–19–280461–7\nISBN: 10 0–19–280461–8");
        assert_eq!(e.isbns.len(), 2);
        let e = inspect_section(&b, "copyright", "Copyright\nISBN 0–19–280360–3 978–0–19–280360–3");
        assert_eq!(e.isbns.iter().map(|i| i.isbn.as_str()).collect::<Vec<_>>(), ["0192803603", "9780192803603"]);
        for line in ["0 1 2 3 4 5 6 7 8 9", "Library of Congress Control Number: 2013933932"] {
            assert!(inspect_section(&b, "copyright", &format!("Copyright\n{line}")).isbns.is_empty());
        }
    }

    #[test]
    fn unresolvable_inline_lcc_is_retained_only_as_candidate() {
        let mut b = book("Alexander Hamilton");
        let e = inspect_section(&b, "copyright", "Copyright 2004\nI. Title. E3002.6.H2C48 2004");
        assert_eq!(e.lcc, ["E3002.6.H2C48 2004"]);
        assert!(!usable_lcc(&e.lcc[0]), "{:?}", subject_projection::unified_lcc_match_report(&e.lcc[0]));
        apply_evidence(&mut b, &[e], "epub");
        assert!(b.book.subjects.is_empty());
    }

    #[test]
    fn malformed_and_oversized_sections_are_skipped() {
        assert!(xhtml_text("<broken").is_none());
        assert!(!inspect_section(&book("Science History"), "copyright", &"x".repeat(MAX_SECTION_BYTES + 1)).accepted);
    }
    #[test]
    fn final_cip_page_without_repeated_heading_is_evidence() {
        let e = inspect_section(
            &book("mangled metadata"),
            "pdf:page:231",
            "Hofmekler, Ori, 1952–\nThe anti-estrogenic diet / by Ori Hofmekler.\np. cm.\neISBN: 978-1-55643-854-7\n1. Anti-estrogenic diet. I. Title.\nRM231.2.H64 2007\n615.9—dc22\n2006103186",
        );
        assert!(e.accepted);
        assert_eq!(e.isbns[0].isbn, "9781556438547");
        assert_eq!(e.isbns[0].scope, IsbnScope::ElectronicEdition);
        assert_eq!(e.lcc, ["RM231.2.H64 2007"]);
    }

    #[test]
    fn isbn_candidates_do_not_require_a_page_kind() {
        let b = book("damaged title");
        let strip = inspect_isbn_image(&b, "epub:image:copyright.png", "ISBN 0-19-285458-5");
        assert_eq!(strip.isbns[0].isbn, "0192854585");
        let cover = inspect_isbn_image(&b, "pdf:page:127:back-cover", "A remarkable book.\nISBN 0-9613921-1-8");
        assert_eq!(cover.isbns[0].isbn, "0961392118");
        assert!(inspect_isbn_image(&b, "page:5", "Inspired by Lions’ Commentary (ISBN 1-57398-013-7)").accepted);
        assert!(inspect_isbn_image(&b, "page:5:back-cover", "Other books\nISBN 1-57398-013-7").accepted);
    }

    #[test]
    fn labelled_groups_can_span_lines_without_hyphens() {
        let e = inspect_section(&book("Operating System Concepts"), "copyright", "Copyright 2012\nISBN 978\n1\n118\n06333\n0\nPrinted in the USA");
        assert_eq!(e.isbns[0].isbn, "9781118063330");
        assert!(inspect_section(&book("x"), "copyright", "Copyright 2012\nISBN 0000000000").isbns.is_empty());
    }

    #[test]
    fn image_text_decimal_in_place_of_cutter_does_not_override_subjects() {
        let mut b = book("Color Design Workbook");
        let e = inspect_section(&b, "pdf:image:page:4", "Copyright 2006\nISBN 1-59253-192-X\nNC1000.576 2006");
        assert_eq!(e.lcc, ["NC1000.576 2006"]);
        apply_evidence(&mut b, &[e], "pdf");
        assert!(b.book.subjects.is_empty());
        assert_eq!(crate::related_isbn::lookup_candidates(&b.book.identifiers), ["159253192X"]);
    }
}
