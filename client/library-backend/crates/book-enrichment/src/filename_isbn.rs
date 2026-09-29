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
        let mut compact = String::new();
        let mut end = start;
        for &(offset, next) in &chars[index..] {
            if !allowed(next) {
                break;
            }
            if next.is_ascii_digit() || matches!(next, 'x' | 'X') {
                compact.push(next);
                end = offset + next.len_utf8();
            } else {
                compact.push(next);
            }
            let digits = compact.chars().filter(|c| c.is_ascii_digit() || matches!(c, 'x' | 'X')).count();
            if digits > 13 {
                break;
            }
            let boundary = chars.get(index + compact.chars().count()).is_none_or(|(_, following)| !following.is_alphanumeric() && !matches!(following, '-' | '\u{00ad}' | '\u{2010}'..='\u{2015}'));
            if boundary {
                if let Some(isbn) = from_content_candidate(&compact) {
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
    fn accepts_supported_filename_forms() {
        for value in ["9780131103627.pdf", "ISBN 978 0 13 110362 7.pdf", "ISBN 978‑0‑13‑110362‑7.opf"] {
            assert_eq!(from_filename(value), ["9780131103627"], "{value}");
        }
    }

    #[test]
    fn rejects_invalid_and_ambiguous_values() {
        for value in ["0000000000.pdf", "9770131103627.pdf", "9780131103628.pdf", "1239780131103627"] {
            assert!(from_filename(value).is_empty(), "{value}");
        }
        assert_eq!(from_filename_single("9780131103627 and 9780306406157.pdf"), None);
    }

    #[test]
    fn retains_deduplicated_utf8_byte_ranges() {
        let entries = matches("Títel 978‑0‑13‑110362‑7 and 978‑0‑13‑110362‑7");
        assert_eq!(entries.len(), 2);
        assert_eq!(&"Títel 978‑0‑13‑110362‑7 and 978‑0‑13‑110362‑7"[entries[0].range.clone()], "978‑0‑13‑110362‑7");
    }
}
