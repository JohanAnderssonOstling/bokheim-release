//! Classification recovery for bibliographically equivalent editions with year differences.
use crate::{
    matching::{bibliographic_match_key, matching_author_count, NormalizedTitle},
    Classification, EditionIdentityQuery, EditionIdentityResult, EditionIdentityStatus,
};
use std::collections::BTreeSet;

pub const METHOD: &str = "verified_year_variant_classifications";

fn credits_match(local: &[String], remote: &[String]) -> bool {
    matching_author_count(local, remote) > 0
}
fn publishers(values: &[String]) -> BTreeSet<String> {
    values.iter().map(|p| bibliographic_match_key(p)).filter(|p| !p.is_empty()).collect()
}

pub fn classifications(query: &EditionIdentityQuery, result: &EditionIdentityResult) -> Vec<Classification> {
    if result.status != EditionIdentityStatus::Ambiguous || result.query_id != query.query_id || result.candidates_truncated || result.candidates.len() < 2 {
        return Vec::new();
    }
    let title = NormalizedTitle::new(&query.title);
    let first = &result.candidates[0];
    let publisher = publishers(&first.publishers);
    if title.full.len() < 5 || publisher.is_empty() {
        return Vec::new();
    }
    for (index, candidate) in result.candidates.iter().enumerate() {
        if candidate.work_title.as_deref().map(bibliographic_match_key) != first.work_title.as_deref().map(bibliographic_match_key)
            || !title.matches(&NormalizedTitle::new(&candidate.full_title()))
            || !credits_match(&query.authors, &candidate.authors)
            || result.candidates[..index].iter().any(|previous| !credits_match(&previous.authors, &candidate.authors))
            || publishers(&candidate.publishers) != publisher
        {
            return Vec::new();
        }
    }
    crate::subject_consensus::candidate_classifications(result.candidates.iter(), METHOD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClassificationScheme, ClassificationSource, EditionIdentityMatch, EditionTitleMatch};
    fn fixture() -> (EditionIdentityQuery, EditionIdentityResult) {
        let query = EditionIdentityQuery { query_id: "1".into(), title: "Example History: Volume 2".into(), authors: vec!["Ada Author".into()], publishers: vec![], book_year: None };
        let candidates = [1999, 2013]
            .into_iter()
            .map(|year| EditionIdentityMatch {
                subtitle: None,
                canonical_isbn13: if year == 1999 { "9780821338278" } else { "9780393243277" }.into(),
                open_library_edition_id: format!("OL{year}M"),
                open_library_work_id: Some(format!("OL{year}W")),
                title: query.title.clone(),
                work_title: Some(query.title.clone()),
                title_match: EditionTitleMatch::Both,
                authors: query.authors.clone(),
                publishers: vec!["Example Press".into()],
                book_year: Some(year),
                matched_publisher: false,
                matched_book_year: false,
                classifications: vec![Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: format!("E806 .S89 {year}"), source: ClassificationSource::ExactEdition, evidence: vec![] }],
            })
            .collect();
        (query, EditionIdentityResult { authority_subjects: None, query_id: "1".into(), status: EditionIdentityStatus::Ambiguous, matched: None, candidates, candidates_truncated: false })
    }
    #[test]
    fn work_classifications_participate_in_agreement() {
        let (q, mut r) = fixture();
        let expected = classifications(&q, &r);
        assert!(!expected.is_empty());
        r.candidates[1].classifications[0].source = ClassificationSource::Work;
        assert_eq!(classifications(&q, &r), expected);
        r.candidates[0].classifications[0].source = ClassificationSource::Work;
        assert_eq!(classifications(&q, &r), expected);
    }

    #[test]
    fn year_difference_yields_work_classification_with_both_evidence_records() {
        let (q, r) = fixture();
        let codes = classifications(&q, &r);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].notation, "E806");
        assert_eq!(codes[0].source, ClassificationSource::Work);
        assert_eq!(codes[0].evidence.len(), 2);
        assert_ne!(codes[0].evidence[0].isbn13, codes[0].evidence[1].isbn13);
        assert!(r.matched.is_none());
    }
    #[test]
    fn rejects_identity_differences_but_collects_valid_classifications() {
        for change in 0..9 {
            let (mut q, mut r) = fixture();
            match change {
                0 => r.candidates[1].title = "Example History: Volume 3".into(),
                1 => r.candidates[1].title.push_str(" Revised edition"),
                2 => r.candidates[1].authors = vec!["Adam Author".into()],
                3 => r.candidates[1].publishers = vec!["Other Press".into()],
                4 => r.candidates[1].classifications[0].notation = "E807 .S90 2013".into(),
                5 => r.candidates[1].classifications[0].notation = "invalid".into(),
                6 => r.candidates_truncated = true,
                7 => q.authors.clear(),
                _ => r.candidates[1].classifications.clear(),
            }
            if matches!(change, 4 | 5 | 8) {
                let codes = classifications(&q, &r);
                assert_eq!(codes.len(), if change == 4 { 2 } else { 1 });
                assert!(codes.iter().all(|c| c.evidence.len() == 1));
            } else {
                assert!(classifications(&q, &r).is_empty(), "change {change}");
            }
        }
    }

    #[test]
    fn omitted_initial_resolves_but_conflicting_candidate_initials_do_not() {
        let (mut q, mut r) = fixture();
        q.authors = vec!["Brian Klaas".into()];
        r.candidates[0].authors = vec!["Brian Klaas".into()];
        r.candidates[1].authors = vec!["Brian P. Klaas".into()];
        assert!(!classifications(&q, &r).is_empty());
        let mut conflicting = r.candidates[1].clone();
        conflicting.authors = vec!["Brian Q. Klaas".into()];
        r.candidates.push(conflicting);
        assert!(classifications(&q, &r).is_empty());
    }
}
