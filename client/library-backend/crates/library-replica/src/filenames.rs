//! Local-only names for downloaded books.
//!
//! Scans preserve names chosen by the user. Downloads use this deterministic,
//! collision-aware ladder and never synchronize a filename register.

use book_model::BookFormat;
use book_model::BookRecord;
use sync_common::{ContentHash, FileName};

pub(crate) const MAX_FILE_NAME_BYTES: usize = 240;
const COMPACT_HASH_HEX_LEN: usize = 12;

pub fn materialize(content_hash: ContentHash, metadata: &BookRecord, format: BookFormat, mut occupied: impl FnMut(&FileName) -> bool) -> FileName {
    let extension = format.canonical_extension();
    let compact_hash = &content_hash.as_str()[..COMPACT_HASH_HEX_LEN];
    let title = sanitize_stem(&metadata.title);
    let title = (!title.is_empty()).then_some(title).unwrap_or_else(|| compact_hash.to_owned());
    let author = metadata.authors().next().map(|author| sanitize_stem(author.name())).filter(|author| !author.is_empty());
    let year = metadata.book.dates.iter().find_map(|date| date.value().get(..4).filter(|year| year.bytes().all(|byte| byte.is_ascii_digit()))).map(str::to_owned);

    let mut candidates = vec![title.clone()];
    let mut detailed = title.clone();
    if let Some(author) = author.as_deref() {
        detailed = format!("{title} - {author}");
        candidates.push(detailed.clone());
        if let Some(year) = year.as_deref() {
            detailed = format!("{detailed} ({year})");
            candidates.push(detailed.clone());
        }
    }
    candidates.push(format!("{detailed} [{compact_hash}]"));
    candidates.push(format!("{detailed} [{content_hash}]"));

    let final_rung = candidates.len() - 1;
    for (index, stem) in candidates.into_iter().enumerate() {
        let stem = if index == final_rung {
            cap_with_suffix(&stem, &format!(" [{content_hash}]"), &extension)
        } else if stem.ends_with(&format!(" [{compact_hash}]")) {
            cap_with_suffix(&stem, &format!(" [{compact_hash}]"), &extension)
        } else {
            cap(&stem, &extension)
        };
        let candidate = format!("{stem}.{extension}");
        if !occupied(&candidate) {
            return candidate;
        }
    }
    unreachable!("the full content hash makes the final filename rung unique")
}

fn sanitize_stem(value: &str) -> String {
    let mut output = String::new();
    let mut space = false;
    for character in value.chars() {
        if character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || character.is_whitespace() {
            space = !output.is_empty();
        } else {
            if space {
                output.push(' ');
                space = false;
            }
            output.push(character);
        }
    }
    let output = output.trim().trim_end_matches('.').trim().to_owned();
    if is_reserved(&output) { String::new() } else { output }
}

pub(crate) fn is_reserved(value: &str) -> bool {
    // Windows reserves these device stems even when followed by an extension.
    let stem = value.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT")).is_some_and(|number| matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
}

pub fn validate_component(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || matches!(value, "." | "..") {
        return Err("name must not be empty, '.' or '..'".to_owned());
    }
    if value.chars().any(|character| character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')) {
        return Err("name contains a character that is not portable across supported devices".to_owned());
    }
    if value.ends_with([' ', '.']) || is_reserved(value) {
        return Err("name is reserved or has an unsupported trailing character".to_owned());
    }
    if value.len() > MAX_FILE_NAME_BYTES {
        return Err(format!("name must not exceed {MAX_FILE_NAME_BYTES} UTF-8 bytes"));
    }
    Ok(value.to_owned())
}

fn cap(stem: &str, extension: &str) -> String {
    truncate_utf8(stem, MAX_FILE_NAME_BYTES.saturating_sub(extension.len() + 1))
}
fn cap_with_suffix(stem: &str, suffix: &str, extension: &str) -> String {
    let title = stem.strip_suffix(suffix).unwrap_or(stem);
    format!("{}{}", truncate_utf8(title, MAX_FILE_NAME_BYTES.saturating_sub(extension.len() + 1 + suffix.len())), suffix)
}
pub(crate) fn truncate_utf8(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use book_model::BookRecord;
    use book_model::{BookDate, BookMetadata, Contributor, MarcRelatorCode};
    fn metadata(title: &str) -> BookRecord {
        BookRecord { title: title.to_owned(), subtitle: None, contributors: vec![], description: String::new(), book: BookMetadata::default() }
    }
    fn hash(value: u64) -> ContentHash {
        ContentHash::new(&format!("{value:012x}{}", "0".repeat(64 - COMPACT_HASH_HEX_LEN)))
    }
    #[test]
    fn uses_shortest_free_sanitized_name() {
        assert_eq!(materialize(hash(7), &metadata(" Dune: "), BookFormat::Epub, |_| false).as_str(), "Dune.epub");
    }
    #[test]
    fn fallback_keeps_content_hash_when_title_is_illegal() {
        assert_eq!(materialize(hash(7), &metadata("CON"), BookFormat::Epub, |_| false).as_str(), "000000000007.epub");
    }

    #[test]
    fn collision_reaches_the_content_hash_rung() {
        let metadata = metadata("Dune");
        let first = materialize(hash(1), &metadata, BookFormat::Epub, |_| false);
        let second = materialize(hash(2), &metadata, BookFormat::Epub, |candidate| candidate == &first);
        assert_eq!(first.as_str(), "Dune.epub");
        assert_eq!(second.as_str(), "Dune [000000000002].epub");
    }

    #[test]
    fn compact_hash_collision_escalates_to_the_full_hash() {
        let first_hash = ContentHash::new(&"abcdef1234560000000000000000000000000000000000000000000000000001");
        let second_hash = ContentHash::new(&"abcdef1234560000000000000000000000000000000000000000000000000002");
        let metadata = metadata("Dune");
        let occupied = ["Dune.epub", "Dune [abcdef123456].epub"];

        let name = materialize(second_hash, &metadata, BookFormat::Epub, |candidate| occupied.contains(&candidate.as_str()));

        assert_eq!(name.as_str(), format!("Dune [{second_hash}].epub"));
        assert_ne!(first_hash, second_hash);
    }

    #[test]
    fn collisions_escalate_through_author_year_and_identity() {
        let mut metadata = metadata("Dune");
        metadata.contributors = vec![Contributor::new("Frank Herbert", MarcRelatorCode(*b"aut")).unwrap()];
        metadata.book.dates = vec![BookDate::new(None, "1965-08-01", Some("book".to_owned()), "test:book-date").unwrap()];
        let occupied = ["Dune.epub", "Dune - Frank Herbert.epub", "Dune - Frank Herbert (1965).epub"];
        assert_eq!(materialize(hash(2), &metadata, BookFormat::Epub, |candidate| occupied.contains(&candidate.as_str())).as_str(), "Dune - Frank Herbert (1965) [000000000002].epub");
    }

    /// An incumbent is never renamed: the newcomer escalates and the file that
    /// was there first keeps its rung permanently, so names cannot oscillate.
    #[test]
    fn escalation_is_not_retroactive() {
        let metadata = metadata("Dune");
        let placed = materialize(hash(1), &metadata, BookFormat::Epub, |_| false);
        assert_eq!(placed.as_str(), "Dune.epub");
        let mut occupied = vec![placed.as_str().to_owned()];
        let newcomer = materialize(hash(2), &metadata, BookFormat::Epub, |candidate| occupied.iter().any(|taken| taken == candidate.as_str()));
        assert_eq!(newcomer.as_str(), "Dune [000000000002].epub");
        occupied.push(newcomer.as_str().to_owned());
        // Re-running the whole sequence returns each book to its own name.
        assert_eq!(materialize(hash(1), &metadata, BookFormat::Epub, |candidate| candidate.as_str() != placed.as_str() && occupied.iter().any(|taken| taken == candidate.as_str())).as_str(), placed.as_str());
        assert_eq!(materialize(hash(2), &metadata, BookFormat::Epub, |candidate| candidate.as_str() != newcomer.as_str() && occupied.iter().any(|taken| taken == candidate.as_str())).as_str(), newcomer.as_str());
    }

    /// Rung 4 always terminates: `content_hash` is unique, so identical metadata can
    /// never exhaust the ladder however many books share it.
    #[test]
    fn identical_metadata_always_terminates() {
        let mut metadata = metadata("Dune");
        metadata.contributors = vec![Contributor::new("Frank Herbert", MarcRelatorCode(*b"aut")).unwrap()];
        metadata.book.dates = vec![BookDate::new(None, "1965-08-01", Some("book".to_owned()), "test:book-date").unwrap()];
        let mut occupied: Vec<String> = Vec::new();
        // Four books exhaust the three metadata rungs; every book after them can
        // only be distinguished by its identity, and that rung cannot fail.
        for id in 1..=5_u64 {
            let name = materialize(hash(id), &metadata, BookFormat::Epub, |candidate| occupied.iter().any(|taken| taken == candidate.as_str()));
            assert!(!occupied.contains(&name.as_str().to_owned()), "rung ladder produced a duplicate name: {name}");
            occupied.push(name.as_str().to_owned());
        }
        assert_eq!(occupied[0], "Dune.epub");
        assert_eq!(occupied[1], "Dune - Frank Herbert.epub");
        assert_eq!(occupied[2], "Dune - Frank Herbert (1965).epub");
        assert_eq!(occupied[3], "Dune - Frank Herbert (1965) [000000000004].epub");
        assert_eq!(occupied[4], "Dune - Frank Herbert (1965) [000000000005].epub");
    }

    /// Rungs whose components are absent are skipped rather than rendered with
    /// empty separators, so each missing-metadata case stays deterministic.
    #[test]
    fn absent_components_skip_their_rungs() {
        let untitled = materialize(hash(9), &metadata("   ..  "), BookFormat::Pdf, |_| false);
        assert_eq!(untitled.as_str(), "000000000009.pdf");

        // No author: rung 2 and 3 cannot be rendered, so a collision goes
        // straight to the identity rung.
        let no_author = materialize(hash(9), &metadata("Dune"), BookFormat::Epub, |candidate| candidate.as_str() == "Dune.epub");
        assert_eq!(no_author.as_str(), "Dune [000000000009].epub");

        // Author but no year: rung 3 is skipped, rung 2 is still available.
        let mut author_only = metadata("Dune");
        author_only.contributors = vec![Contributor::new("Frank Herbert", MarcRelatorCode(*b"aut")).unwrap()];
        assert_eq!(materialize(hash(9), &author_only, BookFormat::Epub, |candidate| candidate.as_str() == "Dune.epub").as_str(), "Dune - Frank Herbert.epub");
        let occupied = ["Dune.epub", "Dune - Frank Herbert.epub"];
        assert_eq!(materialize(hash(9), &author_only, BookFormat::Epub, |candidate| occupied.contains(&candidate.as_str())).as_str(), "Dune - Frank Herbert [000000000009].epub");
    }

    /// Every rendered stem must be legal on FAT/exFAT/NTFS as well as ext4.
    #[test]
    fn sanitization_produces_portable_names() {
        for (title, expected) in [
            ("A: B / C ? D | E \" F * G", "A B C D E F G.epub"),
            ("Tab\there\nand\rthere", "Tab here and there.epub"),
            ("  leading and trailing  ", "leading and trailing.epub"),
            ("Trailing dot...", "Trailing dot.epub"),
            ("CON", "000000000005.epub"),
            ("com1", "000000000005.epub"),
            ("LPT9", "000000000005.epub"),
        ] {
            let name = materialize(hash(5), &metadata(title), BookFormat::Epub, |_| false);
            assert_eq!(name.as_str(), expected, "title {title:?}");
            assert!(!name.as_str().chars().any(|character| character.is_control() || matches!(character, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')), "illegal character survived in {name}");
            let stem = name.as_str().rsplit_once('.').expect("materialized names carry an extension").0;
            assert!(!stem.ends_with(' ') && !stem.ends_with('.') && !stem.starts_with(' '), "windows rejects {name}");
        }
    }

    #[test]
    fn truncation_preserves_extension_and_identity_suffix() {
        let metadata = metadata(&"é".repeat(300));
        let name = materialize(hash(42), &metadata, BookFormat::Epub, |candidate| !candidate.as_str().contains("[00000000002a]"));
        assert!(name.as_str().ends_with("[00000000002a].epub"));
        assert!(name.as_str().len() <= MAX_FILE_NAME_BYTES);
    }
}
