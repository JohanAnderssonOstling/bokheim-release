/// Normalizes an ISBN-10 or ISBN-13 to a compact, checksum-verified form.
pub fn canonical_isbn(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.strip_prefix("urn:isbn:").or_else(|| value.strip_prefix("URN:ISBN:")).unwrap_or(value);
    let compact = value.chars().filter(|character| !matches!(character, '-' | ' ')).map(|character| character.to_ascii_uppercase()).collect::<String>();
    let bytes = compact.as_bytes();
    let valid = match bytes.len() {
        10 => {
            bytes[..9].iter().all(u8::is_ascii_digit)
                && (bytes[9].is_ascii_digit() || bytes[9] == b'X')
                && bytes.iter().enumerate().map(|(index, byte)| u32::from(if *byte == b'X' { 10 } else { *byte - b'0' }) * (10 - index as u32)).sum::<u32>() % 11 == 0
        }
        13 => bytes.iter().all(u8::is_ascii_digit) && bytes.iter().enumerate().map(|(index, byte)| u32::from(*byte - b'0') * if index % 2 == 0 { 1 } else { 3 }).sum::<u32>() % 10 == 0,
        _ => false,
    };
    valid.then_some(compact)
}

/// Normalizes a DOI, stripping resolver URL prefixes, to its bare value.
pub fn canonical_doi(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    let value = lower
        .strip_prefix("https://doi.org/")
        .or_else(|| lower.strip_prefix("http://doi.org/"))
        .or_else(|| lower.strip_prefix("https://dx.doi.org/"))
        .or_else(|| lower.strip_prefix("http://dx.doi.org/"))
        .or_else(|| lower.strip_prefix("doi:"))
        .unwrap_or(&lower);
    let valid = value.starts_with("10.")
        && value.split_once('/').is_some_and(|(registrant, suffix)| (4..=9).contains(&registrant[3..].len()) && registrant[3..].bytes().all(|byte| byte.is_ascii_digit()) && !suffix.is_empty())
        && !value.bytes().any(|byte| byte.is_ascii_whitespace());
    valid.then_some(value.to_owned())
}

/// Normalizes a UUID, accepting an optional `urn:uuid:` prefix.
pub fn canonical_uuid(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    let value = lower.strip_prefix("urn:uuid:").unwrap_or(&lower);
    uuid::Uuid::parse_str(value).ok().map(|value| value.hyphenated().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_identifiers_are_validated_and_normalized() {
        assert_eq!(canonical_isbn("urn:isbn:978-1-23456-789-7"), Some("9781234567897".to_owned()));
        assert_eq!(canonical_isbn("978-1-23456-789-8"), None);
        assert_eq!(canonical_doi("HTTPS://DOI.ORG/10.1000/Example"), Some("10.1000/example".to_owned()));
        assert_eq!(canonical_uuid("URN:UUID:00112233-4455-6677-8899-AABBCCDDEEFF"), Some("00112233-4455-6677-8899-aabbccddeeff".to_owned()));
    }
}

/// Extract complete, checksum-valid ISBNs from metadata text, retaining their
/// original ISBN-10/13 form. Never take a substring of an alphanumeric token.
pub fn from_text(text: &str) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    let separator = |c: char| c.is_whitespace() || matches!(c, '-' | '‐' | '‑' | '–');
    let mut found = std::collections::BTreeSet::new();
    let mut consumed_until = 0;
    for start in 0..chars.len() {
        let isbn_label = start >= 4 && chars[start - 4..start].iter().collect::<String>().eq_ignore_ascii_case("isbn") && (start == 4 || !chars[start - 5].is_alphanumeric());
        if start < consumed_until || !chars[start].is_ascii_digit() || (start > 0 && chars[start - 1].is_alphanumeric() && !isbn_label) {
            continue;
        }
        // A hyphen inside a longer numeric token is not a new ISBN boundary.
        if start > 1 && separator(chars[start - 1]) && !chars[start - 1].is_whitespace() && chars[start - 2].is_ascii_digit() {
            continue;
        }
        let mut compact = String::new();
        let mut candidate = None;
        for end in start..chars.len() {
            let c = chars[end];
            if c.is_ascii_digit() || matches!(c, 'x' | 'X') {
                compact.push(c);
            } else if separator(c) {
                continue;
            } else {
                break;
            }
            if compact.len() > 13 {
                break;
            }
            let boundary = chars.get(end + 1).is_none_or(|c| !c.is_alphanumeric() && !matches!(c, '-' | '‐' | '‑' | '–'));
            if boundary && (compact.len() == 10 || (compact.len() == 13 && (compact.starts_with("978") || compact.starts_with("979")))) {
                if let Some(isbn) = canonical_isbn(&compact) {
                    candidate = Some((isbn, end + 1));
                }
            }
        }
        if let Some((isbn, end)) = candidate {
            found.insert(isbn);
            consumed_until = end;
        }
    }
    found.into_iter().collect()
}

/// Accept a complete ISBN candidate from extracted book content.
///
/// Content readers may normalize layout whitespace and Unicode dash variants
/// before calling this function. It deliberately rejects placeholder values
/// and ISBN-13 values outside the ISBN prefixes.
pub fn from_content_candidate(value: &str) -> Option<String> {
    let raw: String = value.chars().filter(|c| !c.is_whitespace()).map(|c| if matches!(c, '\u{00ad}' | '–' | '‐' | '‑') { '-' } else { c }).collect();
    canonical_isbn(&raw).filter(|isbn| (isbn.len() == 10 || isbn.starts_with("978") || isbn.starts_with("979")) && isbn != "0000000000" && isbn != "0000000000000")
}

#[cfg(test)]
mod embedded_isbn_tests {
    use super::*;
    #[test]
    fn embedded_isbns_accept_titles_labels_and_common_separators() {
        for text in [
            "9780131103627 The C Programming Language",
            "The C Programming Language (ISBN: 978-0-13-110362-7)",
            "ISBN-13: 978 0 13 110362 7",
            "ISBN: 978‑0‑13‑110362‑7",
            "urn:isbn:9780131103627",
            "ISBN9780131103627",
            "9780131103627, 9780131103627",
        ] {
            assert_eq!(from_text(text), vec!["9780131103627"], "{text}");
        }
        assert_eq!(from_text("ISBN-10: 0-8044-2957-X"), vec!["080442957X"]);
        assert_eq!(from_text("9780131103627 / 9780306406157"), vec!["9780131103627", "9780306406157"]);
    }
    #[test]
    fn embedded_isbns_reject_invalid_checksums_and_numeric_substrings() {
        for text in ["9780131103628", "0000000000000", "1239780131103627", "9780131103627123", "id9780131103627foo", "123-9780131103627", "QA76.73.R87", "History"] {
            assert!(from_text(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn content_candidates_apply_content_acceptance_rules() {
        assert_eq!(from_content_candidate("978‑0‑13‑110362‑7"), Some("9780131103627".into()));
        for value in ["0000000000", "0000000000000", "9770131103627", "9780131103628"] {
            assert_eq!(from_content_candidate(value), None, "{value}");
        }
    }
    #[test]
    fn promotion_preserves_subjects_and_explicit_identifiers() {
        use crate::*;
        let subject = BookSubject::new(None, "ISBN: 9780131103627 The C Programming Language", "dc:subject", None, None).unwrap();
        let mut metadata = BookMetadata { subjects: vec![subject.clone()], ..BookMetadata::default() };
        metadata.infer_embedded_subject_isbns().unwrap();
        metadata.infer_embedded_subject_isbns().unwrap();
        assert_eq!(metadata.identifiers.len(), 1);
        assert_eq!(metadata.subjects, vec![subject]);
        metadata.identifiers = vec![Identifier::new("9780131103627", Scheme::Isbn, Scope::Edition).unwrap()];
        metadata.infer_embedded_subject_isbns().unwrap();
        assert_eq!(metadata.identifiers.len(), 1);
        assert_eq!(metadata.identifiers[0], Identifier::new("9780131103627", Scheme::Isbn, Scope::Edition).unwrap());
    }
}
