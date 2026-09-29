//! Filename evidence for unusable identity fields; never infers identifiers.
use metadata_contract::identity_evidence::filename_identity_evidence;

pub fn unusable_identity(value: &str) -> bool {
    let value = value.trim();
    let compact = value.replace('-', "");
    matches!(value.to_ascii_lowercase().as_str(), "" | "unknown" | "untitled") || (compact.len() >= 20 && compact.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Common production labels, document filenames and standalone web addresses.
pub fn standard_title_placeholder(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    let label = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = label.replace('-', " ").replace('_', " ");
    let document_file = [".pdf", ".doc", ".docx", ".epub", ".mobi", ".azw", ".azw3", ".m4b", ".rtf", ".odt", ".indd", ".qxd", ".p65"]
        .iter()
        .any(|extension| lower.ends_with(extension) || lower.split_once(':').is_some_and(|(name, _)| name.ends_with(extension)));
    let address = lower.trim_matches(['<', '>', '(', ')', '[', ']']);
    let host = address.split('/').next().unwrap_or(address);
    let domain = !address.chars().any(char::is_whitespace)
        && [".org", ".com", ".net", ".edu", ".gov", ".info", ".biz", ".io", ".co", ".uk", ".se"]
            .iter()
            .any(|suffix| host.strip_suffix(suffix).is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))));
    matches!(label.as_str(), "ebook" | "cover" | "front cover" | "back cover" | "cover page" | "front matter" | "frontmatter" | "back matter" | "backmatter" | "title page" | "titlepage" | "table of contents" | "contents" | "copyright page")
        || ["_ebook", "-ebook", ".ebook"].iter().any(|suffix| lower.ends_with(suffix))
        || document_file
        || domain
        || address.starts_with("www.")
        || address.starts_with("http://")
        || address.starts_with("https://")
}

/// Standalone Kindle-style identifiers, optionally followed by the EBOK marker.
/// Require the whole field to match so books discussing identifiers keep their title.
pub fn kindle_identifier_title(value: &str) -> bool {
    let upper = value.trim().to_ascii_uppercase();
    let identifier =
        upper.strip_suffix("EBOK").and_then(|prefix| prefix.strip_suffix(|c: char| c.is_whitespace() || matches!(c, '_' | '-'))).map(|prefix| prefix.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '_' | '-'))).unwrap_or(&upper);
    identifier.len() == 10 && identifier.starts_with("B0") && identifier.bytes().all(|c| c.is_ascii_alphanumeric())
}

/// Ambiguous product-code shape; only replace it when a usable filename exists.
pub fn identifier_shaped_title(value: &str) -> bool {
    let value = value.trim();
    let token = value
        .get(value.len().saturating_sub(4)..)
        .filter(|suffix| suffix.eq_ignore_ascii_case("EBOK"))
        .and_then(|_| value.get(..value.len().saturating_sub(4)))
        .and_then(|prefix| prefix.strip_suffix(|c: char| c.is_whitespace() || matches!(c, '_' | '-')))
        .map(|prefix| prefix.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '_' | '-')))
        .unwrap_or(value);
    token.len() >= 10 && token.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) && token.bytes().filter(u8::is_ascii_digit).count() >= 2 && token.bytes().filter(u8::is_ascii_uppercase).count() >= 2
}

/// Long identifier tokens remain invalid when surrounded by ordinary title text.
pub(crate) fn contains_long_identifier(value: &str) -> bool {
    value
        .split(|c: char| !c.is_alphanumeric())
        .any(|token| token.len() >= 20 && token.is_ascii() && (token.bytes().all(|c| c.is_ascii_hexdigit()) || (token.bytes().filter(u8::is_ascii_digit).count() >= 2 && token.bytes().filter(u8::is_ascii_alphabetic).count() >= 2)))
}

/// Reject recognizable technical placeholders, not merely unusual book names.
pub fn unusable_title(value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let stem = [".pdf", ".epub", ".mobi", ".azw3", ".m4b"].iter().find_map(|extension| lower.strip_suffix(extension)).unwrap_or(&lower);
    let number = ["urn:isbn:", "isbn-13:", "isbn-10:", "isbn"].iter().find_map(|prefix| stem.trim().strip_prefix(prefix)).unwrap_or(stem).trim_start_matches([':', ' ', '-']);
    let compact = number.replace([' ', '-'], "");
    let numeric_identifier = compact.len() >= 8 && compact.bytes().all(|c| c.is_ascii_digit() || c == b'x');
    let placeholder = lower.strip_prefix("microsoft word - ").unwrap_or(&lower);
    let numbered_placeholder = ["document", "untitled"].iter().any(|prefix| placeholder.strip_prefix(prefix).is_some_and(|tail| tail.is_empty() || tail.bytes().all(|byte| byte.is_ascii_digit())));
    unusable_identity(value)
        || contains_long_identifier(value)
        || kindle_identifier_title(value)
        || standard_title_placeholder(value)
        || lower.strip_prefix("oup_").is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        || numeric_identifier
        || !super::filename_isbn::from_filename(value).is_empty()
        || unusable_identity(stem)
        || value.len() > 4096
        || value.chars().any(|c| c == '\u{fffd}' || (c.is_control() && !c.is_whitespace()))
        || !value.chars().any(char::is_alphanumeric)
        || numbered_placeholder
        || matches!(lower.as_str(), "true" | "none" | "null" | "n/a" | "microsoft word" | "adobe acrobat" | "acrobat distiller")
        || lower.starts_with("file://")
        || value.starts_with("\\\\")
        || (value.as_bytes().get(1) == Some(&b':') && value.as_bytes().get(2).is_some_and(|c| matches!(c, b'/' | b'\\')))
        || ["/home/", "/users/", "/tmp/", "/private/"].iter().any(|prefix| lower.starts_with(prefix))
}

/// Hosts supply original filenames, never content-addressed storage paths.
/// Embedded fields win independently; valid authors and years are preserved.
pub fn with_filename(title: String, authors: Vec<String>, year: Option<i32>, filename: &str) -> (String, Vec<String>, Option<i32>) {
    let stem = std::path::Path::new(filename).file_stem().and_then(|s| s.to_str()).unwrap_or(filename);
    if unusable_title(stem) && !contains_long_identifier(stem) {
        return (title, authors, year);
    }
    let evidence = filename_identity_evidence(stem);
    if unusable_title(&evidence.title) || identifier_shaped_title(&evidence.title) {
        return (title, authors, year);
    }
    let title = if unusable_title(&title) || identifier_shaped_title(&title) { evidence.title } else { title };
    let authors = if authors.iter().all(|a| unusable_identity(a)) {
        evidence
            .author
            .filter(|a| !unusable_identity(a))
            .map(|author| {
                let parts = author.split(',').map(str::trim).collect::<Vec<_>>();
                if parts.iter().all(|part| part.split_whitespace().count() >= 2) {
                    parts.into_iter().map(str::to_owned).collect()
                } else {
                    vec![author]
                }
            })
            .unwrap_or(authors)
    } else {
        authors
    };
    (title, authors, year.or(evidence.book_year))
}

#[cfg(test)]
mod tests {
    use super::*;
    const NAME: &str = "Refactoring UI -- Steve Schoger, Adam Wathan -- 2018 -- d838e6e05a8f009f5fa3c0ee4fd1ecd8 -- Anna’s Archive.epub";
    #[test]
    fn rejects_long_identifier_tokens_inside_titles() {
        for value in ["A Title 1c026c9c84b8c26cfa2e8544ec01cec2", "Subtitle (928e8c08abc768441bd4965b1aacc4ea)", "Book PRODUCTXYZ12345678901234"] {
            assert!(unusable_title(value), "{value}");
        }
        for value in ["Electroencephalography Explained", "WW2", "Catch-22", "AB12345678 Explained", "An Introduction to C20", "Supercalifragilisticexpialidocious"] {
            assert!(!unusable_title(value), "{value}");
        }
    }

    #[test]
    fn ebook_production_labels_are_not_titles() {
        for title in ["ebook", " EBOOK ", "OMB_ebook", "OMB-EBOOK", "OMB.ebook"] {
            assert!(unusable_title(title), "{title}");
        }
        for title in ["Ebook Publishing", "The Ebook Revolution", "My-ebook Guide", "Notebook"] {
            assert!(!unusable_title(title), "{title}");
        }
    }

    #[test]
    fn rejects_only_standalone_kindle_identifiers() {
        for value in ["B005CU4TJ6 EBOK", "B07DPP4J1B EBOK", "B003EGGIBC EBOK", "B005JC0R84 EBOK", "B005CU4TJ6", " b005cu4tj6_ebok ", "B005CU4TJ6-EBOK"] {
            assert!(unusable_title(value), "{value}");
        }
        for value in ["Understanding B005CU4TJ6", "B005CU4TJ6: A Guide", "B005CU4TJ6 EBOK Explained", "B0", "B005CU4TJ", "B005CU4TJ66", "WW2", "Catch-22", "EBOK"] {
            assert!(!unusable_title(value), "{value}");
        }
    }
    #[test]
    fn rejects_standard_labels_documents_and_web_addresses() {
        for value in [
            "Refactoring.indd",
            "Book.QXD",
            "Book.p65",
            "0465002566-text.qxd:0465002566 text",
            "Cover",
            " FRONT MATTER ",
            "title-page",
            "Table of Contents",
            "PC.pdf",
            "Microsoft Word - Book.docx",
            "Book.doc",
            "www.eBooks-IT.org",
            "example.org",
            "example.com/books",
            "https://example.xyz/book",
        ] {
            assert!(unusable_title(value), "{value}");
        }
        for value in ["Cover Stories", "Microsoft Word for Beginners", "Book.qxdnotes", "The .indd Format Explained", "J. R. R. Tolkien", "Version 2.0", "The dot.com Boom", "A Book (z-lib.org)"] {
            assert!(!unusable_title(value), "{value}");
        }
    }

    #[test]
    fn numeric_title_wrappers_are_not_book_titles() {
        for title in ["0192805045.pdf", "9780192802156", "ISBN: 978-0-19-280215-6", "ISBN-13: 9780192802156", "9780192802157.pdf", "abcdef123456abcdef123456.pdf", "Fossils (ISBN 0192805045)"] {
            assert!(unusable_title(title), "{title}");
        }
        for title in ["1984", "2001: A Space Odyssey"] {
            assert!(!unusable_title(title), "{title}");
        }
    }
    #[test]
    fn refactoring_identity_and_independent_fields() {
        let expected = ("Refactoring UI".into(), vec!["Steve Schoger".into(), "Adam Wathan".into()], Some(2018));
        assert_eq!(with_filename("387b47f0673441e9cb40e35363b49627".into(), vec!["Unknown".into()], None, NAME), expected);
        assert_eq!(with_filename("Actual title".into(), vec!["Actual Author".into()], Some(2020), NAME), ("Actual title".into(), vec!["Actual Author".into()], Some(2020)));
        assert_eq!(with_filename("Refactoring UI".into(), vec!["Unknown".into()], None, NAME), expected);
    }
    #[test]
    fn rejects_storage_hash_and_reuses_pdf_normalization() {
        assert_eq!(with_filename("Unknown".into(), vec![], None, "d838e6e05a8f009f5fa3c0ee4fd1ecd8.epub"), ("Unknown".into(), vec![], None));
        assert_eq!(with_filename("Untitled".into(), vec![], None, "A Concise History of France by Roger Price (z-lib.org).pdf"), ("A Concise History of France".into(), vec!["Roger Price".into()], None));
        assert!(!unusable_identity("1984"));
    }
}
