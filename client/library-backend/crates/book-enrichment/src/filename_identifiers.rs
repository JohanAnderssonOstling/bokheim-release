//! Identifier evidence in source filenames, independent of title usability.
use book_model::{canonical_doi, BookMetadata, Identifier, Scheme, Scope};

pub fn clean(stem: &str) -> (String, Vec<String>) {
    let mut spans = Vec::new();
    let mut dois = Vec::new();
    for (start, _) in stem.match_indices("10.") {
        if start > 0 && stem.as_bytes()[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let digits = stem[start + 3..].bytes().take_while(u8::is_ascii_digit).count();
        let separator = start + 3 + digits;
        if !(4..=9).contains(&digits) || !matches!(stem.as_bytes().get(separator), Some(b'_' | b'/')) {
            continue;
        }
        let end = stem[separator + 1..].char_indices().find(|(_, c)| c.is_whitespace() || matches!(c, ']' | ')')).map(|(i, _)| separator + 1 + i).unwrap_or(stem.len());
        let candidate = format!("{}/{}", &stem[start..separator], &stem[separator + 1..end]);
        if let Some(doi) = canonical_doi(&candidate) {
            spans.push((start, end));
            dois.push(doi);
        }
    }
    spans.extend(super::filename_isbn::matches(stem).into_iter().map(|entry| (entry.range.start, entry.range.end)));
    if spans.is_empty() {
        return (stem.to_owned(), dois);
    }
    spans.sort_unstable();
    let mut clean = String::new();
    let mut cursor = 0;
    for (start, end) in spans {
        if start > cursor {
            clean.push_str(&stem[cursor..start]);
            clean.push(' ');
        }
        cursor = cursor.max(end);
    }
    clean.push_str(&stem[cursor..]);
    let clean = clean.split_whitespace().filter(|word| !matches!(word.trim_matches(['[', ']', '(', ')', ':']).to_ascii_lowercase().as_str(), "isbn" | "isbn-10" | "isbn-13" | "isbn10" | "isbn13" | "doi")).collect::<Vec<_>>().join(" ");
    (clean, dois)
}

pub fn record(stem: &str, book: &mut BookMetadata) -> Result<(), String> {
    super::filename_isbn::record(stem, book).map_err(|e| e.to_string())?;
    for doi in clean(stem).1 {
        if !book.identifiers.iter().any(|id| id.scheme() == &Scheme::Doi && id.canonical_value().as_deref() == Some(&doi)) {
            book.identifiers.push(Identifier::new(doi, Scheme::Doi, Scope::Edition).map_err(|e| e.to_string())?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_non_identifier_text_and_supports_doi_suffixes() {
        for value in ["Understanding ISBN", "1984", "A Title 9781848000705", "1239781848000704123", "Version 10.123_notes"] {
            assert_eq!(clean(value).0, value);
        }
        let (title, dois) = clean("Title [10.1007_978-1-84800-070-4]");
        assert!(!title.contains("978"));
        assert_eq!(dois, ["10.1007/978-1-84800-070-4"]);
        assert_eq!(clean("Title [10.1234/example_part]").1, ["10.1234/example_part"]);
    }
}
