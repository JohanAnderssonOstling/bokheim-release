//! ISBN extraction from filenames and title-like text.
use book_model::{from_content_candidate, BookMetadata, IdentifierError, Scope};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilenameIsbn {
    pub(crate) isbn: String,
    pub(crate) range: std::ops::Range<usize>,
}

fn separator(character: char) -> bool {
    character.is_whitespace() || matches!(character, '-' | '\u{00ad}' | '\u{2010}'..='\u{2015}')
}

fn allowed(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, 'x' | 'X') || separator(character)
}

/// Extract all distinct valid ISBNs from a filename-like value.
///
/// Matches retain their original UTF-8 byte ranges so callers can remove the
/// exact source spans without reimplementing the tokenizer.
pub(crate) fn matches(value: &str) -> Vec<FilenameIsbn> {
    let chars = value.char_indices().collect::<Vec<_>>();
    let mut matches = Vec::new();
    for (index, &(start, character)) in chars.iter().enumerate() {
        if !character.is_ascii_digit() || (index > 0 && chars[index - 1].1.is_alphanumeric()) {
            continue;
        }
        // Do not begin in the middle of a hyphenated ISBN run.
        if index > 1 && separator(chars[index - 1].1) && !chars[index - 1].1.is_whitespace() && chars[index - 2].1.is_ascii_digit() {
            continue;
        }
        let mut digits = 0;
        let mut end = start;
        for (position, &(offset, next)) in chars[index..].iter().enumerate() {
            if !allowed(next) {
                break;
            }
            if next.is_ascii_digit() || matches!(next, 'x' | 'X') {
                digits += 1;
                end = offset + next.len_utf8();
            }
            if digits > 13 {
                break;
            }
            let boundary = chars.get(index + position + 1).is_none_or(|(_, following)| !following.is_alphanumeric() && !matches!(following, '-' | '\u{00ad}' | '\u{2010}'..='\u{2015}'));
            if boundary {
                if let Some(isbn) = from_content_candidate(&value[start..offset + next.len_utf8()]) {
                    matches.push(FilenameIsbn { isbn, range: start..end });
                }
            }
        }
    }
    matches
}

pub fn from_filename(value: &str) -> Vec<String> {
    matches(value).into_iter().map(|entry| entry.isbn).collect::<BTreeSet<_>>().into_iter().collect()
}

pub fn from_filename_single(value: &str) -> Option<String> {
    let values = from_filename(value);
    (values.len() == 1).then(|| values.into_iter().next()).flatten()
}

pub fn record(value: &str, book: &mut BookMetadata) -> Result<(), IdentifierError> {
    for isbn in from_filename(value) {
        book_model::push_isbn(&mut book.identifiers, isbn, Scope::Edition)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_candidates_keep_exact_source_byte_ranges() {
        for (raw, canonical) in [
            ("978-0-306-40615-7", "9780306406157"),
            ("978‐0‐306‐40615‐7", "9780306406157"),
            ("978\u{00ad}0\u{00ad}306\u{00ad}40615\u{00ad}7", "9780306406157"),
            ("978\u{00a0}0\u{2003}306\t40615 7", "9780306406157"),
            ("0-8044-2957-x", "080442957X"),
        ] {
            let prefix = "Ångström 📖 [";
            let text = format!("{prefix}{raw}   ] Épilogue");
            let found = matches(&text);
            assert!(!found.is_empty(), "{raw:?}");
            for entry in found {
                assert_eq!(entry.isbn, canonical);
                assert_eq!(entry.range, prefix.len()..prefix.len() + raw.len());
                assert_eq!(&text[entry.range], raw);
            }
        }
    }

    #[test]
    fn extraction_rejects_invalid_checksums_placeholders_and_partial_tokens() {
        for text in ["A9780306406157", "9780306406157é", "19780306406157", "1-9780306406157", "9780306406157-9", "0000000000", "0000000000000", "9780306406158"] {
            assert!(from_filename(text).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn repeated_isbns_are_unique_and_single_extraction_rejects_ambiguity() {
        let repeated = "9780306406157 [978-0-306-40615-7]";
        assert_eq!(from_filename(repeated), ["9780306406157"]);
        assert_eq!(from_filename_single(repeated).as_deref(), Some("9780306406157"));
        let multiple = format!("{repeated} [0-8044-2957-X]");
        assert_eq!(from_filename(&multiple), ["080442957X", "9780306406157"]);
        assert_eq!(from_filename_single(&multiple), None);
        assert_eq!(from_filename_single("No ISBN"), None);
    }
}
