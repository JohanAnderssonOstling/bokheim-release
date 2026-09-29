//! Collect classifications from verified candidates without choosing an edition ISBN.
use crate::{
    matching::{author_credits, author_names_compatible, matching_author_count, title_qualifiers, title_substring_keys},
    Classification, ClassificationEvidence, ClassificationScheme, ClassificationSource, EditionIdentityQuery, EditionIdentityResult, EditionIdentityStatus,
};
use std::collections::BTreeSet;
pub const METHOD: &str = "verified_matching_edition_classifications";

fn title_key(title: &str) -> String {
    let mut key = crate::matching::NormalizedTitle::new(title).full;
    // This known series label is not a volume, revision, or adaptation qualifier.
    for suffix in [" very short introductions", " a very short introduction"] {
        if let Some(base) = key.strip_suffix(suffix) {
            key = base.to_owned();
        }
    }
    key
}

// Lookup can find an edition by a whole-word fragment of the local title.
// Keep qualifier evidence: a subtitle must not conceal a different volume or adaptation.
fn titles_compatible(local: &str, remote: &str) -> bool {
    let local_title = crate::matching::NormalizedTitle::new(local);
    let remote_title = crate::matching::NormalizedTitle::new(remote);
    if local_title.qualifiers != remote_title.qualifiers {
        return false;
    }
    if local_title.subtitle.is_none() && local_title.full == remote_title.main {
        return true;
    }
    let local = title_key(local);
    let remote = title_key(remote);
    if local == remote {
        return true;
    }
    if remote.len() < 5 || title_qualifiers(&local) != title_qualifiers(&remote) {
        return false;
    }
    let edition_markers = |text: &str| text.split_whitespace().filter(|word| matches!(*word, "edition" | "ed" | "revised" | "revision" | "translation" | "translated" | "unabridged")).map(str::to_owned).collect::<BTreeSet<_>>();
    edition_markers(&local) == edition_markers(&remote) && title_substring_keys(&local, 64).iter().any(|(key, _)| key == &remote)
}

pub fn classifications(query: &EditionIdentityQuery, result: &EditionIdentityResult) -> Vec<Classification> {
    let mut ranked = result.clone();
    let best = ranked.candidates.iter().map(|c| matching_author_count(&query.authors, &c.authors)).max().unwrap_or(0);
    if best > 0 {
        ranked.candidates.retain(|c| matching_author_count(&query.authors, &c.authors) == best);
    }
    let result = &ranked;
    let year_codes = crate::year_consensus::classifications(query, result);
    if !year_codes.is_empty() {
        return year_codes;
    }
    if result.status != EditionIdentityStatus::Ambiguous || result.query_id != query.query_id || result.candidates_truncated || result.candidates.len() < 2 {
        return Vec::new();
    }
    let title = title_key(&query.title);
    if title.len() < 5 || query.authors.is_empty() {
        return Vec::new();
    }
    let local_authors = author_credits(&query.authors);
    let mut credited_names: Vec<Vec<String>> = vec![Vec::new(); local_authors.len()];
    for candidate in &result.candidates {
        if !titles_compatible(&query.title, &candidate.full_title()) || candidate.authors.is_empty() {
            return Vec::new();
        }
        if matching_author_count(&local_authors, &candidate.authors) == 0 {
            return Vec::new();
        }
        // An initial must not merge explicitly different full names across editions.
        for (i, local) in local_authors.iter().enumerate() {
            for remote in author_credits(&candidate.authors).into_iter().filter(|remote| author_names_compatible(local, remote)) {
                if credited_names[i].iter().any(|previous| !author_names_compatible(previous, &remote)) {
                    return Vec::new();
                }
                credited_names[i].push(remote);
            }
        }
    }
    candidate_classifications(result.candidates.iter(), METHOD)
}

/// Union valid codes while attributing each code only to records that supplied it.
pub(crate) fn candidate_classifications<'a>(candidates: impl IntoIterator<Item = &'a crate::EditionIdentityMatch>, method: &str) -> Vec<Classification> {
    let mut codes = std::collections::BTreeMap::<String, Vec<ClassificationEvidence>>::new();
    for candidate in candidates {
        for code in &candidate.classifications {
            if code.scheme != ClassificationScheme::LibraryOfCongress {
                continue;
            }
            let Some(notation) = crate::classification_consensus_notation(code.scheme, &code.notation) else {
                continue;
            };
            let evidence = ClassificationEvidence {
                method: method.into(),
                isbn13: candidate.canonical_isbn13.clone(),
                open_library_work_id: candidate.open_library_work_id.clone().unwrap_or_default(),
                open_library_edition_id: candidate.open_library_edition_id.clone(),
            };
            let sources = codes.entry(notation).or_default();
            if !sources.contains(&evidence) {
                sources.push(evidence);
            }
        }
    }
    codes.into_iter().map(|(notation, evidence)| Classification { notation, scheme: ClassificationScheme::LibraryOfCongress, source: ClassificationSource::Work, evidence }).collect()
}

/// Prefer a unique exact title unless known authors conflict. Multiple exact
/// titles are ranked by the number of distinct matching author credits.
/// This resolves subjects only; it does not assert an exact local edition ISBN.
pub fn resolved_classifications(query: &EditionIdentityQuery, result: &EditionIdentityResult) -> Vec<Classification> {
    let agreed = classifications(query, result);
    if !agreed.is_empty() {
        return agreed;
    }
    if result.status != EditionIdentityStatus::Ambiguous || result.candidates.is_empty() || result.candidates_truncated || result.query_id != query.query_id {
        return vec![];
    }
    let known_authors = |authors: &[String]| author_credits(authors).into_iter().filter(|name| !matches!(name.trim().to_ascii_lowercase().as_str(), "" | "unknown" | "untitled")).collect::<Vec<_>>();
    let local = known_authors(&query.authors);
    let title = title_key(&query.title);
    if title.is_empty() {
        return vec![];
    }
    let full_key = crate::matching::NormalizedTitle::new(&query.title).full;
    let mut exact = result.candidates.iter().filter(|candidate| full_key == crate::matching::NormalizedTitle::new(&candidate.full_title()).full).collect::<Vec<_>>();
    if exact.is_empty() {
        exact = result.candidates.iter().filter(|candidate| title == title_key(&candidate.full_title())).collect();
    }
    let filename_surname = exact.is_empty() && local.is_empty();
    if filename_surname {
        exact = result.candidates.iter().filter(|candidate| crate::identity_evidence::filename_surname_title_matches(&query.title, &candidate.title, &candidate.authors)).collect();
    }
    let unique_title = exact.len() == 1;
    let best = exact.iter().map(|candidate| matching_author_count(&local, &candidate.authors)).max().unwrap_or(0);
    let matches = exact
        .into_iter()
        .filter(|candidate| {
            if local.is_empty() {
                return unique_title;
            }
            best > 0 && matching_author_count(&local, &candidate.authors) == best
        })
        .collect::<Vec<_>>();
    if matches.is_empty() || (local.is_empty() && matches.len() != 1) {
        return vec![];
    }
    for author in &local {
        let names = matches.iter().flat_map(|candidate| author_credits(&candidate.authors)).filter(|remote| author_names_compatible(author, remote)).collect::<Vec<_>>();
        if names.iter().enumerate().any(|(i, name)| names[..i].iter().any(|other| !author_names_compatible(name, other))) {
            return vec![];
        }
    }
    candidate_classifications(
        matches,
        if filename_surname {
            "verified_filename_surname_title_classification"
        } else if unique_title {
            "verified_unique_title_classification"
        } else {
            METHOD
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditionIdentityMatch, EditionTitleMatch};
    fn code(value: &str, source: ClassificationSource) -> Classification {
        Classification { notation: value.into(), scheme: ClassificationScheme::LibraryOfCongress, source, evidence: vec![] }
    }
    fn fixture() -> (EditionIdentityQuery, EditionIdentityResult) {
        let query = EditionIdentityQuery {
            query_id: "art".into(),
            title: "Art History: A Very Short Introduction (Very Short Introductions)".into(),
            authors: vec!["Arnold, Dana".into()],
            publishers: vec!["Oxford University Press".into()],
            book_year: Some(2004),
        };
        let candidates = [2004, 2020]
            .into_iter()
            .map(|year| EditionIdentityMatch {
                subtitle: None,
                canonical_isbn13: if year == 2004 { "9780192801814" } else { "9780198831808" }.into(),
                open_library_edition_id: format!("OL{year}M"),
                open_library_work_id: Some(format!("OL{year}W")),
                title: "Art History".into(),
                work_title: Some("Art History".into()),
                title_match: EditionTitleMatch::Both,
                authors: vec!["Dana Arnold".into()],
                publishers: vec![if year == 2004 { "Oxford University Press, USA" } else { "Oxford University Press" }.into()],
                book_year: Some(year),
                matched_publisher: year == 2020,
                matched_book_year: year == 2004,
                classifications: vec![
                    code("N5300", ClassificationSource::ExactEdition),
                    code(if year == 2004 { "N7425 .A646 2004" } else { "N7480 .A773 2010" }, if year == 2004 { ClassificationSource::ExactEdition } else { ClassificationSource::Work }),
                ],
            })
            .collect();
        (query, EditionIdentityResult { authority_subjects: None, query_id: "art".into(), status: EditionIdentityStatus::Ambiguous, matched: None, candidates, candidates_truncated: false })
    }
    #[test]
    fn candidate_corroborates_author_prefix_before_subject_assignment() {
        let (mut q, mut r) = fixture();
        q.title = "Paul Bushkovitch - A Concise History of Russia".into();
        q.authors.clear();
        for candidate in &mut r.candidates {
            candidate.title = "A Concise History of Russia".into();
        }
        r.candidates[0].authors = vec!["Ronald Hingley".into()];
        r.candidates[0].classifications = vec![code("DK41", ClassificationSource::Work)];
        r.candidates[1].authors = vec!["Paul Bushkovitch".into()];
        r.candidates[1].classifications = vec![code("DK37 .B86 2011", ClassificationSource::Work)];
        let verified = crate::identity_evidence::verified_author_prefix_query(&q, &r).unwrap();
        assert_eq!(verified.authors, vec!["Paul Bushkovitch"]);
        assert_eq!(resolved_classifications(&verified, &r)[0].notation, "DK37");
        assert!(r.matched.is_none());
        q.authors = vec!["Known Different Author".into()];
        assert!(crate::identity_evidence::verified_author_prefix_query(&q, &r).is_none());
        q.authors.clear();
        q.title = "Paul Bushkovitch - A Different Book".into();
        assert!(crate::identity_evidence::verified_author_prefix_query(&q, &r).is_none());
        q.title = "Paul Bushkovitch - A Concise History of Russia (Volume 2)".into();
        assert!(crate::identity_evidence::verified_author_prefix_query(&q, &r).is_none());
        q.title = "History Series - A Concise History of Russia".into();
        assert!(crate::identity_evidence::verified_author_prefix_query(&q, &r).is_none());
        q.title = "Paul Bushkovitch - A Concise History of Russia".into();
        r.candidates_truncated = true;
        assert!(crate::identity_evidence::verified_author_prefix_query(&q, &r).is_none());
    }
    #[test]
    fn huntington_prefix_handles_initials_and_filename_subtitle_punctuation() {
        let (mut q, mut r) = fixture();
        q.title = "Samuel P.Huntington - The Soldier and the State_：the Theory and Politics of Civil-Military Relations".into();
        q.authors.clear();
        for candidate in &mut r.candidates {
            candidate.title = "The Soldier and the State".into();
            candidate.subtitle = Some("The Theory and Politics of Civil-Military Relations (Belknap Press)".into());
            candidate.authors = vec!["Samuel P. Huntington".into()];
            candidate.classifications = vec![code("UA23 .H95 1957", ClassificationSource::Work)];
        }
        let verified = crate::identity_evidence::verified_author_prefix_query(&q, &r).unwrap();
        assert_eq!(resolved_classifications(&verified, &r)[0].notation, "UA23");
        assert!(r.matched.is_none());
    }
    #[test]
    fn more_matching_authors_break_ties_but_extra_narrators_do_not() {
        let (mut q, mut r) = fixture();
        q.title = "Art History".into();
        q.authors = vec!["Dana Arnold".into(), "Casey Roe".into()];
        r.candidates[0].classifications = vec![code("N5300", ClassificationSource::ExactEdition)];
        r.candidates[1].classifications = vec![code("N7425", ClassificationSource::ExactEdition)];
        r.candidates[1].authors.push("Casey Roe".into());
        r.candidates[1].authors.push("Narrator Person".into());
        let codes = resolved_classifications(&q, &r);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].notation, "N7425");
        q.authors.pop();
        assert_eq!(resolved_classifications(&q, &r).len(), 2);
        q.authors = vec!["Unrelated Person".into()];
        assert!(resolved_classifications(&q, &r).is_empty());
    }
    #[test]
    fn underscore_space_is_a_subtitle_separator() {
        let evidence = crate::identity_evidence::edition_identity_evidence("Logic_ a very short introduction");
        assert_eq!(evidence.title, "Logic: a very short introduction");
        assert_eq!(evidence.author, None);
        assert_eq!(title_key(&evidence.title), title_key("Logic"));
        assert_eq!(crate::identity_evidence::edition_identity_evidence("knuth_mathematical_writing").title, "knuth mathematical writing");
    }
    #[test]
    fn filename_surname_disambiguates_knuth_without_inventing_an_author() {
        let (mut q, mut r) = fixture();
        q.title = "knuth_mathematical_writing".into();
        q.authors.clear();
        for c in &mut r.candidates {
            c.title = "Mathematical Writing".into();
        }
        r.candidates[0].authors = vec!["Donald Knuth".into()];
        r.candidates[0].classifications = vec![code("QA42 .K59 1989", ClassificationSource::ExactEdition)];
        r.candidates[1].authors = vec!["Franco Vivaldi".into()];
        r.candidates[1].classifications = vec![code("QA1-939", ClassificationSource::ExactEdition)];
        let codes = resolved_classifications(&q, &r);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].notation, "QA42");
        assert_eq!(codes[0].evidence[0].method, "verified_filename_surname_title_classification");
        r.candidates_truncated = true;
        assert!(resolved_classifications(&q, &r).is_empty());
        r.candidates_truncated = false;
        q.authors = vec!["Franco Vivaldi".into()];
        assert!(resolved_classifications(&q, &r).is_empty());
        q.authors.clear();
        r.candidates[1].authors = vec!["Another Knuth".into()];
        assert!(resolved_classifications(&q, &r).is_empty());
        r.candidates.pop();
        for title in ["donald_mathematical_writing", "knuth_different_title", "knuth mathematical writing"] {
            q.title = title.into();
            assert!(resolved_classifications(&q, &r).is_empty(), "{title}");
        }
        q.title = "knuth_mathematical_writing".into();
        r.candidates[0].authors = vec!["Knuth, Donald E.".into()];
        assert!(!resolved_classifications(&q, &r).is_empty());
    }
    #[test]
    fn unique_exact_title_resolves_single_or_mixed_results() {
        let (mut q, mut r) = fixture();
        q.title = "Refactoring UI".into();
        q.authors.clear();
        r.candidates[0].title = q.title.clone();
        r.candidates[1].title = "Refactoring Databases".into();
        assert!(!resolved_classifications(&q, &r).is_empty());
        r.candidates.truncate(1);
        assert!(!resolved_classifications(&q, &r).is_empty());
        r.candidates_truncated = true;
        assert!(resolved_classifications(&q, &r).is_empty());
        r.candidates_truncated = false;
        q.authors = vec!["Steve Schoger".into()];
        assert!(resolved_classifications(&q, &r).is_empty());
        r.candidates[0].authors = vec!["Steve Schoger".into(), "Adam Wathan".into()];
        assert!(!resolved_classifications(&q, &r).is_empty());
        r.candidates[0].classifications.clear();
        assert!(resolved_classifications(&q, &r).is_empty());
    }
    #[test]
    fn duplicate_exact_titles_with_different_subjects_still_need_evidence() {
        let (mut q, mut r) = fixture();
        q.title = "Refactoring UI".into();
        q.authors.clear();
        for c in &mut r.candidates {
            c.title = q.title.clone();
        }
        assert!(resolved_classifications(&q, &r).is_empty());
    }
    #[test]
    fn sutton_middle_initial_expansion_preserves_shared_subject() {
        let (mut q, mut r) = fixture();
        q.title = "Wall Street and the Rise of Hitler".into();
        q.authors = vec!["Antony C. Sutton".into()];
        for (i, c) in r.candidates.iter_mut().enumerate() {
            c.title = q.title.clone();
            c.authors = vec![if i == 0 { "Antony Cyril Sutton" } else { "Antony C. Sutton" }.into()];
            c.classifications = vec![code("HF1456.5.G3 S87", ClassificationSource::ExactEdition)];
        }
        assert!(!resolved_classifications(&q, &r).is_empty());
        r.candidates[1].authors = vec!["Antony Charles Sutton".into()];
        assert!(resolved_classifications(&q, &r).is_empty());
    }
    #[test]
    fn whiteout_exact_author_outweighs_unrelated_title_hit() {
        let (mut q, mut r) = fixture();
        q.title = "Whiteout".into();
        q.authors = vec!["Alexander Cockburn".into()];
        for c in &mut r.candidates {
            c.title = q.title.clone();
        }
        r.candidates[0].authors = q.authors.clone();
        r.candidates[0].classifications = vec![code("HV5825 .C59 1998", ClassificationSource::ExactEdition)];
        r.candidates[1].authors = vec!["Alexander Lowe".into(), "Sebastian Kadlecik".into()];
        r.candidates[1].classifications = vec![code("QC926.37", ClassificationSource::Work)];
        assert_eq!(resolved_classifications(&q, &r)[0].notation, "HV5825");
        r.candidates_truncated = true;
        assert!(resolved_classifications(&q, &r).is_empty());
        r.candidates_truncated = false;
        r.candidates[1].authors = q.authors.clone();
        assert_eq!(resolved_classifications(&q, &r).len(), 2);
    }

    #[test]
    fn lotte_language_edition_note_does_not_block_shared_subject() {
        let (mut q, mut r) = fixture();
        q.title = "Lotte in Weimar: Roman (Fischer Klassik PLUS) (German Edition)".into();
        q.authors = vec!["Mann, Thomas".into(), "Frizen, Werner".into()];
        for (i, c) in r.candidates.iter_mut().enumerate() {
            c.title = "Lotte in Weimar".into();
            c.authors = vec!["Thomas Mann".into()];
            c.classifications = vec![code(if i == 0 { "PT2625.A44 L62 1990" } else { "PT2625.A44" }, ClassificationSource::ExactEdition)];
        }
        assert_eq!(resolved_classifications(&q, &r)[0].notation, "PT2625");
    }
    #[test]
    fn athwart_history_duplicate_records_share_subject() {
        let (mut q, mut r) = fixture();
        q.title = "Athwart History".into();
        q.authors = vec!["Buckley, William F., Jr., Roger Kimball, Linda Bridges".into()];
        for c in &mut r.candidates {
            c.title = "Athwart history".into();
            c.authors = vec!["William F. Buckley".into()];
            c.classifications = vec![code("JC573.2.U6 B83 2010", ClassificationSource::ExactEdition)];
            c.canonical_isbn13 = "9781594033797".into();
        }
        assert!(!classifications(&q, &r).is_empty());
    }

    #[test]
    fn metternich_subtitle_omission_resolves_shared_subject_only() {
        let (mut q, mut r) = fixture();
        q.title = "Metternich Strategist and Visionary".into();
        q.authors = vec!["Wolfram Siemann".into()];
        for c in &mut r.candidates {
            c.title = "Metternich".into();
            c.authors = q.authors.clone();
            c.classifications = vec![code("DB80.8.M5", ClassificationSource::ExactEdition)];
        }
        assert_eq!(classifications(&q, &r).len(), 1);
        assert!(r.matched.is_none());
        assert_ne!(r.candidates[0].canonical_isbn13, r.candidates[1].canonical_isbn13);
        for title in ["Metternich Volume 2", "Metternich: An Adaptation", "Metternich Revised Edition", "Metternichs"] {
            r.candidates[1].title = title.into();
            assert!(classifications(&q, &r).is_empty(), "{title}");
        }
    }

    #[test]
    fn master_of_disguise_combined_credits_resolve_shared_subject() {
        let (mut q, mut r) = fixture();
        q.title = "The Master of Disguise".into();
        for c in &mut r.candidates {
            c.title = q.title.clone();
            c.authors = vec!["Antonio Mendez".into(), "Malcolm McConnell".into()];
            c.classifications = vec![code("UB271.U52", ClassificationSource::ExactEdition)];
        }
        for separator in [" with ", " AND ", " & ", "; "] {
            q.authors = vec![format!("Antonio J. Mendez{separator}Malcolm McConnell")];
            let codes = classifications(&q, &r);
            assert_eq!(codes.len(), 1, "{separator}");
            assert_eq!(codes[0].evidence.len(), 2);
        }
        r.candidates[1].authors = vec!["Antonio Smith".into(), "Malcolm Jones".into()];
        assert!(classifications(&q, &r).is_empty());
        r.candidates[1].authors = vec!["Antonio Q. Mendez".into()];
        assert!(classifications(&q, &r).is_empty());
    }

    #[test]
    fn about_face_accepts_additional_coauthors_but_requires_a_shared_author() {
        let (mut q, mut r) = fixture();
        q.title = "About Face".into();
        q.authors = vec!["Alan Cooper".into()];
        for (i, candidate) in r.candidates.iter_mut().enumerate() {
            candidate.title = "About Face".into();
            candidate.authors = vec!["Alan Cooper".into()];
            candidate.classifications = vec![code(if i == 0 { "QA76.9.U83 C6 1995" } else { "QA76.9.U83 C6596 2014" }, if i == 0 { ClassificationSource::ExactEdition } else { ClassificationSource::Work })];
        }
        r.candidates[1].authors.extend(["Robert Reimann", "David Cronin", "Christopher Noessel"].map(str::to_owned));
        let codes = classifications(&q, &r);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].notation, "QA76.9");
        assert_eq!(codes[0].evidence.len(), 2);
        r.candidates[1].authors.remove(0);
        assert!(classifications(&q, &r).is_empty());
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
    fn art_history_collects_different_codes_with_per_record_provenance() {
        let (q, r) = fixture();
        let codes = classifications(&q, &r);
        assert_eq!(codes.len(), 3);
        assert_eq!(codes[0].notation, "N5300");
        assert_eq!(codes[0].evidence.len(), 2);
        assert_eq!(codes[1].evidence.len(), 1);
        assert_eq!(codes[2].evidence.len(), 1);
        assert_ne!(codes[1].evidence[0].isbn13, codes[2].evidence[0].isbn13);
        assert!(r.matched.is_none());
    }
    #[test]
    fn incomplete_coauthors_can_support_the_same_subject() {
        let (mut q, mut r) = fixture();
        q.authors = vec!["Clive H. Church".into(), "Randolph C. Head".into()];
        r.candidates[0].authors = vec![q.authors[0].clone()];
        r.candidates[1].authors = vec![q.authors[1].clone()];
        assert_eq!(classifications(&q, &r).len(), 3);
    }
    #[test]
    fn unrelated_titles_authors_or_invalid_codes_cannot_vote() {
        for change in 0..7 {
            let (mut q, mut r) = fixture();
            match change {
                0 => r.candidates[1].authors = vec!["Dana Smith".into()],
                1 => r.candidates[1].title = "Art History: Volume 2".into(),
                2 => r.candidates[1].classifications[0].notation = "invalid".into(),
                3 => r.candidates[1].classifications[0].notation = "N5301".into(),
                4 => r.candidates_truncated = true,
                5 => q.authors.clear(),
                _ => r.candidates[1].authors.clear(),
            }
            if matches!(change, 2 | 3) {
                let codes = classifications(&q, &r);
                assert!(!codes.is_empty());
                assert!(codes.iter().all(|c| c.notation != "invalid"));
            } else {
                assert!(classifications(&q, &r).is_empty(), "change {change}");
            }
        }
    }
    #[test]
    fn shared_first_name_is_not_author_identity() {
        let (mut q, mut r) = fixture();
        q.authors = vec!["Fred C. Piper".into(), "Sean Murphy".into()];
        r.candidates[0].authors = vec!["Fred C. Piper".into()];
        r.candidates[1].authors = vec!["Sean-Philip Oriyano".into()];
        assert!(classifications(&q, &r).is_empty());
    }

    #[test]
    fn real_switzerland_title_and_split_coauthors_share_dq54() {
        let (mut q, mut r) = fixture();
        q.title = "A Concise History of Switzerland (Cambridge Concise Histories)".into();
        q.authors = vec!["Clive H. Church".into(), "Randolph C. Head".into()];
        for (i, c) in r.candidates.iter_mut().enumerate() {
            c.title = "A Concise History Of Switzerland".into();
            c.authors = vec![q.authors[i].clone()];
            c.classifications = vec![code("DQ54 .C47 2013", ClassificationSource::ExactEdition)];
        }
        r.candidates[0].classifications.push(code("DQ54", ClassificationSource::ExactEdition));
        let codes = classifications(&q, &r);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].notation, "DQ54");
    }
    #[test]
    fn real_filename_debris_and_cutter_difference_share_hq31() {
        let (mut q, mut r) = fixture();
        q.title = "(by-Anne-Hooper)-Great-Sex-Guide-692141-(z-lib.org)".into();
        q.authors = vec!["Anne Hooper".into()];
        for (i, c) in r.candidates.iter_mut().enumerate() {
            c.title = "Great sex guide".into();
            c.authors = q.authors.clone();
            c.classifications = vec![code(if i == 0 { "HQ31 .H742 1999" } else { "HQ31" }, ClassificationSource::ExactEdition)];
        }
        let codes = classifications(&q, &r);
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].notation, "HQ31");
    }
    #[test]
    fn cleanup_keeps_volumes_but_demotes_parenthesized_editions() {
        let (mut q, mut r) = fixture();
        q.title = "Art History (Volume 2)".into();
        for c in &mut r.candidates {
            c.title = "Art History (Volume 3)".into();
        }
        assert!(classifications(&q, &r).is_empty());
        q.title = "Art History (Revised Edition)".into();
        for c in &mut r.candidates {
            c.title = "Art History".into();
        }
        assert!(!classifications(&q, &r).is_empty());
    }
}
