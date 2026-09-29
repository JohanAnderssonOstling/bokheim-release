//! Additional lookup evidence, independent of the fields selected for display.
use metadata_contract::EditionIdentityQuery;

pub const MAX_FILENAME_QUERIES: usize = 4;
pub fn queries(base: &EditionIdentityQuery, filenames: &[String]) -> Vec<EditionIdentityQuery> {
    let mut queries = Vec::new();
    for filename in filenames {
        let mut parsed = book_model::BookRecord { title: String::new(), subtitle: None, contributors: vec![], description: String::new(), book: Default::default() };
        if crate::title_identity::normalize_with_filename(&mut parsed, filename).is_err() {
            continue;
        }
        let title = crate::title_identity::full_title(&parsed.title, parsed.subtitle());
        if crate::filename_identity::unusable_title(&title) {
            continue;
        }
        let authors = parsed.contributors.iter().filter(|c| c.role_code() == book_model::AUTHOR_MARC_RELATOR_CODE).map(|c| c.name().to_owned()).collect::<Vec<_>>();
        let parsed_authors = (!authors.is_empty()).then_some(authors);
        let year = parsed.book.dates.iter().find_map(|d| d.value().get(..4).and_then(|s| s.parse().ok()));
        for authors in parsed_authors.into_iter().chain(std::iter::once(base.authors.clone())) {
            let mut query = base.clone();
            query.title = title.clone();
            query.authors = crate::resolution::clean_fallback_authors(&authors);
            query.book_year = query.book_year.or(year);
            let equivalent = |other: &EditionIdentityQuery| {
                metadata_contract::matching::NormalizedTitle::new(&query.title).full == metadata_contract::matching::NormalizedTitle::new(&other.title).full && query.authors == crate::resolution::clean_fallback_authors(&other.authors)
            };
            if equivalent(base) || queries.iter().any(equivalent) {
                continue;
            }
            query.query_id = format!("{}:filename:{}", base.query_id, queries.len());
            queries.push(query);
            if queries.len() == MAX_FILENAME_QUERIES {
                return queries;
            }
        }
    }
    queries
}

/// Apply the same subject-verification paths used by normal title lookup.
pub fn classifications(query: &EditionIdentityQuery, resource_isbns: &[String], result: &metadata_contract::EditionIdentityResult) -> Vec<metadata_contract::Classification> {
    if result.query_id != query.query_id || result.candidates_truncated {
        return vec![];
    }
    let query = metadata_contract::identity_evidence::verified_author_prefix_query(query, result).unwrap_or_else(|| query.clone());
    let verified = crate::related_isbn::verified_isbns(&query, resource_isbns, result);
    let codes = crate::related_isbn::classifications(&verified, |isbn| {
        result.matched.iter().chain(&result.candidates).filter(|m| &m.canonical_isbn13 == isbn).map(|m| crate::related_isbn::classification_record(m.classifications.clone())).collect()
    });
    if !codes.is_empty() {
        return codes;
    }
    if let Some(record) = result.authority_subjects.as_ref().and_then(|m| metadata_contract::authority_subjects::classification_record(&query, m)) {
        return record.classifications;
    }
    let codes = metadata_contract::subject_consensus::resolved_classifications(&query, result);
    if !codes.is_empty() {
        return codes;
    }
    if result.status == metadata_contract::EditionIdentityStatus::Matched {
        if let Some(record) = result.matched.clone().and_then(|m| crate::resolution::classification_only_fallback(&query, m)) {
            return record.classifications;
        }
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readable_display_title_does_not_hide_filename_author() {
        let base = EditionIdentityQuery { query_id: "book".into(), title: "Wall Street and FDR".into(), authors: vec!["Aggelos".into()], publishers: vec![], book_year: None };
        let queries = queries(&base, &["Wall Street and FDR by Antony C Sutton (z-lib.org).pdf".into()]);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].authors, vec!["Antony C Sutton"]);
        assert_eq!(base.authors, vec!["Aggelos"]);
        assert_eq!(queries[0].title, base.title);
    }
    #[test]
    fn filename_title_can_keep_known_authors_and_duplicate_placements_are_deduplicated() {
        let base = EditionIdentityQuery { query_id: "book".into(), title: "Cambridge concise histories".into(), authors: vec!["William D. Phillips Jr.".into(), "Carla Rahn Phillips".into()], publishers: vec![], book_year: None };
        let name = "A Concise History of Spain by William D. Phillips Jr Carla Rahn Phillips.epub".to_owned();
        let queries = queries(&base, &[name.clone(), name]);
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[1].title, "A Concise History of Spain");
        assert_eq!(queries[1].authors, crate::resolution::clean_fallback_authors(&base.authors));
    }
}

#[cfg(test)]
mod credit_tests {
    use super::*;
    #[test]
    fn uses_import_parser_for_comma_separated_full_names() {
        let base = EditionIdentityQuery { query_id: "livy".into(), title: "Discourses on Livy".into(), authors: vec!["Jim Manis".into()], publishers: vec![], book_year: None };
        let queries = queries(&base, &["Discourses on Livy by Niccolo Machiavelli, Ninian Hill Thomson (z-lib.org).pdf".into()]);
        assert_eq!(queries[0].authors, vec!["Niccolo Machiavelli", "Ninian Hill Thomson"]);
        assert_eq!(base.authors, vec!["Jim Manis"]);
    }
    #[test]
    fn variants_are_bounded_and_do_not_include_unusable_filenames() {
        let base = EditionIdentityQuery { query_id: "base".into(), title: "Stored title".into(), authors: vec![], publishers: vec![], book_year: None };
        let names = (0..20).map(|n| format!("A distinct title {n} by Jane Smith.pdf")).collect::<Vec<_>>();
        assert_eq!(queries(&base, &names).len(), MAX_FILENAME_QUERIES);
        assert!(queries(&base, &["unknown.pdf".into(), "untitled.pdf".into()]).is_empty());
    }
}
