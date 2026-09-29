//! Pure verification policy; callers perform ISBN requests without uploading book bytes.
use metadata_contract::{
    matching::{author_match_keys, matching_author_count, NormalizedTitle},
    Classification, ClassificationEvidence, ClassificationScheme, ClassificationSource, EditionIdentityQuery, EditionIdentityResult, EditionIdentityStatus, RichWorkMetadataMatch,
};

/// All checksum-valid ISBN references identified as a candidate edition of this
/// book at ingestion time (see `book_inspection::evidence::apply_evidence`).
/// These can identify another format of the book; they are not edition identities.
pub fn lookup_candidates(identifiers: &[book_model::Identifier]) -> Vec<String> {
    identifiers.iter().filter(|identifier| identifier.scheme() == &book_model::Scheme::Isbn && identifier.scope() == book_model::Scope::Edition).filter_map(book_model::Identifier::canonical_value).collect()
}

/// A printed ISBN disambiguates returned editions only when their full title and
/// at least one author agrees. A truncated search cannot establish that agreement.
pub fn verified_isbns(query: &EditionIdentityQuery, printed: &[String], result: &EditionIdentityResult) -> Vec<String> {
    if result.query_id != query.query_id || result.candidates_truncated || !matches!(result.status, EditionIdentityStatus::Matched | EditionIdentityStatus::Ambiguous) {
        return Vec::new();
    }
    let title = NormalizedTitle::new(&query.title);
    if title.full.len() < 5 || query.authors.is_empty() || query.authors.iter().any(|a| author_match_keys(a).iter().all(|k| k.split_whitespace().count() < 2)) {
        return Vec::new();
    }
    let records: Vec<_> = result.matched.iter().chain(&result.candidates).collect();
    let relevant = records.iter().filter(|e| printed.contains(&e.canonical_isbn13) && title.matches(&NormalizedTitle::new(&e.full_title()))).collect::<Vec<_>>();
    if relevant.iter().enumerate().any(|(index, e)| relevant[..index].iter().any(|previous| !authors_agree(&previous.authors, &e.authors))) {
        return Vec::new();
    }
    printed
        .iter()
        .filter(|isbn| {
            let editions: Vec<_> = records.iter().filter(|e| &e.canonical_isbn13 == *isbn).collect();
            !editions.is_empty() && editions.iter().all(|e| title.matches(&NormalizedTitle::new(&e.full_title())) && authors_agree(&query.authors, &e.authors))
        })
        .cloned()
        .collect()
}

/// Collect valid classifications from verified records. Transfer classifications only,
/// never a print edition's identifiers, work identity, description or entities.
pub fn classifications(verified: &[String], records: impl Fn(&str) -> Vec<RichWorkMetadataMatch>) -> Vec<Classification> {
    let mut accepted = Vec::new();
    for isbn in verified {
        for record in records(isbn) {
            let mut codes: Vec<_> = record.classifications.into_iter().filter(|c| c.scheme == ClassificationScheme::LibraryOfCongress && metadata_contract::classification_consensus_notation(c.scheme, &c.notation).is_some()).collect();
            for code in &mut codes {
                code.source = ClassificationSource::Work;
                code.evidence.push(ClassificationEvidence {
                    method: "local_bibliographic_print_isbn_verified_title_author".into(),
                    isbn13: isbn.clone(),
                    open_library_work_id: record.open_library_work_id.clone().unwrap_or_default(),
                    open_library_edition_id: record.exact_edition_ids.first().cloned().unwrap_or_default(),
                });
            }
            accepted.extend(codes);
        }
    }
    accepted.sort();
    accepted.dedup();
    accepted
}

/// Lookup accepted local references without claiming an exact edition match.
pub fn reference_classifications(isbn: &str, records: Vec<RichWorkMetadataMatch>) -> Vec<Classification> {
    let mut codes = classifications(&[isbn.to_owned()], |_| records.clone());
    for code in &mut codes {
        for evidence in &mut code.evidence {
            if evidence.method == "local_bibliographic_print_isbn_verified_title_author" {
                evidence.method = "local_bibliographic_isbn_reference".into();
            }
        }
    }
    codes
}

pub fn classification_record(classifications: Vec<Classification>) -> RichWorkMetadataMatch {
    RichWorkMetadataMatch { open_library_work_id: None, exact_edition_ids: Vec::new(), classifications, authors: Vec::new(), description: None, subjects: Vec::new() }
}

/// Keep the supporting print ISBN in the persisted subject's provenance.
pub fn subject(code: &Classification) -> Option<book_model::BookSubject> {
    if let Some(evidence) = code.evidence.iter().find(|e| e.method == "local_bibliographic_isbn_reference") {
        return book_model::BookSubject::new(None, &code.notation, format!("inspection:isbn-reference:{}", evidence.isbn13), Some("lcc".into()), Some(code.notation.clone())).ok();
    }
    let evidence = code.evidence.iter().find(|e| e.method == "local_bibliographic_print_isbn_verified_title_author")?;
    book_model::BookSubject::new(None, &code.notation, format!("inspection:verified-print-isbn:{}", evidence.isbn13), Some("lcc".into()), Some(code.notation.clone())).ok()
}

fn authors_agree(local: &[String], remote: &[String]) -> bool {
    matching_author_count(local, remote) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::IsbnEvidence;
    use metadata_contract::{EditionIdentityMatch, EditionTitleMatch};
    const ISBN: &str = "9780821338278";
    fn query() -> EditionIdentityQuery {
        EditionIdentityQuery { query_id: "print".into(), title: "Science History: Volume 4".into(), authors: vec!["Ada Author".into()], publishers: Vec::new(), book_year: None }
    }
    fn result() -> EditionIdentityResult {
        EditionIdentityResult {
            authority_subjects: None,
            query_id: "print".into(),
            status: EditionIdentityStatus::Matched,
            candidates_truncated: false,
            candidates: Vec::new(),
            matched: Some(EditionIdentityMatch {
                subtitle: None,
                canonical_isbn13: ISBN.into(),
                open_library_edition_id: "OL1M".into(),
                open_library_work_id: Some("OL1W".into()),
                title: query().title,
                work_title: None,
                title_match: EditionTitleMatch::Edition,
                authors: vec!["Author, Ada".into()],
                publishers: Vec::new(),
                book_year: None,
                matched_publisher: false,
                matched_book_year: false,
                classifications: Vec::new(),
            }),
        }
    }
    fn code(notation: &str) -> Classification {
        Classification { scheme: ClassificationScheme::LibraryOfCongress, notation: notation.into(), source: ClassificationSource::ExactEdition, evidence: Vec::new() }
    }
    #[test]
    fn lookup_candidates_reads_edition_scoped_isbn_identifiers_only() {
        let edition = book_model::Identifier::new(ISBN, book_model::Scheme::Isbn, book_model::Scope::Edition).unwrap();
        let book = book_model::Identifier::new("9780393243277", book_model::Scheme::Isbn, book_model::Scope::Book).unwrap();
        assert_eq!(lookup_candidates(&[edition.clone(), book]), [ISBN]);
        assert!(lookup_candidates(&[]).is_empty());
    }
    #[test]
    fn verifies_print_isbn_without_inferring_ebook_identity() {
        let verified = verified_isbns(&query(), &[ISBN.into()], &result());
        assert_eq!(verified, [ISBN]);
        let codes = classifications(&verified, |_| vec![classification_record(vec![code("Q125 .A12 2023")])]);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].source, ClassificationSource::Work);
        assert_eq!(subject(&codes[0]).unwrap().source(), format!("inspection:verified-print-isbn:{ISBN}"));
        let record = classification_record(codes);
        assert!(record.open_library_work_id.is_none() && record.exact_edition_ids.is_empty() && record.authors.is_empty() && record.subjects.is_empty());
    }
    #[test]
    fn rejects_wrong_volume_author_isbn_and_truncated_search() {
        for change in 0..6 {
            let mut r = result();
            match change {
                0 => r.matched.as_mut().unwrap().title = "Science History: Volume 3".into(),
                1 => r.matched.as_mut().unwrap().authors = vec!["Adam Author".into()],
                2 => r.matched.as_mut().unwrap().canonical_isbn13 = "9780393243277".into(),
                3 => r.candidates_truncated = true,
                4 => r.status = EditionIdentityStatus::NoMatch,
                _ => r.query_id = "another-book".into(),
            }
            assert!(verified_isbns(&query(), &[ISBN.into()], &r).is_empty(), "change {change}");
        }
    }
    #[test]
    fn ambiguous_identity_requires_print_isbn_and_rejects_contradiction() {
        let mut r = result();
        r.status = EditionIdentityStatus::Ambiguous;
        r.candidates.push(r.matched.take().unwrap());
        assert_eq!(verified_isbns(&query(), &[ISBN.into()], &r), [ISBN]);
        let mut contradiction = r.candidates[0].clone();
        contradiction.title = "Another Book".into();
        r.candidates.push(contradiction);
        assert!(verified_isbns(&query(), &[ISBN.into()], &r).is_empty());
    }
    #[test]
    fn differing_classifications_are_collected_and_empty_records_are_ignored() {
        let printed = vec![ISBN.into()];
        assert_eq!(classifications(&printed, |_| vec![classification_record(vec![code("Q125")]), classification_record(vec![code("E179")])]).len(), 2);
        assert_eq!(classifications(&printed, |_| vec![classification_record(Vec::new()), classification_record(vec![code("Q125")])]).len(), 1);
        assert_eq!(classifications(&[ISBN.into(), "9780393243277".into()], |isbn| vec![classification_record(vec![code(if isbn == ISBN { "Q125" } else { "E179" })])]).len(), 2);
    }
    #[test]
    fn missing_authors_fail_but_repeated_credits_do_not_veto_overlap() {
        let mut q = query();
        q.authors.clear();
        assert!(verified_isbns(&q, &[ISBN.into()], &result()).is_empty());
        q.authors = vec!["Ada Author".into(), "Ada Author".into()];
        let mut r = result();
        r.matched.as_mut().unwrap().authors.push("Other Writer".into());
        assert_eq!(verified_isbns(&q, &[ISBN.into()], &r), [ISBN]);
    }

    #[test]
    fn print_verification_accepts_omitted_initial_but_not_conflicting_credits() {
        let mut q = query();
        q.authors = vec!["Brian Klaas".into()];
        let mut r = result();
        r.matched.as_mut().unwrap().authors = vec!["Klaas, Brian P.".into()];
        assert_eq!(verified_isbns(&q, &[ISBN.into()], &r), [ISBN]);
        let mut conflicting = r.matched.as_ref().unwrap().clone();
        conflicting.authors = vec!["Brian Q. Klaas".into()];
        r.candidates.push(r.matched.as_ref().unwrap().clone());
        r.matched.as_mut().unwrap().authors = vec!["Brian Klaas".into()];
        r.candidates.push(conflicting);
        assert!(verified_isbns(&q, &[ISBN.into()], &r).is_empty());
    }
}
