//! Conservative book-identity evidence derived from filename-like titles.
//!
//! These helpers only prepare lookup evidence. They do not mutate embedded
//! book metadata or claim that a filename identifies an edition.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditionIdentityEvidence {
    pub title: String,
    pub author: Option<String>,
    pub book_year: Option<i32>,
}

/// Extracts conservative title, author, and year evidence from a title that
/// may have originated from a local filename.
pub fn edition_identity_evidence(title: &str) -> EditionIdentityEvidence {
    parse_identity_evidence(title, false, None)
}

/// Filenames use an explicit ` by ` delimiter for trailing author credits.
pub fn filename_identity_evidence(title: &str) -> EditionIdentityEvidence {
    parse_identity_evidence(title, true, None)
}

/// Recover multiple subtitle segments only when the first segment matches a known title.
pub fn filename_identity_evidence_with_title(title: &str, anchor: &str) -> EditionIdentityEvidence {
    parse_identity_evidence(title, true, Some(anchor))
}

/// Recognize a second subtitle that repeats the first subtitle's whole-word prefix.
fn overlapping_subtitles(value: &str) -> Option<String> {
    let parts = value.split('_').filter(|part| !part.is_empty()).map(str::trim).collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    let first = crate::matching::bibliographic_match_key(parts[1]);
    let later = title_without_parenthetical_segments(parts[2], true);
    // A repeated subtitle prefix can have a trailing publisher suffix.
    let later = crate::matching::bibliographic_match_key(later.split(['-', '–', '—']).next()?.trim());
    if later.split_whitespace().count() < 2 {
        return None;
    }
    if first != later && !first.strip_prefix(&later).is_some_and(|rest| rest.starts_with(' ')) {
        return None;
    }
    Some(format!("{}: {}", parts[0], parts[1]))
}

fn parse_identity_evidence(title: &str, filename: bool, anchor: Option<&str>) -> EditionIdentityEvidence {
    // Only underscore groups followed by whitespace can mark a subtitle.
    // Other groups are filename punctuation, not additional subtitle candidates.
    let title = title.trim_end();
    let title = title.rsplit_once(" -- ").filter(|(_, tag)| tag.replace('’', "'").eq_ignore_ascii_case("Anna's Archive")).map_or(title, |(title, _)| title);
    let production = clean_production_title(title).replace(';', ": ");
    let trimmed = production.trim();
    let mut stripped = title_without_parenthetical_segments(trimmed, true);
    // Remove only separators attached to metadata groups that cleanup consumed.
    let groups = delimited_segments(trimmed);
    if groups.first().is_some_and(|(start, end)| *start == 0 && title_without_parenthetical_segments(&trimmed[..*end], true).is_empty()) {
        stripped = stripped.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—')).to_owned();
    }
    if groups.last().is_some_and(|(start, end)| *end == trimmed.len() && title_without_parenthetical_segments(&trimmed[*start..], true).is_empty()) {
        stripped = stripped.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—')).to_owned();
    }
    let cleaned = stripped;

    let groups = cleaned
        .match_indices('_')
        .filter(|(index, _)| *index == 0 || cleaned.as_bytes()[index - 1] != b'_')
        .map(|(start, _)| {
            let end = start + cleaned[start..].bytes().take_while(|byte| *byte == b'_').count();
            (start, end)
        })
        .collect::<Vec<_>>();
    let candidates = groups.iter().copied().filter(|(_, end)| cleaned[*end..].starts_with(char::is_whitespace)).collect::<Vec<_>>();
    let subtitle_normalized = if let Some(title) = overlapping_subtitles(&cleaned) {
        title
    } else if let [(start, end)] = candidates.as_slice() {
        format!("{}: {}", cleaned[..*start].replace('_', " "), cleaned[*end..].trim_start().replace('_', " "))
    } else if candidates.len() > 1 && anchor.is_some_and(|anchor| !anchor.trim().is_empty() && crate::matching::bibliographic_match_key(&cleaned[..candidates[0].0]) == crate::matching::bibliographic_match_key(anchor)) {
        let mut parts = Vec::new();
        let mut previous = 0;
        for (start, end) in &candidates {
            parts.push(cleaned[previous..*start].replace('_', " ").trim().to_owned());
            previous = *end;
        }
        parts.push(cleaned[previous..].replace('_', " ").trim().to_owned());
        if parts.iter().all(|part| !part.is_empty()) {
            parts.join(": ")
        } else {
            cleaned.replace('_', " ")
        }
    } else if groups.len() > 1 {
        cleaned.replace('_', " ")
    } else {
        cleaned
    };
    let trimmed = subtitle_normalized.trim();
    // Structured archive filenames explicitly delimit title, credits and year.
    let fields = trimmed.split(" -- ").map(str::trim).collect::<Vec<_>>();
    if fields.len() >= 3 && !fields[0].is_empty() && !fields[1].is_empty() {
        if let Some(year) = fields[2].parse::<i32>().ok().filter(|year| (1450..=2100).contains(year)) {
            return EditionIdentityEvidence { title: fields[0].to_owned(), author: Some(fields[1].to_owned()), book_year: Some(year) };
        }
    }
    let segments = parenthetical_segments(title);
    let mut inferred_author = None;
    let mut inferred_year = None;
    for segment in &segments {
        inferred_year = inferred_year.or_else(|| plausible_book_year(segment));
        if inferred_author.is_some() {
            continue;
        }
        let lower = segment.to_lowercase();
        let (author, explicit) = if lower.starts_with("by-") || lower.starts_with("by_") || lower.starts_with("by ") { (&segment[3..], true) } else { (segment.as_str(), false) };
        let author = author.replace(['-', '_'], " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if !author.is_empty() && (explicit || looks_like_person_name(&author)) {
            inferred_author = Some(author);
        }
    }
    let stripped = trimmed;
    let tokens = stripped.split_whitespace().collect::<Vec<_>>();
    let distribution_tag = |token: &str| {
        let key = token.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
        matches!(key.as_str(), "itebooks" | "ibooker" | "zliborg" | "zlibrary" | "libgenli")
    };
    let clean_tokens = tokens
        .iter()
        .enumerate()
        .filter_map(|(index, token)| {
            let metadata_separator = matches!(*token, "-" | "–" | "—") && (tokens.get(index + 1).is_some_and(|next| distribution_tag(next)) || index.checked_sub(1).is_some_and(|previous| distribution_tag(tokens[previous])));
            (!distribution_tag(token) && !metadata_separator && !token.chars().all(|c| c == '_')).then_some(*token)
        })
        .collect::<Vec<_>>();
    let cleaned = clean_tokens.join(" ").trim().to_owned();
    let mut title = if cleaned.is_empty() { stripped.to_owned() } else { cleaned };
    if let Some(by) = rfind_ascii_case_insensitive(&title, " by ") {
        let inline_author = title[by + 4..].trim();
        if by > 0 && !inline_author.is_empty() && (filename || looks_like_person_name(inline_author)) {
            inferred_author.get_or_insert_with(|| inline_author.to_owned());
            title = title[..by].trim_matches(|character: char| character.is_whitespace() || matches!(character, '-' | '_' | '–' | '—')).to_owned();
        }
    }
    if let Some((base, suffix)) = title.rsplit_once('-') {
        let suffix = suffix.trim();
        if suffix.len() >= 6 && suffix.bytes().all(|b| b.is_ascii_digit()) && base.matches('-').count() >= 2 && !base.split('-').any(|word| matches!(word.to_ascii_lowercase().as_str(), "volume" | "vol" | "part" | "book" | "edition")) {
            title = base.to_owned();
        }
    }
    // A whole title written as hyphen-delimited words is filename formatting.
    // Require several words; preserve ordinary compounds and numeric ranges.
    let parts = title.split('-').map(str::trim).collect::<Vec<_>>();
    if parts.len() >= 4 && parts.iter().all(|part| !part.is_empty() && part.chars().all(|c| c.is_alphabetic() || matches!(c, '\'' | '’'))) {
        title = parts.join(" ");
    }
    EditionIdentityEvidence { title, author: inferred_author, book_year: inferred_year }
}

/// Normalize unmistakable release naming while preserving initials and decimals.
fn clean_production_title(title: &str) -> String {
    let title = title.trim();
    let lower = title.to_ascii_lowercase();
    let title = [".ebook-een", " ebook-een", ".een", " een"].iter().find_map(|suffix| lower.strip_suffix(suffix).map(|base| &title[..base.len()])).unwrap_or(title);
    let mut words = Vec::new();
    for word in title.split_whitespace() {
        let parts = word.split('.').collect::<Vec<_>>();
        let long_words = parts.iter().filter(|part| part.chars().count() > 2 && part.chars().all(char::is_alphabetic)).count();
        if parts.len() >= 4 && long_words >= 3 {
            let mut normalized = String::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    let previous = parts[i - 1];
                    let letters = previous.chars().all(char::is_alphabetic) && part.chars().all(char::is_alphabetic);
                    let short = previous.chars().count() <= 2 && part.chars().count() <= 2;
                    normalized.push(if !previous.is_empty() && !part.is_empty() && letters && !short { ' ' } else { '.' });
                }
                normalized.push_str(part);
            }
            words.push(normalized);
        } else {
            words.push(word.to_owned());
        }
    }
    let mut cleaned = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if words[i].bytes().all(|b| b.is_ascii_digit()) && words.get(i + 1).is_some_and(|word| word.eq_ignore_ascii_case("dpi")) {
            i += 2;
        } else {
            cleaned.push(words[i].as_str());
            i += 1;
        }
    }
    cleaned.join(" ")
}

/// Complete groups can mix parentheses and brackets. Unclosed groups remain
/// intact so cleanup cannot swallow the remaining title.
fn delimited_segments(value: &str) -> Vec<(usize, usize)> {
    let mut stack = Vec::new();
    let mut start = 0;
    let mut spans = Vec::new();
    for (index, character) in value.char_indices() {
        match character {
            '(' | '[' => {
                if stack.is_empty() {
                    start = index;
                }
                stack.push(character);
            }
            ')' | ']' if !stack.is_empty() => {
                stack.pop();
                if stack.is_empty() {
                    spans.push((start, index + 1));
                }
            }
            _ => {}
        }
    }
    spans
}

/// Remove complete leading bracket/parenthesis groups before filename credits.
pub fn without_leading_bracket_segments(title: &str) -> &str {
    let mut rest = title.trim_start();
    while let Some((0, end)) = delimited_segments(rest).first().copied() {
        rest = rest[end..].trim_start();
    }
    rest
}

/// Remove leading bracketed noise using the same rules as title cleanup.
/// Preserve edition/volume notes and unclosed groups.
pub fn without_leading_bracket_noise(title: &str) -> &str {
    let mut rest = title.trim_start();
    loop {
        let Some((0, end)) = delimited_segments(rest).first().copied() else {
            return rest;
        };
        if !title_without_parenthetical_segments(&rest[..end], true).is_empty() {
            return rest;
        }
        rest = rest[end..].trim_start();
    }
}

fn title_without_parenthetical_segments(title: &str, keep_edition_notes: bool) -> String {
    let mut outside = String::new();
    let mut previous = 0;
    for (start, end) in delimited_segments(title) {
        outside.push_str(&title[previous..start]);
        let inside = &title[start + 1..end - 1];
        let key = crate::matching::bibliographic_match_key(inside);
        if key.split_whitespace().any(|word| crate::matching::is_title_qualifier_word(word) || (keep_edition_notes && matches!(word, "edition" | "ed" | "revised" | "revision" | "unabridged" | "translation" | "translated"))) {
            outside.push(' ');
            outside.push_str(&title[start..end]);
        }
        previous = end;
    }
    outside.push_str(&title[previous..]);
    outside.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rfind_ascii_case_insensitive(value: &str, needle: &str) -> Option<usize> {
    value.as_bytes().windows(needle.len()).rposition(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn parenthetical_segments(value: &str) -> Vec<String> {
    delimited_segments(value).into_iter().map(|(start, end)| value[start + 1..end - 1].trim()).filter(|value| !value.is_empty()).map(str::to_owned).collect()
}

fn plausible_book_year(value: &str) -> Option<i32> {
    value.split(|character: char| !character.is_ascii_digit()).filter(|part| part.len() == 4).filter_map(|part| part.parse::<i32>().ok()).find(|year| (1450..=2100).contains(year))
}

fn looks_like_person_name(value: &str) -> bool {
    const PARTICLES: &[&str] = &["al", "bin", "da", "de", "del", "den", "der", "di", "du", "la", "le", "van", "von"];
    const NON_NAMES: &[&str] =
        &["abridged", "answers", "solutions", "book", "edition", "guide", "histories", "illustrated", "introduction", "introductions", "manual", "novel", "revised", "series", "textbook", "translation", "unabridged", "volume"];
    let words = value.split_whitespace().collect::<Vec<_>>();
    if !(2..=5).contains(&words.len()) || words.iter().any(|word| NON_NAMES.contains(&word.to_lowercase().as_str())) {
        return false;
    }
    words.iter().all(|word| {
        let clean = word.trim_matches(|character: char| matches!(character, '.' | ',' | '\'' | '’'));
        if clean.is_empty() || !clean.chars().all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | '’')) {
            return false;
        }
        PARTICLES.contains(&clean.to_lowercase().as_str()) || clean.chars().next().is_some_and(char::is_uppercase)
    })
}

/// Recover an author prefix only after a returned candidate corroborates both
/// the name and the remaining title. This supplies verification evidence, not
/// an edition ISBN or a replacement for known local author credits.
pub fn verified_author_prefix_query(query: &crate::EditionIdentityQuery, result: &crate::EditionIdentityResult) -> Option<crate::EditionIdentityQuery> {
    use crate::matching::{author_credits, matching_author_count, NormalizedTitle};
    if query.query_id != result.query_id || result.candidates_truncated || !query.authors.is_empty() {
        return None;
    }
    for separator in [" - ", " – ", " — "] {
        let Some((prefix, rest)) = query.title.split_once(separator) else {
            continue;
        };
        let authors = author_credits(&[prefix.trim().to_owned()]);
        if authors.is_empty() || rest.trim().is_empty() {
            continue;
        }
        let cleaned = edition_identity_evidence(&rest.replace('：', ":").replace('_', " ")).title;
        let local = NormalizedTitle::new(&cleaned);
        if local.full.len() < 5 {
            continue;
        }
        let supported = result.matched.iter().chain(&result.candidates).any(|candidate| {
            let remote = NormalizedTitle::new(&candidate.full_title());
            matching_author_count(&authors, &candidate.authors) > 0
                && local.qualifiers == remote.qualifiers
                && (local.full == remote.full || (local.subtitle.is_none() && local.full == remote.main) || (remote.subtitle.is_none() && local.main == remote.full))
        });
        if supported {
            let mut verified = query.clone();
            verified.title = cleaned;
            verified.authors = authors;
            return Some(verified);
        }
    }
    None
}

/// Validate surname_title_words against returned authority evidence.
/// Only underscore-delimited filenames qualify; ordinary prose is not split.
pub fn filename_surname_title_matches(filename_title: &str, candidate_title: &str, authors: &[String]) -> bool {
    let Some((surname, rest)) = filename_title.trim().split_once('_') else {
        return false;
    };
    if surname.chars().count() < 2 || !surname.chars().all(char::is_alphabetic) {
        return false;
    }
    let title = edition_identity_evidence(&rest.replace('_', " ")).title;
    let key = crate::matching::bibliographic_match_key(&title);
    if key.is_empty() || key != crate::matching::bibliographic_match_key(candidate_title) {
        return false;
    }
    let surname = crate::matching::bibliographic_match_key(surname);
    crate::matching::author_credits(authors).iter().any(|author| {
        crate::matching::author_match_keys(author).iter().any(|key| {
            let words = key.split_whitespace().filter(|word| crate::matching::author_suffix(word).is_none()).collect::<Vec<_>>();
            words.len() >= 2 && words.last().copied() == Some(surname.as_str())
        })
    })
}

/// Parenthetical edition/language labels are secondary evidence for subjects.
/// Keep structural volume/adaptation distinctions and all non-parenthetical text.
pub fn subject_identity_title(title: &str) -> String {
    title_without_parenthetical_segments(&edition_identity_evidence(title).title, false)
}

#[cfg(test)]
mod delimiter_tests {
    use super::*;

    #[test]
    fn removes_trailing_annas_archive_tag_only() {
        for tag in ["Anna’s Archive", "Anna's Archive", "ANNA'S ARCHIVE"] {
            assert_eq!(filename_identity_evidence(&format!("A Title -- {tag}")).title, "A Title");
        }
        assert_eq!(filename_identity_evidence("A Guide to Anna’s Archive").title, "A Guide to Anna’s Archive");
    }

    #[test]
    fn preserves_meaningful_punctuation_except_whole_title_word_separators() {
        for value in ["Title - A Subtitle", "Title — A Subtitle", "C++ & C#", "10 % Less", "A + B", "Step-by-Step", "History 1900-1920", "- A Title -"] {
            assert_eq!(edition_identity_evidence(value).title, value, "{value}");
        }
        for value in ["The-Art-of-Computer-Programming", "The - Art - of - Computer - Programming"] {
            assert_eq!(filename_identity_evidence(value).title, "The Art of Computer Programming");
        }
        assert_eq!(filename_identity_evidence("A Title - libgen.li").title, "A Title");
    }

    #[test]
    fn preserves_ampersands_while_removing_separator_noise() {
        for value in ["Hackers & Painters", "Simon & Schuster", "Title_ Research & Development"] {
            assert!(edition_identity_evidence(value).title.contains(" & "));
            assert!(filename_identity_evidence(value).title.contains(" & "));
        }
        assert_eq!(filename_identity_evidence("Hackers & Painters - libgen.li").title, "Hackers & Painters");
    }

    #[test]
    fn mixed_brackets_and_semicolon_subtitles() {
        for title in ["Title [archive)", "Title (archive]", "Title [outer (inner])"] {
            assert_eq!(edition_identity_evidence(title).title, "Title");
        }
        assert_eq!(without_leading_bracket_segments("[Penguin Classics) (Bks. 21-30] Titus Livius Livy - The War with Hannibal"), "Titus Livius Livy - The War with Hannibal");
        for title in ["Title; Subtitle", "Title;Subtitle"] {
            assert_eq!(edition_identity_evidence(title).title, "Title: Subtitle");
            assert_eq!(filename_identity_evidence(title).title, "Title: Subtitle");
        }
    }

    #[test]
    fn repeated_subtitle_prefix_keeps_first_subtitle() {
        for value in ["The Old Money Book_ How to Live Better While Spending Less_ How to Live-Acorn Street Press (2020)", "The Old Money Book__ How to Live Better While Spending Less___ HOW TO LIVE (2020)"] {
            let parsed = filename_identity_evidence(value);
            assert_eq!(parsed.title, "The Old Money Book: How to Live Better While Spending Less");
            assert_eq!(parsed.book_year, Some(2020));
        }
        assert_eq!(filename_identity_evidence("Title_ A Better Life_ A Better Life").title, "Title: A Better Life");
        for value in ["Title_ How to Live_ How to Live Better", "Title_ How to Live_ How to Leave", "Title_ A Guide_ A", "Title_ First Subtitle_ Different Subtitle", "Title_ How to Live_ How to Live_ Extra"] {
            assert!(!filename_identity_evidence(value).title.contains(": "), "{value}");
        }
    }

    #[test]
    fn only_one_underscore_group_can_mark_a_subtitle() {
        for (input, expected) in [
            ("Title_ Subtitle", "Title: Subtitle"),
            ("Title___ Subtitle", "Title: Subtitle"),
            ("10_ Less Democracy_ Why You Should Trust Elites", "10 Less Democracy Why You Should Trust Elites"),
            ("Title___ Subtitle__ More", "Title Subtitle More"),
            ("Title_part_ Subtitle", "Title part: Subtitle"),
            ("Book_ The Hardware_Software Interface", "Book: The Hardware Software Interface"),
            ("Book__\tThe Hardware_Software Interface", "Book: The Hardware Software Interface"),
            ("Book_ Subtitle-Publisher_Record", "Book: Subtitle-Publisher Record"),
            ("Author_Title", "Author_Title"),
        ] {
            assert_eq!(filename_identity_evidence(input).title, expected, "{input}");
            assert_eq!(edition_identity_evidence(input).title, expected, "{input}");
        }
    }

    #[test]
    fn filename_by_separator_accepts_short_and_multiple_credits() {
        for (input, title, author) in [
            ("The Classical Music Book by DK", "The Classical Music Book", "DK"),
            ("About Face BY Alan Cooper, Robert Reimann, David Cronin, Christopher Noessel", "About Face", "Alan Cooper, Robert Reimann, David Cronin, Christopher Noessel"),
            ("Surveillance by J.K. Petersen (z-lib.org)", "Surveillance", "J.K. Petersen"),
        ] {
            let parsed = filename_identity_evidence(input);
            assert_eq!(parsed.title, title);
            assert_eq!(parsed.author.as_deref(), Some(author));
        }
        assert_eq!(edition_identity_evidence("Addiction by Design").title, "Addiction by Design");
    }

    #[test]
    fn removes_libgen_distribution_marker() {
        for title in ["Technopoly libgen.li", "Technopoly - LIBGEN.LI", "Technopoly (libgen.li)", "Technopoly [libgen.li]"] {
            assert_eq!(edition_identity_evidence(title).title, "Technopoly", "{title}");
        }
        assert_eq!(edition_identity_evidence("Library Genesis").title, "Library Genesis");
    }

    #[test]
    fn cleans_release_dots_and_scan_resolution() {
        assert_eq!(edition_identity_evidence("Understanding.Surveillance.Technologies.Spy.Devices.Their.Origins.and.Applications.eBook-EEn").title, "Understanding Surveillance Technologies Spy Devices Their Origins and Applications");
        assert_eq!(edition_identity_evidence("The.Geeks.of.Modern.War.EEn").title, "The Geeks of Modern War");
        assert_eq!(edition_identity_evidence("Operating System Concepts (7th Edition) 600 dpi").title, "Operating System Concepts (7th Edition)");
        assert_eq!(edition_identity_evidence("A 300 DPI Scanned Book").title, "A Scanned Book");
        for title in ["J.R.R. Tolkien", "Ph.D. Studies", "Version 2.0", "DPI Explained", "Een verhaal", "Structured Analytic Techniques for Intelligence Techniques"] {
            assert_eq!(edition_identity_evidence(title).title, title);
        }
        let clean = edition_identity_evidence("Understanding.Surveillance.Technologies.Spy.Devices.eBook-EEn").title;
        assert_eq!(edition_identity_evidence(&clean).title, clean);
    }

    #[test]
    fn brackets_extract_identity_and_remove_distribution_notes() {
        assert_eq!(edition_identity_evidence("Algorithms [Jeff Erickson] [2019] [electronic resource]"), EditionIdentityEvidence { title: "Algorithms".into(), author: Some("Jeff Erickson".into()), book_year: Some(2019) });
        assert_eq!(edition_identity_evidence("History [z-lib.org]").title, "History");
        assert_eq!(edition_identity_evidence("History [scan (archive)]").title, "History");
        assert_eq!(edition_identity_evidence("History (scan [archive])").title, "History");
    }

    #[test]
    fn preserves_structural_notes_and_solutions_in_both_delimiters() {
        for title in ["History [Volume 2]", "History [Revised Edition]", "Algorithms (solutions)", "Algorithms [solutions]", "Algorithms [Selected Answers]"] {
            let evidence = edition_identity_evidence(title);
            assert_eq!(evidence.title, title);
            assert_eq!(evidence.author, None);
        }
        assert_eq!(subject_identity_title("Algorithms [solutions]"), "Algorithms [solutions]");
    }

    #[test]
    fn malformed_groups_do_not_discard_title_text() {
        for title in ["Title [unfinished", "Title (unfinished", "Title [outer (inner]"] {
            assert_eq!(edition_identity_evidence(title).title, title);
        }
    }
}
