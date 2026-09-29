//! Pure subject and author resolution for enrichment results.
use book_model::AuthorName;
use book_model::{AuthorAuthority, BookSubject, ExternalAuthorId};
use metadata_contract::{ClassificationScheme, ClassificationSource, EditionIdentityQuery, LookupStatus, RichWorkMetadataMatch};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Project supported classifications into unique canonical subject paths.
pub fn classification_paths(classifications: &[metadata_contract::Classification]) -> Vec<String> {
    classifications
        .iter()
        .flat_map(|classification| match classification.scheme {
            ClassificationScheme::LibraryOfCongress => subject_projection::unified_lcc_subject_paths(&classification.notation),
            ClassificationScheme::Bisac => subject_projection::unified_subject_paths(subject_projection::BISAC_SYSTEM_ID, &classification.notation),
            _ => Vec::new(),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn classification_rank(paths: &[String]) -> (usize, usize, usize) {
    let depths = paths.iter().map(|path| path.split('/').filter(|part| !part.trim().is_empty()).count()).collect::<Vec<_>>();
    (depths.len(), depths.iter().sum(), depths.into_iter().max().unwrap_or(0))
}

pub fn merge_rich_matches(matches: &[RichWorkMetadataMatch]) -> RichWorkMetadataMatch {
    metadata_contract::merge_rich_work_matches(matches, classification_similarity_keys)
}

pub fn classification_similarity_keys(scheme: ClassificationScheme, notation: &str) -> Vec<String> {
    let system_id = match scheme {
        ClassificationScheme::LibraryOfCongress => subject_projection::LCC_SYSTEM_ID,
        ClassificationScheme::Bisac => subject_projection::BISAC_SYSTEM_ID,
        _ => return Vec::new(),
    };
    subject_projection::unified_subject_similarity_keys(system_id, notation)
}

pub fn rich_subjects(matched: &RichWorkMetadataMatch) -> Result<Vec<BookSubject>, String> {
    let mut subjects = Vec::new();
    for heading in &matched.subjects {
        let name = heading.components.join(" / ");
        subjects.push(BookSubject::new(heading.authority_uri.clone(), name, &heading.source, Some(heading.authority.clone()), None).map_err(|error| error.to_string())?);
    }
    for classification in &matched.classifications {
        if classification.evidence.iter().any(|e| e.method.starts_with("librarything_isbn:")) {
            let authority = match classification.scheme {
                ClassificationScheme::LibraryOfCongress => "lcc",
                ClassificationScheme::DeweyDecimal => "ddc",
                ClassificationScheme::Bisac => "bisac",
            };
            subjects.push(BookSubject::new(None, &classification.notation, "librarything:metadata:isbn", Some(authority.into()), Some(classification.notation.clone())).map_err(|error| error.to_string())?);
            continue;
        }
        if classification.evidence.iter().any(|e| e.method.starts_with(metadata_contract::authority_subjects::METHOD_PREFIX)) {
            subjects.push(BookSubject::new(None, &classification.notation, "library_of_congress:metadata:title-author", Some("lcc".into()), Some(classification.notation.clone())).map_err(|error| error.to_string())?);
            continue;
        }
        if classification.evidence.iter().any(|e| e.method == metadata_contract::subject_consensus::METHOD) {
            subjects.push(BookSubject::new(None, &classification.notation, "metadata:shared-edition-classification", Some("lcc".into()), Some(classification.notation.clone())).map_err(|error| error.to_string())?);
            continue;
        }
        if classification.evidence.iter().any(|e| e.method == metadata_contract::year_consensus::METHOD) {
            subjects.push(BookSubject::new(None, &classification.notation, "metadata:year-only-edition-consensus", Some("lcc".into()), Some(classification.notation.clone())).map_err(|error| error.to_string())?);
            continue;
        }
        if let Some(subject) = crate::related_isbn::subject(classification) {
            subjects.push(subject);
            continue;
        }
        let authority = match classification.scheme {
            ClassificationScheme::LibraryOfCongress => "lcc",
            ClassificationScheme::Bisac => "bisac",
            _ => continue,
        };
        let source = match (matched.open_library_work_id.is_some(), classification.source) {
            (true, ClassificationSource::ExactEdition) => "openlibrary:metadata:exact-edition",
            (true, ClassificationSource::Work) => "openlibrary:metadata:work",
            (false, ClassificationSource::ExactEdition) => "metadata:exact-edition",
            (false, ClassificationSource::Work) => "metadata:work",
        };
        subjects.push(BookSubject::new(None, &classification.notation, source, Some(authority.to_owned()), Some(classification.notation.clone())).map_err(|error| error.to_string())?);
    }
    subjects.sort_by(|left, right| (left.authority(), left.name(), left.code()).cmp(&(right.authority(), right.name(), right.code())));
    subjects.dedup();
    Ok(subjects)
}

pub fn external_author_identity(author: &metadata_contract::AuthorMetadata) -> Option<ExternalAuthorId> {
    ExternalAuthorId::parse(AuthorAuthority::OpenLibrary, &author.open_library_author_id).ok().or_else(|| {
        let id = author.identifiers.iter().find(|id| id.authority == "wikidata")?;
        ExternalAuthorId::parse(AuthorAuthority::Wikidata, &id.value).ok()
    })
}

pub fn authoritative_author_resolutions(result: &metadata_contract::RichIsbnMetadataResult) -> Option<Vec<(usize, String, ExternalAuthorId)>> {
    if result.status != LookupStatus::Matched || result.matches.len() != 1 {
        return None;
    }
    let remote = &result.matches[0].authors;
    if remote.is_empty() {
        return None;
    }
    let mut names = HashMap::new();
    let mut ids = HashSet::new();
    let mut authors = Vec::new();
    for author in remote {
        let name = author.name.as_deref()?;
        let key = AuthorName::parse(name).ok()?.match_key().as_str().to_owned();
        let id = external_author_identity(author)?;
        if let Some(previous) = names.insert(key, author.identity_key()) {
            if previous != author.identity_key() {
                return None;
            }
        }
        if ids.insert(author.identity_key()) {
            authors.push((authors.len(), name.to_owned(), id));
        }
    }
    Some(authors)
}

pub fn match_author_resolutions(credits: &[(usize, &str)], remote: &[metadata_contract::AuthorMetadata]) -> (&'static str, Vec<(usize, String, ExternalAuthorId)>, Option<String>) {
    let mut remote_by_name = HashMap::<String, Vec<&metadata_contract::AuthorMetadata>>::new();
    for author in remote {
        let Some(name) = author.name.as_deref().and_then(|name| AuthorName::parse(name).ok()) else { continue };
        remote_by_name.entry(name.match_key().as_str().to_owned()).or_default().push(author);
    }
    let mut used = HashSet::new();
    let mut resolutions = Vec::new();
    for &(position, credit_name) in credits {
        let Some(name) = AuthorName::parse(credit_name).ok() else { continue };
        let Some(matches) = remote_by_name.get(name.match_key().as_str()).filter(|matches| matches.len() == 1) else { continue };
        let matched = matches[0];
        if !used.insert(matched.identity_key()) {
            continue;
        }
        let Some(external_id) = external_author_identity(matched) else { continue };
        resolutions.push((position, matched.name.clone().unwrap_or_else(|| credit_name.to_owned()), external_id));
    }
    if resolutions.is_empty() {
        ("ambiguous", resolutions, Some("Open Library authors could not be matched uniquely to the scanned author credits".to_owned()))
    } else {
        let unresolved = credits.len().saturating_sub(resolutions.len());
        let detail = (unresolved > 0).then(|| format!("resolved {} author credits; {unresolved} remained ambiguous", resolutions.len()));
        ("updated", resolutions, detail)
    }
}

pub fn clean_fallback_authors(authors: &[String]) -> Vec<String> {
    authors
        .iter()
        .flat_map(|a| a.split(';'))
        .map(|a| a.split('[').next().unwrap_or(a).trim())
        .filter(|a| !a.is_empty() && !a.to_lowercase().contains("calibre") && !a.contains(".com"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn classification_only_fallback(query: &EditionIdentityQuery, matched: metadata_contract::EditionIdentityMatch) -> Option<RichWorkMetadataMatch> {
    use metadata_contract::matching::matching_author_count;
    let local = clean_fallback_authors(&query.authors);
    let remote = clean_fallback_authors(&matched.authors);
    if matching_author_count(&local, &remote) == 0 {
        return None;
    }
    let mut classifications = matched.classifications;
    classifications.retain(|c| metadata_contract::classification_consensus_notation(c.scheme, &c.notation).is_some());
    if classifications.is_empty() {
        return None;
    }
    for classification in &mut classifications {
        classification.source = ClassificationSource::Work;
        classification.evidence.push(metadata_contract::ClassificationEvidence {
            method: "local_title_author_after_isbn_miss".into(),
            isbn13: matched.canonical_isbn13.clone(),
            open_library_work_id: matched.open_library_work_id.clone().unwrap_or_default(),
            open_library_edition_id: matched.open_library_edition_id.clone(),
        });
    }
    Some(RichWorkMetadataMatch { open_library_work_id: None, exact_edition_ids: Vec::new(), classifications, authors: Vec::new(), description: None, subjects: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metadata_contract::RichIsbnMetadataResult;
    #[test]
    fn iron_kingdom_keeps_classifications_despite_narrator_credit() {
        let query = EditionIdentityQuery { query_id: "iron".into(), title: "Iron Kingdom".into(), authors: vec!["Christopher Clark".into()], publishers: vec!["Tantor Audio".into()], book_year: None };
        let matched = metadata_contract::EditionIdentityMatch {
            canonical_isbn13: "9781515966029".into(),
            open_library_edition_id: "OL37768522M".into(),
            open_library_work_id: Some("OL224696W".into()),
            title: "Iron Kingdom".into(),
            subtitle: Some("The Rise and Downfall of Prussia, 1600-1947".into()),
            work_title: None,
            title_match: metadata_contract::EditionTitleMatch::Edition,
            authors: vec!["Christopher Clark".into(), "Shaun Grindell".into()],
            publishers: vec!["Tantor Audio".into()],
            book_year: Some(2017),
            matched_publisher: true,
            matched_book_year: false,
            classifications: vec![metadata_contract::Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: "DD389 .C53 2008".into(), source: ClassificationSource::Work, evidence: vec![] }],
        };
        assert!(!rich_subjects(&classification_only_fallback(&query, matched).unwrap()).unwrap().is_empty());
    }
    #[test]
    fn spain_classification_accepts_omitted_suffix_and_partial_author_agreement() {
        let mut query = EditionIdentityQuery { query_id: "spain".into(), title: "A Concise History of Spain".into(), authors: vec!["William D. Phillips Jr.".into(), "Carla Rahn Phillips".into()], publishers: vec![], book_year: None };
        let matched = metadata_contract::EditionIdentityMatch {
            canonical_isbn13: "9780521607216".into(),
            open_library_edition_id: "OL10435920M".into(),
            open_library_work_id: Some("OL15507568W".into()),
            title: query.title.clone(),
            subtitle: None,
            work_title: None,
            title_match: metadata_contract::EditionTitleMatch::Edition,
            authors: vec!["William D. Phillips".into(), "Carla Rahn Phillips".into()],
            publishers: vec![],
            book_year: Some(2008),
            matched_publisher: false,
            matched_book_year: false,
            classifications: vec![metadata_contract::Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: "DP17 .P55 2010".into(), source: ClassificationSource::ExactEdition, evidence: vec![] }],
        };
        let record = classification_only_fallback(&query, matched.clone()).unwrap();
        assert_eq!(rich_subjects(&record).unwrap()[0].code(), Some("DP17 .P55 2010"));
        query.authors[0] = "William E. Phillips".into();
        // Carla still agrees even when another credit differs.
        assert!(classification_only_fallback(&query, matched.clone()).is_some());
        query.authors = vec!["William D. Phillips".into(), "William D. Phillips Jr.".into()];
        assert!(classification_only_fallback(&query, matched.clone()).is_some());
        query.authors = vec!["Unrelated Person".into()];
        assert!(classification_only_fallback(&query, matched).is_none());
    }
    #[test]
    fn authoritative_authors_do_not_require_local_name_matches_or_choose_duplicate_ids() {
        let author = |id: &str, name: &str| metadata_contract::AuthorMetadata { open_library_author_id: id.into(), name: Some(name.into()), identifiers: vec![], source: metadata_contract::AuthorSource::Work, position: 0 };
        let mut result = RichIsbnMetadataResult {
            wikidata_books: Vec::new(),
            requested_isbn: "9780393242768".into(),
            canonical_isbn13: Some("9780393242768".into()),
            status: LookupStatus::Matched,
            matches: vec![RichWorkMetadataMatch {
                open_library_work_id: Some("OL1W".into()),
                exact_edition_ids: vec!["OL1M".into()],
                classifications: vec![],
                authors: vec![author("OL139896A", "George Frost Kennan")],
                description: None,
                subjects: vec![],
            }],
        };
        assert_eq!(authoritative_author_resolutions(&result).unwrap()[0].1, "George Frost Kennan");
        result.matches[0].authors.push(author("OL999A", "George Frost Kennan"));
        assert!(authoritative_author_resolutions(&result).is_none());
        result.matches[0].authors.pop();
        result.status = LookupStatus::Ambiguous;
        assert!(authoritative_author_resolutions(&result).is_none());
    }

    #[test]
    fn librarything_lcc_keeps_provider_provenance_on_an_openlibrary_work() {
        let matched = RichWorkMetadataMatch {
            open_library_work_id: Some("OL20068530W".into()),
            exact_edition_ids: vec![],
            classifications: vec![metadata_contract::Classification {
                scheme: ClassificationScheme::LibraryOfCongress,
                notation: "PS3608.O623".into(),
                source: ClassificationSource::Work,
                evidence: vec![metadata_contract::ClassificationEvidence { method: "librarything_isbn:22550208".into(), isbn13: "9781538724736".into(), open_library_work_id: String::new(), open_library_edition_id: String::new() }],
            }],
            authors: vec![],
            description: None,
            subjects: vec![],
        };
        let subjects = rich_subjects(&matched).unwrap();
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].source, "librarything:metadata:isbn");
        assert_eq!(subjects[0].code(), Some("PS3608.O623"));
    }

    #[test]
    fn wikidata_only_isbn_authors_keep_distinct_external_identities() {
        let authors = [("Q1", "Alex Smith"), ("Q2", "Jane Jones")]
            .into_iter()
            .enumerate()
            .map(|(position, (id, name))| metadata_contract::AuthorMetadata {
                open_library_author_id: String::new(),
                name: Some(name.into()),
                identifiers: vec![metadata_contract::AuthorIdentifier { authority: "wikidata".into(), value: id.into() }],
                source: metadata_contract::AuthorSource::Work,
                position: position as u32,
            })
            .collect();
        let record = metadata_contract::RichWorkMetadataMatch { open_library_work_id: None, exact_edition_ids: vec![], classifications: vec![], authors, description: None, subjects: vec![] };
        let merged = metadata_contract::merge_rich_work_matches([&record, &record], |_, _| vec![]);
        assert_eq!(merged.authors.len(), 2);
        let result = metadata_contract::RichIsbnMetadataResult {
            wikidata_books: vec![],
            requested_isbn: "9780306406157".into(),
            canonical_isbn13: Some("9780306406157".into()),
            status: metadata_contract::LookupStatus::Matched,
            matches: vec![merged],
        };
        let authors = authoritative_author_resolutions(&result).unwrap();
        assert_eq!(authors.len(), 2);
        assert_eq!(authors[0].2, ExternalAuthorId::parse(AuthorAuthority::Wikidata, "Q1").unwrap());
        assert_eq!(authors[1].2, ExternalAuthorId::parse(AuthorAuthority::Wikidata, "Q2").unwrap());
    }
}
