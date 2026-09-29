//! Normalize ingested bibliographic fields and preserve their original title evidence.
use metadata_contract::identity_evidence::edition_identity_evidence;

pub fn full_title(title: &str, subtitle: Option<&str>) -> String {
    match subtitle {
        Some(subtitle) => format!("{title}: {subtitle}"),
        None => title.to_owned(),
    }
}

/// Reject an author credit only when the complete title matches it.
pub fn unusable_title(title: &str, contributors: &[book_model::Contributor]) -> bool {
    fn key(value: &str) -> String {
        let value = value.trim();
        let value = match value.split_once(',') {
            Some((last, first)) if !first.contains(',') => format!("{first} {last}"),
            _ => value.to_owned(),
        };
        metadata_contract::matching::author_name_match_key(&value)
    }
    let title_key = key(title);
    super::filename_identity::unusable_title(title) || contributors.iter().any(|credit| credit.role_code() == book_model::AUTHOR_MARC_RELATOR_CODE && key(credit.name()) == title_key)
}

/// Remove only a trailing, corroborated author-labelled omnibus segment.
fn without_author_collection<'a>(subtitle: &'a str, metadata: &book_model::BookRecord) -> Option<&'a str> {
    let (remaining, mut segment) = subtitle.rsplit_once(": ")?;
    if remaining.trim().is_empty() {
        return None;
    }
    let key = metadata_contract::matching::bibliographic_match_key;
    for (index, _) in segment.char_indices().filter(|(_, c)| matches!(c, '-' | '–' | '—')) {
        let tail = segment[index..].trim_start_matches(['-', '–', '—']).trim();
        if metadata.book.publishers.iter().any(|publisher| key(publisher.name().as_str()) == key(tail)) {
            segment = segment[..index].trim_end();
            break;
        }
    }
    let normalized = metadata_contract::matching::author_name_match_key(segment);
    let mut words = normalized.split_whitespace().collect::<Vec<_>>();
    if words.first() != Some(&"a") || words.pop() != Some("omnibus") {
        return None;
    }
    words.remove(0);
    words.retain(|word| !matches!(*word, "jr" | "sr" | "junior" | "senior" | "ii" | "iii" | "iv"));
    if words.len() < 2 {
        return None;
    }
    let surname = *words.last()?;
    if surname.chars().count() < 2 {
        return None;
    }
    let given = &words[..words.len() - 1];
    let matches = metadata.authors().any(|credit| {
        let normalized = metadata_contract::matching::author_name_match_key(credit.name());
        let parts = normalized.split_whitespace().filter(|word| !matches!(*word, "jr" | "sr" | "junior" | "senior" | "ii" | "iii" | "iv")).collect::<Vec<_>>();
        if parts.len() < 2 || !(parts.first() == Some(&surname) || parts.last() == Some(&surname)) {
            return false;
        }
        parts.iter().filter(|word| **word != surname).all(|word| given.iter().any(|other| word == other || ((word.chars().count() == 1 || other.chars().count() == 1) && word.chars().next() == other.chars().next())))
    });
    matches.then_some(remaining.trim_end())
}

fn has_edition_note(title: &str) -> bool {
    let lower = title.to_lowercase();
    let words = lower.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect::<Vec<_>>();
    words.windows(2).any(|pair| {
        matches!(pair[1], "edition" | "ed")
            && (matches!(pair[0], "first" | "second" | "third" | "fourth" | "fifth" | "sixth" | "seventh" | "eighth" | "ninth" | "tenth" | "revised" | "expanded" | "updated" | "new" | "anniversary")
                || ["st", "nd", "rd", "th"].iter().find_map(|suffix| pair[0].strip_suffix(suffix)).unwrap_or(pair[0]).parse::<u32>().is_ok())
    })
}

fn normalized_author_words(value: &str) -> Vec<String> {
    metadata_contract::matching::author_name_match_key(value).split_whitespace().filter(|word| metadata_contract::matching::author_suffix(word).is_none()).map(str::to_owned).collect()
}

/// Strip only prefixes backed by an author credit, at a filename separator.
fn filename_without_authors<'a>(filename: &'a str, contributors: &[book_model::Contributor]) -> &'a str {
    let mut aliases = std::collections::BTreeSet::new();
    let mut single_names = std::collections::BTreeSet::new();
    for credit in contributors.iter().filter(|credit| credit.role_code() == book_model::AUTHOR_MARC_RELATOR_CODE) {
        let name = credit.name();
        let words = normalized_author_words(name);
        if words.len() == 1 {
            single_names.insert(words[0].clone());
        }
        if words.len() < 2 {
            continue;
        }
        aliases.insert(words.concat());
        let given_first = book_model::given_name_first(name).unwrap_or_else(|| name.to_owned());
        let words = normalized_author_words(&given_first);
        aliases.insert(words.concat());
        let (family, given) = words.split_last().unwrap();
        aliases.insert(format!("{}{}", family, given.concat()));
        let initials: String = given.iter().filter_map(|word| word.chars().next()).collect();
        aliases.insert(format!("{family}{initials}"));
        aliases.insert(format!("{initials}{family}"));
    }
    // Only the first spaced hyphen can delimit a filename credit section.
    if let Some((index, dash)) =
        filename.char_indices().find(|(index, character)| matches!(character, '-' | '–' | '—') && filename[..*index].ends_with(char::is_whitespace) && filename[*index + character.len_utf8()..].starts_with(char::is_whitespace))
    {
        let prefix = filename[..index].trim_end();
        // A dangling underscore before the hyphen is an incomplete filename,
        // not evidence that everything before the hyphen is an author section.
        if prefix.ends_with('_') {
            return filename;
        }
        let words = normalized_author_words(prefix);
        let single_match = prefix.len() <= 512
            && prefix.split(['_', ';', ',', '&']).any(|entry| {
                let cleaned = edition_identity_evidence(entry).title;
                let entry_words = normalized_author_words(&cleaned);
                entry_words.len() == 1 && single_names.contains(&entry_words[0])
            });
        let matched = single_match || (prefix.len() <= 512 && (0..words.len()).any(|start| (start + 2..=words.len()).any(|end| aliases.contains(&words[start..end].concat()))));
        let tail = filename[index + dash.len_utf8()..].trim_start();
        if matched && !super::filename_identity::unusable_title(tail) {
            let parsed = metadata_contract::identity_evidence::filename_identity_evidence(tail);
            if !super::filename_identity::unusable_title(&parsed.title) {
                return tail;
            }
        }
    }
    let mut rest = filename;
    loop {
        let end = rest
            .char_indices()
            .filter(|(_, c)| c.is_whitespace() || matches!(c, '_' | '-' | '–' | '—' | ',' | '&'))
            .filter(|(i, _)| *i > 0 && *i <= 512)
            .filter_map(|(i, _)| aliases.contains(&normalized_author_words(&rest[..i]).concat()).then_some(i))
            .max();
        let Some(end) = end else {
            return rest;
        };
        let next = rest[end..].trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '_' | '-' | '–' | '—' | ',' | '&'));
        if next.is_empty() {
            return rest;
        }
        rest = next;
    }
}

fn filename_title_stem(filename: &str, contributors: &[book_model::Contributor]) -> String {
    let stem = std::path::Path::new(filename).file_stem().and_then(|stem| stem.to_str()).unwrap_or(filename);
    let (clean, _) = super::filename_identifiers::clean(stem);
    filename_without_authors(metadata_contract::identity_evidence::without_leading_bracket_segments(&clean), contributors).to_owned()
}

fn filename_evidence(metadata: &book_model::BookRecord, stem: &str) -> metadata_contract::identity_evidence::EditionIdentityEvidence {
    if metadata.subtitle().is_none() && !unusable_title(&metadata.title, &metadata.contributors) {
        metadata_contract::identity_evidence::filename_identity_evidence_with_title(stem, &metadata.title)
    } else {
        metadata_contract::identity_evidence::filename_identity_evidence(stem)
    }
}

/// Compare bibliographic title text, excluding filename credits and archive tags.
/// Edition information wins, then subtitle structure; ties retain the embedded title.
pub fn prefer_filename(metadata: &book_model::BookRecord, filename: &str) -> bool {
    let stem = filename_title_stem(filename, &metadata.contributors);
    // Structured archive parsing can discard the identifier-bearing suffix.
    // Validate that resulting candidate below before allowing it to compete.
    if unusable_title(&stem, &metadata.contributors) && !super::filename_identity::contains_long_identifier(&stem) {
        return false;
    }
    let filename_evidence = filename_evidence(metadata, &stem);
    let candidate = &filename_evidence.title;
    if unusable_title(&candidate, &metadata.contributors) || super::filename_identity::identifier_shaped_title(&candidate) {
        return false;
    }
    let current = full_title(&metadata.title, metadata.subtitle());
    if unusable_title(&current, &metadata.contributors) || super::filename_identity::identifier_shaped_title(&current) {
        return true;
    }
    let current = edition_identity_evidence(&current).title;
    let compact = |title: &str| metadata_contract::matching::bibliographic_match_key(title).replace(' ', "");
    if compact(&candidate) == compact(&current) {
        return false;
    }
    // Low-level readers may already have copied/cleaned the filename into the
    // title. Still apply its explicit by-credit syntax when the evidence agrees.
    if filename_evidence.author.is_some() && stem.as_bytes().windows(4).any(|part| part.eq_ignore_ascii_case(b" by ")) && compact(&current) == compact(&edition_identity_evidence(&stem).title) {
        return true;
    }
    let rank = |title: &str| (has_edition_note(title), title.contains(": "));
    rank(&candidate) > rank(&current)
}

/// Split only author credits; each person keeps their own display name.
fn split_author_semicolons(metadata: &mut book_model::BookRecord) -> Result<(), String> {
    use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE};
    let mut credits = Vec::new();
    for credit in &metadata.contributors {
        if credit.role_code() != AUTHOR_MARC_RELATOR_CODE || !credit.name().contains(';') {
            credits.push(credit.clone());
            continue;
        }
        let names = credit.name().split(';').map(str::trim).filter(|name| !name.is_empty()).collect::<Vec<_>>();
        for (index, name) in names.iter().enumerate() {
            let person = if index == 0 { Contributor::with_id(credit.contributor_id(), name, credit.role_code()) } else { Contributor::new(name, credit.role_code()) }.map_err(|e| e.to_string())?;
            credits.push(person);
        }
    }
    metadata.contributors = credits;
    Ok(())
}

/// Split repeated surname/given-name pairs only when each complete name is
/// independently present in the filename's credit section.
fn split_corroborated_comma_authors(metadata: &mut book_model::BookRecord, filename: &str) -> Result<(), String> {
    use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE};
    let stem = std::path::Path::new(filename).file_stem().and_then(|s| s.to_str()).unwrap_or(filename);
    let stem = metadata_contract::identity_evidence::without_leading_bracket_segments(stem);
    let Some((end, _)) = stem.char_indices().find(|(index, c)| matches!(c, '-' | '–' | '—') && stem[..*index].ends_with(char::is_whitespace) && stem[*index + c.len_utf8()..].starts_with(char::is_whitespace)) else {
        return Ok(());
    };
    let prefix = stem[..end].trim_end();
    if prefix.len() > 512 || prefix.ends_with('_') {
        return Ok(());
    }
    let entries = prefix.split([',', ';', '_', '&']).map(|entry| normalized_author_words(&edition_identity_evidence(entry).title)).collect::<Vec<_>>();
    let mut credits = Vec::new();
    for credit in &metadata.contributors {
        let parts = credit.name().split(',').map(str::trim).collect::<Vec<_>>();
        if credit.role_code() != AUTHOR_MARC_RELATOR_CODE || parts.len() < 4 || parts.len() > 16 || parts.len() % 2 != 0 || parts.iter().any(|part| part.is_empty()) {
            credits.push(credit.clone());
            continue;
        }
        let names = parts.chunks_exact(2).map(|pair| format!("{} {}", pair[1], pair[0])).collect::<Vec<_>>();
        let keys = names.iter().map(|name| normalized_author_words(name)).collect::<Vec<_>>();
        let distinct = keys.iter().collect::<std::collections::BTreeSet<_>>().len() == keys.len();
        if !distinct || !keys.iter().all(|key| key.len() >= 2 && entries.contains(key)) {
            credits.push(credit.clone());
            continue;
        }
        for (index, name) in names.iter().enumerate() {
            let person = if index == 0 { Contributor::with_id(credit.contributor_id(), name, credit.role_code()) } else { Contributor::new(name, credit.role_code()) }.map_err(|e| e.to_string())?;
            credits.push(person);
        }
    }
    metadata.contributors = credits;
    Ok(())
}

/// Prefer usable filename evidence when its parsed title is more descriptive.
pub fn normalize_with_filename(metadata: &mut book_model::BookRecord, filename: &str) -> Result<(), String> {
    let mut next = metadata.clone();
    split_author_semicolons(&mut next)?;
    split_corroborated_comma_authors(&mut next, filename)?;
    let original_stem = std::path::Path::new(filename).file_stem().and_then(|stem| stem.to_str()).unwrap_or(filename);
    super::filename_isbn::record(&next.title, &mut next.book).map_err(|e| e.to_string())?;
    super::filename_identifiers::record(original_stem, &mut next.book)?;
    if prefer_filename(&next, filename) {
        let stem = filename_title_stem(filename, &next.contributors);
        let evidence = filename_evidence(&next, &stem);
        next.title = evidence.title;
        if next.contributors.iter().filter(|credit| credit.role_code() == book_model::AUTHOR_MARC_RELATOR_CODE).all(|credit| super::filename_identity::unusable_identity(credit.name())) {
            if let Some(author) = evidence.author {
                let parts = author.split(',').map(str::trim).collect::<Vec<_>>();
                let names = if parts.iter().all(|part| part.split_whitespace().count() >= 2) { parts } else { vec![author.as_str()] };
                let credits = names.into_iter().map(|name| book_model::Contributor::new(name, book_model::AUTHOR_MARC_RELATOR_CODE).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
                next.contributors.retain(|credit| credit.role_code() != book_model::AUTHOR_MARC_RELATOR_CODE);
                next.contributors.extend(credits);
            }
        }
        if next.book.dates.is_empty() {
            if let Some(year) = evidence.book_year {
                next.book.dates.push(book_model::BookDate::new(None, year.to_string(), Some("book".into()), "ingestion:title").map_err(|e| e.to_string())?);
            }
        }
        next.subtitle = None;
    }
    normalize(&mut next)?;
    *metadata = next;
    Ok(())
}

/// Populate the ordinary metadata fields before persistence. Explicit subtitles,
/// usable author credits and existing dates take precedence over inference.
pub fn normalize(metadata: &mut book_model::BookRecord) -> Result<(), String> {
    use book_model::{BookDate, Contributor, AUTHOR_MARC_RELATOR_CODE};
    split_author_semicolons(metadata)?;
    super::filename_isbn::record(&full_title(&metadata.title, metadata.subtitle()), &mut metadata.book).map_err(|e| e.to_string())?;
    if unusable_title(&full_title(&metadata.title, metadata.subtitle()), &metadata.contributors) {
        // Without a usable source filename, do not reinterpret technical junk
        // (for example the colon in a Windows path) as bibliographic fields.
        let original = full_title(&metadata.title, metadata.subtitle());
        if super::filename_identity::kindle_identifier_title(&original)
            || super::filename_identity::standard_title_placeholder(&original)
            || !super::filename_isbn::from_filename(&original).is_empty()
            || original.trim().eq_ignore_ascii_case("true")
            || original.trim().to_ascii_lowercase().starts_with("oup_")
            || !super::filename_identity::unusable_title(&original)
        {
            metadata.title = "Untitled".into();
            metadata.subtitle = None;
        }
        return Ok(());
    }
    let mut next = metadata.clone();
    let explicit_subtitle = next.subtitle.is_some();
    // Normalize matching explicit suffixes first, but parse a fallback subtitle
    // together with its title so credits at the end are extracted once.
    next.normalize_title();
    let parsed_input = if explicit_subtitle { next.title.clone() } else { full_title(&next.title, next.subtitle()) };
    let evidence = edition_identity_evidence(&parsed_input);
    if !evidence.title.trim().is_empty() && evidence.title != parsed_input {
        next.title = evidence.title;
        if !explicit_subtitle {
            next.subtitle = None;
        }
        next.normalize_title();
    }
    if let Some(subtitle) = next.subtitle().and_then(|subtitle| without_author_collection(subtitle, &next)).map(str::to_owned) {
        next.subtitle = Some(subtitle);
    }
    if next.contributors.iter().filter(|credit| credit.role_code() == AUTHOR_MARC_RELATOR_CODE).all(|credit| super::filename_identity::unusable_identity(credit.name())) {
        if let Some(author) = evidence.author {
            // Commas between full names delimit credits; surname-first names
            // remain a single credit.
            let parts = author.split(',').map(str::trim).collect::<Vec<_>>();
            let names = if parts.iter().all(|part| part.split_whitespace().count() >= 2) { parts } else { vec![author.as_str()] };
            let inferred = names.into_iter().map(|name| Contributor::new(name, AUTHOR_MARC_RELATOR_CODE).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
            next.contributors.retain(|credit| credit.role_code() != AUTHOR_MARC_RELATOR_CODE);
            next.contributors.extend(inferred);
        }
    }
    if next.book.dates.is_empty() {
        if let Some(year) = evidence.book_year {
            next.book.dates.push(BookDate::new(None, year.to_string(), Some("book".into()), "ingestion:title").map_err(|e| e.to_string())?);
        }
    }
    *metadata = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(title: &str) -> book_model::BookRecord {
        book_model::BookRecord { title: title.into(), subtitle: None, contributors: vec![], description: String::new(), book: Default::default() }
    }

    #[test]
    fn author_omnibus_segment_requires_corroborated_surname_and_initial() {
        for (author, segment, remove) in [
            ("Buckley F.", "A William F. Buckley Jr. Omnibus", true),
            ("BUCKLEY, F.", "A William F. Buckley Jr. Omnibus", true),
            ("Buckley G.", "A William F. Buckley Jr. Omnibus", false),
            ("Smith F.", "A William F. Buckley Jr. Omnibus", false),
            ("Buckley", "A William F. Buckley Jr. Omnibus", false),
            ("Buckley F.", "William F. Buckley and Modern Politics", false),
            ("Buckley F.", "A William F. Buckley Jr. Omnibus-Unknown Publisher", false),
        ] {
            let mut metadata = book(&format!("Athwart History: Half a Century of Polemics: {segment}"));
            metadata.contributors.push(book_model::Contributor::new(author, book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
            normalize(&mut metadata).unwrap();
            let expected = if remove { "Half a Century of Polemics".to_owned() } else { format!("Half a Century of Polemics: {segment}") };
            assert_eq!(metadata.subtitle(), Some(expected.as_str()), "{author}, {segment}");
        }
        let mut metadata = book("Athwart History");
        metadata.contributors.push(book_model::Contributor::new("Buckley F.", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
        metadata.book.publishers.push(book_model::PublisherCredit::new("Encounter Books").unwrap());
        normalize_with_filename(&mut metadata, "Buckley F. - Athwart History_ Half a Century of Polemics, Animadversions, and Illuminations_ A William F. Buckley Jr. Omnibus-Encounter Books (2010).epub").unwrap();
        assert_eq!(metadata.subtitle(), Some("Half a Century of Polemics, Animadversions, and Illuminations"));
        let once = metadata.clone();
        normalize(&mut metadata).unwrap();
        assert_eq!(metadata, once);
    }

    #[test]
    fn matching_embedded_title_anchors_multiple_subtitle_segments() {
        let mut metadata = book("They Made America");
        metadata.contributors.push(book_model::Contributor::new("Harold Evans", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
        normalize_with_filename(&mut metadata, "Evans, Harold - They Made America_ From the Steam Engine to the Search Engine_ Two Centuries of Innovators (2006, Back Bay Books_ Little Brown & Company) - libgen.li.epub").unwrap();
        assert_eq!(metadata.title, "They Made America");
        assert_eq!(metadata.subtitle(), Some("From the Steam Engine to the Search Engine: Two Centuries of Innovators"));
        let mut different = book("10% Less Democracy");
        normalize_with_filename(&mut different, "10_ Less Democracy_ Why You Should Trust Elites.epub").unwrap();
        assert_eq!(different.title, "10% Less Democracy");
        assert_eq!(different.subtitle(), None);
    }

    #[test]
    fn publisher_parentheses_do_not_count_as_subtitle_separators() {
        for (title, author, filename, subtitle) in [
            (
                "The Sedated Society",
                "James Davies",
                "James Davies - The Sedated Society_ The Causes and Harms of our Psychiatric Drug Epidemic (2017, Springer Nature_ Palgrave Macmillan) - libgen.li.epub",
                "The Causes and Harms of our Psychiatric Drug Epidemic",
            ),
            (
                "Makers and Takers",
                "Rana Foroohar",
                "Rana Foroohar - Makers and Takers_ the rise of finance and the fall of American business (2016, The Crown Publishing Group_ Crown Business) - libgen.li.epub",
                "the rise of finance and the fall of American business",
            ),
        ] {
            let mut metadata = book(title);
            metadata.contributors.push(book_model::Contributor::new(author, book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, title);
            assert_eq!(metadata.subtitle(), Some(subtitle));
        }
    }

    #[test]
    fn long_identifier_without_usable_fallback_keeps_title() {
        let original = "A Title 6ddce1a50d360421c0c73c76f698f904 -- Anna’s Archive";
        let mut metadata = book(original);
        normalize_with_filename(&mut metadata, &format!("{original}.epub")).unwrap();
        assert_eq!(metadata.title, original);
    }

    #[test]
    fn archive_hash_filenames_cannot_override_clean_embedded_titles() {
        for (title, filename) in [
            ("Tysk höst", "Tysk höst -- Dagerman, Stig -- 2009;2010 -- Norstedts -- 9789113031194 -- 1c026c9c84b8c26cfa2e8544ec01cec2 -- Anna’s Archive.epub"),
            (
                "The Well-Educated Mind",
                "The Well-Educated Mind A Guide to the Classical Education -- Bauer, Susan, Wise -- UpdED, 2015;2014 -- W_ W_ Norton & Company -- 9780393253917 -- 928e8c08abc768441bd4965b1aacc4ea -- Anna’s Archive.epub",
            ),
            (
                "Rust Atomics and Locks",
                "Rust atomics and locks _ low-level concurrency in practice -- Mara Bos -- Early release, 2022 -- O'Reilly Media, Incorporated -- 9781098119386 -- 8e1fcd5d26a273d7019692e7729148ac -- Anna’s Archive.epub",
            ),
            ("Effective Rust", "Effective Rust_ 35 Specific Ways to Improve Your Rust Code -- David Drysdale -- 1, PS, 2024 -- O'Reilly Media, Incorporated -- 9781098151409 -- 2b9295592d125afeba4fe5e25e3fc25a -- Anna’s Archive.epub"),
            (
                "An Empire of Wealth",
                "An Empire of Wealth _ The Epic History of American Economic -- John Steele Gordon -- Open Road Integrated Media, Inc_, Pymble, NSW, 2008 -- isbn13 9780060505127 -- 4193ec78914db5b527a5f4dd87dd0512 -- Anna’s Archive.epub",
            ),
            ("The Reckoning", "The Reckoning -- David Halberstam -- Open Road Integrated Media, Inc_, New York, 2012 -- Open Road Integrated Media, Inc_ -- isbn13 9781453286104 -- 00ed60f6f9dc1a7d8717b9329e63df39 -- Anna’s Archive.epub"),
        ] {
            let mut metadata = book(title);
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, title);
            assert_eq!(metadata.subtitle(), None);
        }
    }

    #[test]
    fn filename_identifiers_are_preserved_without_vetoing_readable_titles() {
        for (filename, expected, isbn, doi) in [
            (
                "Steven S. Skiena (auth.) - Algorithm Design Manual, The (2012_2008, Springer-Verlag London) [10.1007_978-1-84800-070-4] - libgen.li.pdf",
                "Steven S. Skiena - Algorithm Design Manual, The",
                "9781848000704",
                "10.1007/978-1-84800-070-4",
            ),
            (
                "John J. Mearsheimer_ Sebastian Rosato - How States Think_ The Rationality of Foreign Policy (2023, Yale University Press) [10.12987_9780300274967] - libgen.li.pdf",
                "John J. Mearsheimer Sebastian Rosato - How States Think The Rationality of Foreign Policy",
                "9780300274967",
                "10.12987/9780300274967",
            ),
        ] {
            let mut metadata = book("Untitled");
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, expected);
            assert!(metadata.contributors.is_empty());
            for identifier in [isbn, doi] {
                assert!(metadata.book.identifiers.iter().any(|id| id.canonical_value().as_deref() == Some(identifier)));
            }
            let before = metadata.clone();
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata, before);
        }
        for filename in ["Readable Title ISBN 978-1-84800-070-4.pdf", "Readable Title [10.1000_example].pdf"] {
            let mut metadata = book("Untitled");
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, "Readable Title");
        }
        for filename in ["9781848000704.pdf", "[10.1000_example].pdf"] {
            let mut metadata = book("Untitled");
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, "Untitled");
        }
    }

    #[test]
    fn filename_corroborates_each_comma_paired_author() {
        use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE};
        let author = Contributor::new("LoPiccolo, Joseph, von Krafft-Ebing, Richard", AUTHOR_MARC_RELATOR_CODE).unwrap();
        let mut metadata = book("Psychopathia Sexualis");
        metadata.contributors.push(author.clone());
        let filename = "Richard von Krafft-Ebing, Joseph LoPiccolo - Psychopathia Sexualis_ The Classic Study of Deviant Sex (2011, Arcade Publishing) - libgen.li.epub";
        normalize_with_filename(&mut metadata, filename).unwrap();
        assert_eq!(metadata.title, "Psychopathia Sexualis");
        assert_eq!(metadata.subtitle(), Some("The Classic Study of Deviant Sex"));
        assert_eq!(metadata.contributors.iter().map(|c| c.name()).collect::<Vec<_>>(), ["Joseph LoPiccolo", "Richard von Krafft-Ebing"]);
        assert_eq!(metadata.contributors[0].contributor_id(), author.contributor_id());
        let before = metadata.clone();
        normalize_with_filename(&mut metadata, filename).unwrap();
        assert_eq!(metadata, before);
        for filename in ["Joseph LoPiccolo - Book.epub", "Joseph LoPiccolo, Richard Other - Book.epub", "Book about Joseph LoPiccolo, Richard von Krafft-Ebing - A Study.epub", "Richard von Krafft-Ebing, Joseph LoPiccolo_ - Book.epub"] {
            let mut metadata = book("Book");
            metadata.contributors.push(author.clone());
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.contributors, [author.clone()], "{filename}");
        }
        let mut metadata = book("Book");
        let author = Contributor::new("Smith, John, Jr.", AUTHOR_MARC_RELATOR_CODE).unwrap();
        metadata.contributors.push(author.clone());
        normalize_with_filename(&mut metadata, "John Smith Jr. - Book.epub").unwrap();
        assert_eq!(metadata.contributors, [author]);
    }

    #[test]
    fn single_name_authors_require_a_complete_filename_credit_entry() {
        use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE};
        let credits = vec![Contributor::new("Homer", AUTHOR_MARC_RELATOR_CODE).unwrap()];
        for filename in ["Homer - The Iliad.epub", "Homer_ Robert Fagles (translator) - The Iliad.epub", "Robert Fagles; HOMER - The Iliad.epub"] {
            assert_eq!(filename_title_stem(filename, &credits), "The Iliad");
        }
        for stem in ["Homer and Greek Poetry - A Study", "Homeric Poetry - A Study", "Homer's World - A Study", "The Homer Companion - A Study", "Homer_ - A Study", "Homer The Iliad"] {
            assert_eq!(filename_title_stem(&format!("{stem}.epub"), &credits), stem);
        }
        let mut metadata = book("The Iliad");
        metadata.contributors = credits;
        normalize_with_filename(&mut metadata, "Homer_ Robert Fagles (translator) - The Illiad (1990, Penguin Classics) - libgen.li.epub").unwrap();
        assert_eq!(metadata.title, "The Iliad");
        let mut metadata = book("The Complete Aristotle");
        metadata.contributors.push(Contributor::new("Aristotle", AUTHOR_MARC_RELATOR_CODE).unwrap());
        let filename = "Aristotle (Author), J. Barnes (Editor) - The Complete Works of Aristotle_ The Revised Oxford Translation (2 Volume Set) (1984, Princeton University Press) - libgen.li.epub";
        normalize_with_filename(&mut metadata, filename).unwrap();
        assert_eq!(metadata.title, "The Complete Works of Aristotle");
        assert_eq!(metadata.subtitle(), Some("The Revised Oxford Translation (2 Volume Set)"));
        assert_eq!(metadata.contributors.len(), 1);
        let before = metadata.clone();
        normalize_with_filename(&mut metadata, filename).unwrap();
        assert_eq!(metadata, before);
    }

    #[test]
    fn ampersand_survives_ingestion_and_filename_subtitle_selection() {
        let mut metadata = book("Hackers & Painters");
        metadata.contributors.push(book_model::Contributor::new("Paul Graham", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
        normalize_with_filename(&mut metadata, "Paul Graham - Hackers & Painters_ Big Ideas from the Computer Age (2008_2004, O’Reilly Media) - libgen.li.epub").unwrap();
        assert_eq!(metadata.title, "Hackers & Painters");
        assert_eq!(metadata.subtitle(), Some("Big Ideas from the Computer Age"));
        let before = metadata.clone();
        normalize(&mut metadata).unwrap();
        assert_eq!(metadata, before);
    }

    #[test]
    fn filename_by_credits_are_removed_from_filename_derived_titles() {
        for (raw, expected, authors) in [
            ("Concrete Mathematics (2nd Edition) by Ronald L. Graham, Donald E. Knuth, Oren Patashnik", "Concrete Mathematics (2nd Edition)", vec!["Ronald L. Graham", "Donald E. Knuth", "Oren Patashnik"]),
            ("Silent Warfare by Abram N. Shulsky, Gary J. Schmitt", "Silent Warfare", vec!["Abram N. Shulsky", "Gary J. Schmitt"]),
            ("Color Design Workbook by AdamsMorioka", "Color Design Workbook", vec!["AdamsMorioka"]),
            ("Surveillance by J.K. Petersen", "Surveillance", vec!["J.K. Petersen"]),
            ("Manual by CIA", "Manual", vec!["CIA"]),
        ] {
            let filename = format!("{raw} (z-lib.org).pdf");
            for current in [raw.to_owned(), edition_identity_evidence(&filename[..filename.len() - 4]).title] {
                let mut metadata = book(&current);
                normalize_with_filename(&mut metadata, &filename).unwrap();
                assert_eq!(metadata.title, expected);
                assert_eq!(metadata.contributors.iter().map(|c| c.name()).collect::<Vec<_>>(), authors);
                let before = metadata.clone();
                normalize_with_filename(&mut metadata, &filename).unwrap();
                assert_eq!(metadata, before);
            }
        }
        let mut metadata = book("Addiction by Design");
        normalize_with_filename(&mut metadata, "Unrelated by Jane Doe.pdf").unwrap();
        assert_eq!(metadata.title, "Addiction by Design");
        let mut metadata = book("Silent Warfare by Abram N. Shulsky, Gary J. Schmitt");
        metadata.contributors.push(book_model::Contributor::new("Existing Author", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
        normalize_with_filename(&mut metadata, "Silent Warfare by Abram N. Shulsky, Gary J. Schmitt.pdf").unwrap();
        assert_eq!(metadata.title, "Silent Warfare");
        assert_eq!(metadata.contributors[0].name(), "Existing Author");
    }

    #[test]
    fn honorifics_and_semicolon_authors_are_normalized_before_filename_matching() {
        use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE, CONTRIBUTOR_MARC_RELATOR_CODE};
        for author in ["Dr. Emily Morse", "Prof. Dr. Emily Morse", "Morse, Dr. Emily"] {
            let mut metadata = book("Smart Sex");
            metadata.contributors.push(Contributor::new(author, AUTHOR_MARC_RELATOR_CODE).unwrap());
            normalize_with_filename(&mut metadata, "Emily Morse - Smart Sex_ A Guide.epub").unwrap();
            assert_eq!(metadata.title, "Smart Sex");
            assert_eq!(metadata.subtitle(), Some("A Guide"));
            assert_eq!(metadata.contributors[0].name(), author);
            assert!(metadata_contract::matching::author_names_compatible(author, "Emily Morse"));
        }
        let mut metadata = book("Book");
        metadata.contributors.push(Contributor::new("Dr. Jane Doe; John Smith;", AUTHOR_MARC_RELATOR_CODE).unwrap());
        let first_id = metadata.contributors[0].contributor_id();
        metadata.contributors.push(Contributor::new("Other; Contributor", CONTRIBUTOR_MARC_RELATOR_CODE).unwrap());
        normalize_with_filename(&mut metadata, "John Smith - Book_ Subtitle.epub").unwrap();
        assert_eq!(metadata.subtitle(), Some("Subtitle"));
        assert_eq!(metadata.contributors.iter().map(|c| c.name()).collect::<Vec<_>>(), ["Dr. Jane Doe", "John Smith", "Other; Contributor"]);
        assert_eq!(metadata.contributors[0].contributor_id(), first_id);
        let before = metadata.clone();
        normalize(&mut metadata).unwrap();
        assert_eq!(metadata, before);
    }

    #[test]
    fn semicolon_subtitle_reaches_metadata_fields() {
        let mut metadata = book("Title; Subtitle");
        normalize(&mut metadata).unwrap();
        assert_eq!(metadata.title, "Title");
        assert_eq!(metadata.subtitle(), Some("Subtitle"));
        let before = metadata.clone();
        normalize(&mut metadata).unwrap();
        assert_eq!(metadata, before);
        let mut metadata = book("Title");
        normalize_with_filename(&mut metadata, "Title; Subtitle.epub").unwrap();
        assert_eq!(metadata.title, "Title");
        assert_eq!(metadata.subtitle(), Some("Subtitle"));
    }

    #[test]
    fn one_known_author_matches_filename_credit_section_after_bracket_cleanup() {
        let credits = vec![book_model::Contributor::new("David Riesman", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap()];
        for filename in [
            "David Riesman_ Nathan Glazer_ Reuel Denney_ Todd Gitlin - The Lonely Crowd_ A Study.epub",
            "[Series Book 3] Denney, Reuel_Gitlin, Todd_Glazer, Nathan_Riesman, David - The Lonely Crowd_ A Study.epub",
            "(Series Volume 4) [Archive] David Riesman_ Nathan Glazer - The Lonely Crowd_ A Study.epub",
        ] {
            assert_eq!(filename_title_stem(filename, &credits), "The Lonely Crowd_ A Study");
            let mut metadata = book("The Lonely Crowd");
            metadata.contributors = credits.clone();
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, "The Lonely Crowd");
            assert_eq!(metadata.subtitle(), Some("A Study"));
            assert_eq!(metadata.contributors, credits);
        }
        let credits = vec![book_model::Contributor::new("Nir Shavit", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap()];
        assert_eq!(filename_title_stem("[ITPro collection] Shavit, Nir _ - libgen.li.epub", &credits), "Shavit, Nir _ - libgen.li");
        assert_eq!(filename_title_stem("(Volume 4) Shavit, Nir __ - A Title.epub", &credits), "Shavit, Nir __ - A Title");
        assert_eq!(filename_title_stem("[Nir Shavit] Different Person - A Title.epub", &credits), "Different Person - A Title");
    }

    #[test]
    fn repeated_filename_subtitle_is_stored_once() {
        let filename = "Byron Tully - The Old Money Book_ How to Live Better While Spending Less_ How to Live-Acorn Street Press (2020).epub";
        let mut metadata = book("OMB_ebook");
        normalize_with_filename(&mut metadata, filename).unwrap();
        assert_eq!(metadata.title, "Byron Tully - The Old Money Book");
        assert_eq!(metadata.subtitle(), Some("How to Live Better While Spending Less"));
        assert!(metadata.contributors.is_empty());
        assert_eq!(metadata.book.dates[0].value(), "2020");
        let before = metadata.clone();
        normalize_with_filename(&mut metadata, filename).unwrap();
        assert_eq!(metadata, before);
    }

    #[test]
    fn leading_bracket_noise_is_removed_before_matching_author_prefixes() {
        for prefix in ["[The Pragmatic Programmers]", "[The Pragmatic Programmers] [archive]", "[Series (archive)]"] {
            let mut metadata = book("Your Code as a Crime Scene (for Manfred Kampf)");
            metadata.contributors.push(book_model::Contributor::new("Adam Tornhill", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
            let filename = format!("{prefix} Adam Tornhill - Your Code as a Crime Scene_ Use Forensic Techniques to Arrest Defects, Bottlenecks, and Bad Design in Your Programs (2015, Pragmatic Bookshelf) - libgen.li.epub");
            normalize_with_filename(&mut metadata, &filename).unwrap();
            assert_eq!(metadata.title, "Your Code as a Crime Scene");
            assert_eq!(metadata.subtitle(), Some("Use Forensic Techniques to Arrest Defects, Bottlenecks, and Bad Design in Your Programs"));
            assert_eq!(metadata.contributors[0].name(), "Adam Tornhill");
            let before = metadata.clone();
            normalize_with_filename(&mut metadata, &filename).unwrap();
            assert_eq!(metadata, before);
        }
        for value in ["[2nd Edition] Author - Title", "[Volume 1] Author - Title", "[Unclosed Author - Title"] {
            assert_eq!(metadata_contract::identity_evidence::without_leading_bracket_noise(value), value);
        }
    }

    #[test]
    fn longer_filename_does_not_override_accepted_title() {
        for (title, filename) in [
            ("Macbeth", "macbeth_advanced.epub"),
            ("10% Less Democracy", "10_ Less Democracy_ Why You Should Trust Elites a Little More and the Masses a Little Less.epub"),
            ("Nationalism", "134 Nationalism.m4b"),
            ("Technopoly", "Technopoly 1999.m4b"),
            ("A Book: A Guide", "A Book_ A Much Longer Guide.pdf"),
        ] {
            assert!(!prefer_filename(&book(title), filename), "{filename}");
            let mut metadata = book(title);
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(full_title(&metadata.title, metadata.subtitle()), title);
        }
        assert!(prefer_filename(&book("Unknown"), "A Much Longer Filename.pdf"));
    }

    #[test]
    fn filename_by_credit_does_not_inflate_title_and_can_supply_author() {
        assert!(!prefer_filename(&book("The Classical Music Book"), "The Classical Music Book by DK.pdf"));
        let mut metadata = book("Unknown");
        normalize_with_filename(&mut metadata, "The Classical Music Book by DK.pdf").unwrap();
        assert_eq!(metadata.title, "The Classical Music Book");
        assert_eq!(metadata.contributors[0].name(), "DK");
    }

    #[test]
    fn author_prefixes_use_normalized_credits_and_ignore_name_suffixes() {
        for (author, filename, title) in [
            ("Jane Austen", "jane-austen_emma.epub", "Emma"),
            ("Austen, Jane", "JANE_AUSTEN_Emma.epub", "Emma"),
            ("Hjalmar Söderberg", "SoderbergH_DoktorGlas.epub", "Doktor Glas"),
            ("Neil Postman", "Neil Postman - Technopoly - libgen.li.epub", "Technopoly"),
            ("Martin Luther King Jr.", "Martin-Luther-King_Freedom.epub", "Freedom"),
            ("Martin Luther King", "King, Martin Luther, Jr. - Freedom.epub", "Freedom"),
            ("John Smith III", "John Smith Jr. - A Book.epub", "A Book"),
        ] {
            let mut metadata = book(title);
            metadata.contributors.push(book_model::Contributor::new(author, book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
            normalize_with_filename(&mut metadata, filename).unwrap();
            assert_eq!(metadata.title, title, "{filename}");
            assert_eq!(metadata.contributors[0].name(), author);
        }
        let credit = book_model::Contributor::new("Jane Austen", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap();
        assert_eq!(filename_without_authors("Jane Eyre", &[credit.clone()]), "Jane Eyre");
        assert_eq!(filename_without_authors("JaneAustenland_Book", &[credit]), "JaneAustenland_Book");
        let mut metadata = book("Unknown");
        metadata.contributors.push(book_model::Contributor::new("Jane Austen", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
        normalize_with_filename(&mut metadata, "Jane Austen - Emma.epub").unwrap();
        assert_eq!(metadata.title, "Emma");
    }

    #[test]
    fn edition_information_precedes_subtitle() {
        let current = book("Operating System Concepts (7th Edition)");
        assert!(!prefer_filename(&current, "Operating System Concepts_ A Much Longer Description.pdf"));
        assert!(prefer_filename(&book("Operating System Concepts: An Extensive Guide"), "Operating System Concepts (7th Edition).pdf"));
        assert!(has_edition_note("Algorithms, Second Edition"));
        assert!(has_edition_note("Algorithms (3rd ed.)"));
        assert!(!has_edition_note("The Art of Edition Design"));
        assert!(!prefer_filename(&book("Algorithms (2nd Edition)"), "Algorithms (3rd Edition).pdf"));
    }

    #[test]
    fn filename_preference_uses_parsed_title_and_subtitle() {
        let mut kubark = book("Microsoft Word - Kubark");
        normalize_with_filename(&mut kubark, "KUBARK Counterintelligence Interrogation by Central Intelligence Agency (z-lib.org).pdf").unwrap();
        assert_eq!(kubark.title, "Microsoft Word - Kubark");
        let mut machiavelli = book("Quentin Skinner");
        normalize_with_filename(&mut machiavelli, "Machiavelli_ A Very Short Introduction.pdf").unwrap();
        assert_eq!(machiavelli.title, "Machiavelli");
        assert_eq!(machiavelli.subtitle(), Some("A Very Short Introduction"));
        assert!(!prefer_filename(&book("A descriptive embedded title"), "Short by Jane Author (2020) (archive).pdf"));
        assert!(!prefer_filename(&book("Good title"), "www.example.org.pdf"));
        assert!(!prefer_filename(&book("Good title"), "Production filename.docx.pdf"));
        assert!(prefer_filename(&book("A considerably longer title"), "Short_ Subtitle.epub"));
        let before = machiavelli.clone();
        normalize_with_filename(&mut machiavelli, "Machiavelli_ A Very Short Introduction.pdf").unwrap();
        assert_eq!(machiavelli, before);
    }

    #[test]
    fn production_cleanup_is_persisted_and_preserves_original() {
        for (original, expected) in [
            ("Understanding.Surveillance.Technologies.Spy.Devices.Their.Origins.and.Applications.eBook-EEn", "Understanding Surveillance Technologies Spy Devices Their Origins and Applications"),
            ("Operating System Concepts (7th Edition) 600 dpi", "Operating System Concepts (7th Edition)"),
        ] {
            let mut metadata = book(original);
            normalize_with_filename(&mut metadata, "unrelated.pdf").unwrap();
            assert_eq!(metadata.title, expected);
            let before = metadata.clone();
            normalize(&mut metadata).unwrap();
            assert_eq!(metadata, before);
        }
    }

    #[test]
    fn author_only_and_publisher_placeholders_use_filename() {
        for title in ["Quentin Skinner", "Skinner, Quentin", " QUENTIN  SKINNER ", "oup_6555", "OUP_5783", "true", " TRUE ", "ebook", "OMB_ebook"] {
            let mut metadata = book(title);
            metadata.contributors.push(book_model::Contributor::new("Quentin Skinner", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
            normalize_with_filename(&mut metadata, "Machiavelli.epub").unwrap();
            assert_eq!(metadata.title, "Machiavelli", "{title}");
        }
        for title in ["Machiavelli", "Quentin Skinner: A Biography", "True Grit", "OUP Publishing", "oup_12_notes"] {
            let mut metadata = book(title);
            metadata.contributors.push(book_model::Contributor::new("Quentin Skinner", book_model::AUTHOR_MARC_RELATOR_CODE).unwrap());
            assert!(!unusable_title(title, &metadata.contributors), "{title}");
        }
        let mut metadata = book("true");
        normalize_with_filename(&mut metadata, "oup_6555.mobi").unwrap();
        assert_eq!(metadata.title, "Untitled");
        let narrator = book_model::Contributor::new("Quentin Skinner", book_model::CONTRIBUTOR_MARC_RELATOR_CODE).unwrap();
        assert!(!unusable_title("Quentin Skinner", &[narrator]));
    }

    #[test]
    fn ambiguous_identifiers_require_a_usable_filename() {
        for title in ["AB12345678", "PRODUCT12345", "AB12345678 EBOK", "AB12345678_ebok"] {
            let mut metadata = book(title);
            normalize_with_filename(&mut metadata, "Readable title.epub").unwrap();
            assert_eq!(metadata.title, "Readable title");
            for filename in ["Untitled.epub", "XY98765432.epub", "XY98765432 by An Author.epub"] {
                let mut metadata = book(title);
                normalize_with_filename(&mut metadata, filename).unwrap();
                assert_eq!(metadata.title, title, "{filename}");
            }
            let mut metadata = book(title);
            normalize(&mut metadata).unwrap();
            assert_eq!(metadata.title, title);
        }
        for title in ["WW2", "Catch-22", "Product12345", "AB1234567", "ABCDEFGHI1", "A123456789", "AB12345678 Explained", "ÅB12345678"] {
            assert!(!super::super::filename_identity::identifier_shaped_title(title), "{title}");
            let mut metadata = book(title);
            normalize_with_filename(&mut metadata, "Readable title.epub").unwrap();
            assert_eq!(metadata.title, title);
        }
        assert_eq!(super::super::filename_identity::with_filename("AB12345678".into(), vec![], None, "Readable title.epub").0, "Readable title");
        assert_eq!(super::super::filename_identity::with_filename("AB12345678".into(), vec![], None, "XY98765432.epub").0, "AB12345678");
    }

    #[test]
    fn kindle_identifiers_fall_back_to_filename_and_preserve_original() {
        for (identifier, title, author) in
            [("B005CU4TJ6 EBOK", "Numbers", "Peter M. Higgins"), ("B07DPP4J1B EBOK", "Artificial Intelligence", "Margaret A. Boden"), ("B003EGGIBC EBOK", "The Koran", "Michael Cook"), ("B005JC0R84 EBOK", "Jesus", "Richard Bauckham")]
        {
            let mut metadata = book(identifier);
            let filename = format!("{title} A Very Short Introduction by {author} (z-lib.org).epub");
            normalize_with_filename(&mut metadata, &filename).unwrap();
            assert_eq!(metadata.title, format!("{title} A Very Short Introduction"));
            assert_eq!(metadata.contributors[0].name(), author);
            assert!(metadata.book.identifiers.is_empty());
            let before = metadata.clone();
            normalize_with_filename(&mut metadata, &filename).unwrap();
            assert_eq!(metadata, before);
        }
        let mut metadata = book("B005CU4TJ6 EBOK");
        normalize_with_filename(&mut metadata, "B005CU4TJ6.epub").unwrap();
        assert_eq!(metadata.title, "Untitled");
        let mut metadata = book("B005CU4TJ6 EBOK");
        normalize(&mut metadata).unwrap();
        assert_eq!(metadata.title, "Untitled");
    }

    #[test]
    fn isbn_title_becomes_identifier_and_filename_becomes_title() {
        let mut book = book("0192805045.pdf");
        normalize_with_filename(&mut book, "Fossils_ A Very Short Introduction.pdf").unwrap();
        assert_eq!(book.title, "Fossils");
        assert_eq!(book.subtitle(), Some("A Very Short Introduction"));
        assert_eq!(book.book.identifiers[0].value(), "0192805045");
        let before = book.clone();
        normalize_with_filename(&mut book, "Fossils_ A Very Short Introduction.pdf").unwrap();
        assert_eq!(book, before);
    }

    #[test]
    fn isbn_bearing_title_is_discarded_even_when_it_contains_words() {
        let mut book = book("Fossils ISBN 0192805045");
        normalize_with_filename(&mut book, "Different title.pdf").unwrap();
        assert_eq!(book.title, "Different title");
        assert_eq!(book.book.identifiers[0].value(), "0192805045");
        let mut unknown = super::tests::book("9780192802157.pdf");
        normalize_with_filename(&mut unknown, "Dreaming.pdf").unwrap();
        assert_eq!(unknown.title, "Dreaming");
        assert!(unknown.book.identifiers.is_empty());
    }

    #[test]
    fn isbn_title_is_discarded_even_without_a_usable_filename() {
        let mut metadata = book("Misleading text ISBN: 0192805045");
        normalize_with_filename(&mut metadata, "9780192802156.pdf").unwrap();
        assert_eq!(metadata.title, "Untitled");
        assert_eq!(metadata.book.identifiers[0].value(), "0192805045");
    }

    #[test]
    fn canonical_fields_are_clean_and_normalization_is_idempotent() {
        let mut book = book("Algorithms: A Guide (Jeff Erickson) (2019)");
        normalize(&mut book).unwrap();
        assert_eq!(book.title, "Algorithms");
        assert_eq!(book.subtitle(), Some("A Guide"));
        assert_eq!(book.contributors[0].name(), "Jeff Erickson");
        assert_eq!(book.book.dates[0].value(), "2019");
        let before = book.clone();
        normalize(&mut book).unwrap();
        assert_eq!(book, before);
    }

    #[test]
    fn technical_titles_without_a_fallback_are_not_parsed_as_subtitles() {
        let mut book = book(r"C:\Users\scanner\Document1.doc");
        normalize(&mut book).unwrap();
        assert_eq!(book.title, "Untitled");
        assert!(book.subtitle().is_none());
    }

    #[test]
    fn explicit_subtitle_author_and_date_win() {
        use book_model::{BookDate, Contributor, AUTHOR_MARC_RELATOR_CODE};
        let mut book = book("Star Trek: Voyager (Inferred Author) (2019)");
        book.set_subtitle("A Guide (Illustrated)");
        book.contributors.push(Contributor::new("Explicit Author", AUTHOR_MARC_RELATOR_CODE).unwrap());
        book.book.dates.push(BookDate::new(None, "2020-01-02", Some("book".into()), "epub:date").unwrap());
        normalize(&mut book).unwrap();
        assert_eq!(book.title, "Star Trek: Voyager");
        assert_eq!(book.subtitle(), Some("A Guide (Illustrated)"));
        assert_eq!(book.contributors[0].name(), "Explicit Author");
        assert_eq!(book.book.dates[0].value(), "2020-01-02");
    }

    #[test]
    fn archive_credits_fill_missing_authors_and_preserve_narrators() {
        use book_model::{Contributor, AUTHOR_MARC_RELATOR_CODE, NARRATOR_MARC_RELATOR_CODE};
        let mut book = book("Refactoring UI -- Steve Schoger, Adam Wathan -- 2018 -- archive");
        let narrator = Contributor::new("Narrator Name", NARRATOR_MARC_RELATOR_CODE).unwrap();
        book.contributors.push(narrator.clone());
        book.contributors.push(Contributor::new("Unknown", AUTHOR_MARC_RELATOR_CODE).unwrap());
        normalize(&mut book).unwrap();
        assert_eq!(book.title, "Refactoring UI");
        assert_eq!(book.contributors[0], narrator);
        assert_eq!(book.contributors.iter().skip(1).map(|credit| credit.name()).collect::<Vec<_>>(), ["Steve Schoger", "Adam Wathan"]);
        assert_eq!(book.book.dates[0].value(), "2018");
    }
}
