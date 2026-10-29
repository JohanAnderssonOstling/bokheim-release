//! Controlled subject projection built from source-system parsers and the
//! editable SQLite taxonomy graph.

mod bisac;
mod bisac_database;
mod ddc;
mod lcc_recovery;
mod pruning;
mod runtime;
mod source_headings;
mod taxonomy_sqlite;
use subject_taxonomy::bisac_reference::{normalize_path, normalize_term, split_path};
use subject_taxonomy::{compact_whitespace, lcc, runtime_index, taxonomy_reader};
pub mod installed;

pub use book_model::BookSubject;
pub use lcc_recovery::{LccMatchAssignment, LccMatchEvidence, LccMatchMethod, LccMatchReport};
pub use pruning::redundant_lcc_exact_selectors;
pub use source_headings::{reviewed_source_heading_path, reviewed_source_heading_subject};
use std::collections::{BTreeMap, BTreeSet};

pub use bisac::{bisac_code_for_path, match_bisac_subjects, match_bisac_subjects_with_minimum_adjacent, SubjectAssignment};
pub use bisac_database::{bisac_reference_filename, install_bisac_reference, open_bisac_reference, BISAC_REFERENCE_DIGEST, BISAC_REFERENCE_GZIP_DIGEST};
pub use lcc::{canonical_lcc_notation, lcc_subject_path};
#[cfg(not(target_arch = "wasm32"))]
pub mod bundled;
pub use runtime::{
    install_unified_taxonomy_concepts, lcc_subject_sort_orders, loaded_unified_taxonomy_revision_id, most_specific_concept_ids, unified_concept_id, unified_has_exact_source_code, unified_lcc_has_numeric_coverage, unified_lcc_match_report, unified_subject_paths,
    unified_subject_similarity_keys, unified_subject_sort_order, unified_taxonomy_revision_id, unified_taxonomy_routes, UnifiedConceptDefinition, UnifiedConceptRoute, UnifiedTaxonomy, BUNDLED_UNIFIED_TAXONOMY_REVISION_ID,
};
pub use taxonomy_sqlite::{read_unified_taxonomy_connection, read_unified_taxonomy_sqlite};

pub const BISAC_SYSTEM_ID: &str = "bisac";
pub const BISAC_VERSION: &str = "2021";
pub const DDC_SYSTEM_ID: &str = "ddc";
pub const DDC_MATCHER_VERSION: i64 = 1;
pub use runtime_index::{LCC_SYSTEM_ID, ROOT_SUBJECT_PATH, UNIFIED_VERSION};
pub const UNIFIED_SYSTEM_ID: &str = "unified";
pub const UNIFIED_MATCHER_VERSION: i64 = 149;
pub const EDITION_SUBTREE_CONSENSUS_SOURCE: &str = "metadata:edition-subtree-consensus";
pub const MANUAL_SUBJECT_EXCLUSION_SOURCE: &str = "manual:subject:exclude";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedSubjectCode {
    pub system_id: String,
    pub code: String,
    pub evidence_positions: Vec<usize>,
    /// Explicitly excluded navigation concepts; original classification evidence remains intact.
    pub excluded_concept_ids: Vec<i64>,
}

/// A supported number must add a topic beyond the subclass's fallback routes.
/// Some broad numeric ranges still resolve only to the same parent as the
/// letters alone, so numeric syntax by itself does not establish specificity.
pub fn is_specific_lcc_code(code: &str) -> bool {
    let Some(canonical) = canonical_lcc_notation(code) else { return false };
    if !unified_lcc_has_numeric_coverage(&canonical) {
        return false;
    }
    let letters = canonical.chars().take_while(char::is_ascii_alphabetic).collect::<String>();
    let broad = unified_subject_paths(LCC_SYSTEM_ID, &letters);
    unified_subject_paths(LCC_SYSTEM_ID, &canonical).iter().any(|path| !broad.contains(path))
}

impl ProjectedSubjectCode {
    pub fn system_id(&self) -> &str {
        &self.system_id
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn evidence_positions(&self) -> &[usize] {
        &self.evidence_positions
    }
}

pub fn project_subject_codes(subjects: &[BookSubject]) -> Vec<ProjectedSubjectCode> {
    let excluded = subjects
        .iter()
        .filter(|subject| subject.source() == MANUAL_SUBJECT_EXCLUSION_SOURCE && subject.authority() == Some(UNIFIED_SYSTEM_ID))
        .filter_map(|subject| subject.code().and_then(unified_concept_id))
        .collect::<BTreeSet<_>>();
    let mut projected = BTreeMap::<(String, String), BTreeSet<usize>>::new();
    for assignment in match_bisac_subjects(subjects) {
        let Some(code) = bisac_code_for_path(assignment.node_path()) else { continue };
        let paths = unified_subject_paths(BISAC_SYSTEM_ID, &code);
        if !paths.is_empty() && paths.iter().all(|path| path == "Literature / Genres") {
            if unified_concept_id(source_headings::GENERAL_FICTION_PATH) == Some(source_headings::GENERAL_FICTION_CONCEPT_ID) {
                projected.entry((UNIFIED_SYSTEM_ID.to_owned(), source_headings::GENERAL_FICTION_PATH.to_owned())).or_default().extend(assignment.evidence_positions());
            }
            continue;
        }
        projected.entry((BISAC_SYSTEM_ID.to_owned(), code.to_owned())).or_default().extend(assignment.evidence_positions());
    }
    let preferred_lcc = has_usable_lcc_classification(subjects);
    // DDC is fallback evidence after usable LCC and resolved BISAC. Raw
    // classifications remain in the record when another scheme takes priority.
    let has_bisac = projected.keys().any(|(system, _)| system == BISAC_SYSTEM_ID);
    let ddc = if !preferred_lcc && !has_bisac {
        subjects
            .iter()
            .enumerate()
            .filter_map(|(position, subject)| {
                (subject.authority().and_then(canonical_subject_system)? == DDC_SYSTEM_ID).then(|| ddc::canonical_ddc_notation(subject.code().unwrap_or_else(|| subject.name()))).flatten().map(|code| (position, code))
            })
            .collect::<Vec<_>>()
    } else {
        Default::default()
    };
    let maximum_ddc_specificity = ddc.iter().map(|(_, code)| ddc::specificity(code)).max();
    for (position, code) in ddc {
        if Some(ddc::specificity(&code)) == maximum_ddc_specificity {
            projected.entry((DDC_SYSTEM_ID.to_owned(), code)).or_default().insert(position);
        }
    }
    for (position, subject) in subjects.iter().enumerate() {
        if let Some(path) = (!preferred_lcc).then(|| reviewed_source_heading_path(subject)).flatten() {
            projected.entry((UNIFIED_SYSTEM_ID.to_owned(), path.to_owned())).or_default().insert(position);
            continue;
        }
        if subject.authority() == Some(UNIFIED_SYSTEM_ID) && matches!(subject.source(), "manual:subject" | EDITION_SUBTREE_CONSENSUS_SOURCE) {
            if let Some(path) = subject.code().filter(|path| !path.trim().is_empty()) {
                projected.entry((UNIFIED_SYSTEM_ID.to_owned(), path.to_owned())).or_default().insert(position);
            }
            continue;
        }
        if let Some(code) = subject_lcc_code(subject) {
            projected.entry((LCC_SYSTEM_ID.to_owned(), code)).or_default().insert(position);
        }
    }
    let concepts = projected
        .keys()
        .map(|key| {
            let ids = unified_subject_paths(&key.0, &key.1).iter().filter_map(|path| unified_concept_id(path)).collect::<BTreeSet<_>>();
            (key.clone(), ids)
        })
        .collect::<BTreeMap<_, _>>();
    let retained = most_specific_concept_ids(concepts.values().flatten().copied().filter(|id| !excluded.contains(id)));
    projected.retain(|key, _| concepts[key].is_empty() || !concepts[key].is_disjoint(&retained));
    projected.into_iter().map(|((system_id, code), evidence)| ProjectedSubjectCode { system_id, code, evidence_positions: evidence.into_iter().collect(), excluded_concept_ids: excluded.iter().copied().collect() }).collect()
}

/// Resolve a book's assignments once, retaining source-code indexes for evidence.
/// Only real taxonomy ancestry shadows an assignment; sharing an LCC prefix or
/// being deeper in a different branch is not enough.
pub fn project_subject_concepts(codes: &[ProjectedSubjectCode]) -> Vec<(usize, i64)> {
    let mut concepts = codes
        .iter()
        .enumerate()
        .flat_map(|(index, code)| {
            runtime::resolved_subject_concepts(code.system_id(), code.code())
                .into_iter()
                .map(move |id| (index, id))
                .filter(|(_, id)| !code.excluded_concept_ids.contains(id))
        })
        .collect::<Vec<_>>();
    let retained = most_specific_concept_ids(concepts.iter().map(|(_, concept)| *concept));
    concepts.retain(|(_, concept)| retained.contains(concept));
    concepts
}

/// An explicit or complete embedded LCC classification takes priority over
/// raw heading inference. LCSH subject identifiers are not classifications.
pub fn has_lcc_classification(subjects: &[BookSubject]) -> bool {
    subjects.iter().any(|subject| subject_lcc_code(subject).is_some())
}

/// LCC takes precedence over automatic fallback schemes and heading inference only when
/// it resolves to navigation. Sibling consensus is also LCC evidence, although
/// its supported subtree is stored as a unified path rather than a raw code.
pub fn has_usable_lcc_classification(subjects: &[BookSubject]) -> bool {
    subjects.iter().any(|subject| {
        if subject.authority().is_some_and(|authority| authority.eq_ignore_ascii_case(UNIFIED_SYSTEM_ID))
            && subject.source() == EDITION_SUBTREE_CONSENSUS_SOURCE
        {
            return subject.code().is_some_and(|path| path != ROOT_SUBJECT_PATH && unified_concept_id(path).is_some());
        }
        subject_lcc_code(subject).is_some_and(|code| unified_subject_paths(LCC_SYSTEM_ID, &code).iter().any(|path| path != ROOT_SUBJECT_PATH))
    })
}

/// Infer only complete classification values in otherwise uncontrolled embedded
/// subjects. Keep the original declaration and its evidence position unchanged.
fn subject_lcc_code(subject: &BookSubject) -> Option<String> {
    if let Some(authority) = subject.authority() {
        return (canonical_subject_system(authority)? == LCC_SYSTEM_ID).then(|| canonical_lcc_notation(subject.code().unwrap_or_else(|| subject.name()))).flatten();
    }
    if !subject.source().rsplit(':').next()?.eq_ignore_ascii_case("subject") {
        return None;
    }
    let raw = subject.code().unwrap_or_else(|| subject.name()).trim();
    let raw = raw.split_once(':').filter(|(label, _)| label.trim().eq_ignore_ascii_case("lcc")).map_or(raw, |(_, code)| code.trim());
    let canonical = canonical_lcc_notation(raw)?;
    let call = lcc::parse_lcc_call(&canonical)?;
    // ISBNs, BISAC identifiers and plain alphabetic keywords are not LCC.
    if !"ABCDEFGHJKLMNPQRSTUVZ".contains(call.letters.chars().next()?) || (call.letters.len() > 3 && !call.letters.starts_with('K')) {
        return None;
    }
    let numeric = canonical[call.letters.len()..].trim_start_matches([' ', '\'']);
    if numeric.bytes().take_while(u8::is_ascii_digit).count() > 5 {
        return None;
    }
    Some(canonical)
}

pub(crate) fn canonical_subject_system(authority: &str) -> Option<&'static str> {
    let authority = authority.trim().to_ascii_lowercase();
    if authority.is_empty() {
        return None;
    }
    let compact = authority.chars().filter(|character| character.is_ascii_alphanumeric()).collect::<String>();
    if authority == BISAC_SYSTEM_ID || authority.contains("bisac") {
        Some(BISAC_SYSTEM_ID)
    } else if matches!(authority.as_str(), "ddc" | "dewey" | "dewey decimal classification") || compact.contains("deweydecimal") {
        Some(DDC_SYSTEM_ID)
    } else if matches!(authority.as_str(), "lcc" | "library of congress classification") || compact.contains("libraryofcongressclassification") || authority.contains("authorities/classification") {
        Some(LCC_SYSTEM_ID)
    } else if matches!(authority.as_str(), "lcsh" | "library of congress subject headings") || compact.contains("libraryofcongresssubjectheadings") || authority.contains("authorities/subjects") {
        Some("lcsh")
    } else if matches!(authority.as_str(), "lcgft" | "lc genre" | "lc_genre") || authority.contains("genreform") {
        Some("lcgft")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn explicit_exclusions_filter_individual_routes_and_preserve_source_evidence() {
        let subjects = [
            BookSubject::new(None, "PR6003", "metadata:work", Some("lcc".into()), Some("PR6003".into())).unwrap(),
            BookSubject::new(None, "P", "metadata:work", Some("lcc".into()), Some("P".into())).unwrap(),
            BookSubject::new(None, "English", MANUAL_SUBJECT_EXCLUSION_SOURCE, Some("unified".into()), Some("Literature / By Language / English".into())).unwrap(),
            BookSubject::new(None, "Satire", "manual:subject", Some("unified".into()), Some("Literature / Genres / Humorous Writing / Humorous Fiction / Satire".into())).unwrap(),
        ];
        let codes = project_subject_codes(&subjects);
        let ids = project_subject_concepts(&codes).into_iter().map(|(_, id)| id).collect::<BTreeSet<_>>();
        assert!(!ids.contains(&5585));
        assert!(ids.contains(&4179), "Excluding a broad subject preserves the independently matched English period");
        assert!(ids.contains(&3702), "The independent language classification remains");
        assert!(ids.contains(&4107));
        assert_eq!(subjects[0].code(), Some("PR6003"));
        assert!(codes.iter().any(|code| code.code == "PR6003" && code.evidence_positions == [0]));
        assert!(codes.iter().any(|code| code.code == "P" && code.evidence_positions == [1]));
        let mut all_subjects = subjects.to_vec();
        all_subjects.push(BookSubject::new(None, "Language", MANUAL_SUBJECT_EXCLUSION_SOURCE, Some("unified".into()), Some("Language".into())).unwrap());
        let codes = project_subject_codes(&all_subjects);
        assert_eq!(project_subject_concepts(&codes).into_iter().map(|(_, id)| id).collect::<BTreeSet<_>>(), BTreeSet::from([4107, 4179]), "Control labels must not infer unrelated BISAC topics or remove the specific literary period");
    }

    #[test]
    fn most_specific_navigation_preserves_evidence_and_independent_branches() {
        let subjects = [
            BookSubject::new(None, "PZ3", "catalog:gutenberg.org", Some("lcc".into()), Some("PZ3".into())).unwrap(),
            BookSubject::new(None, "PR4550", "metadata:verified", Some("lcc".into()), Some("PR4550".into())).unwrap(),
            BookSubject::new(None, "DA", "catalog:gutenberg.org", Some("lcc".into()), Some("DA".into())).unwrap(),
        ];
        let codes = project_subject_codes(&subjects);
        assert!(!codes.iter().any(|code| code.code == "PZ3"));
        let concepts = project_subject_concepts(&codes);
        assert!(!concepts.iter().any(|(_, id)| *id == 5585), "English is an ancestor of the retained period");
        assert!(concepts.iter().any(|(i, id)| *id == 3915 && codes[*i].evidence_positions == [1]));
        assert!(concepts.iter().any(|(i, _)| codes[*i].code == "DA"));
        assert_eq!(subjects[0].code(), Some("PZ3"));
        // A shared source prefix must not suppress a different taxonomy branch.
        let computing =
            [BookSubject::new(None, "QA", "catalog:gutenberg.org", Some("lcc".into()), Some("QA".into())).unwrap(), BookSubject::new(None, "QA76.73.R87", "metadata:verified", Some("lcc".into()), Some("QA76.73.R87".into())).unwrap()];
        let codes = project_subject_codes(&computing);
        let concepts = project_subject_concepts(&codes);
        assert!(concepts.iter().any(|(i, _)| codes[*i].code == "QA"));
        assert!(concepts.iter().any(|(i, _)| codes[*i].code == "QA76.73.R87"));
    }

    #[test]
    fn embedded_lcc_detection_rejects_identifiers_prose_and_other_authorities() {
        for value in [
            "9780131103627",
            "ISBN0131103628",
            "ISBN: 9780131103627",
            "9780131103627 The C Programming Language",
            "HIS036060",
            "COM051010",
            "QA",
            "IT",
            "History",
            "Rust programming QA76.73.R87",
            "QA76.73.R87: Rust programming",
            "CPB123",
            "MLCS123",
        ] {
            let subject = BookSubject::new(None, value, "dc:subject", None, None).unwrap();
            assert!(subject_lcc_code(&subject).is_none(), "{value}");
        }
        for authority in ["LCSH", "BISAC", "custom"] {
            let subject = BookSubject::new(None, "QA76.73.R87", "dc:subject", Some(authority.into()), None).unwrap();
            assert!(subject_lcc_code(&subject).is_none(), "{authority}");
        }
    }

    #[test]
    fn apostrophe_calls_keep_cutter_specificity_and_exact_evidence() {
        let definition = |id: i64, code: &str| UnifiedConceptDefinition::new(id, id.to_string(), vec![], BTreeMap::from([("lcc".into(), vec![code.into()])]));
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![definition(1, "HD1..HD999"), definition(2, "HD69.C6"), definition(3, "HD69.C7..HD69.C9")]).unwrap();
        assert_eq!(taxonomy.matching_concept_ids("lcc", "HD'69'C6'1999"), vec![2]);
        assert_eq!(taxonomy.matching_concept_ids("lcc", "HD'69'C8'1999"), vec![3]);
        assert_eq!(taxonomy.matching_concept_ids("lcc", "HD'69'C5'1999"), vec![1]);
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![definition(1, "HD69.C6"), definition(2, "HD'69'C6'1999")]).unwrap();
        assert_eq!(taxonomy.matching_concept_ids("lcc", "HD'69'C6'1999"), vec![2]);
    }

    use super::*;
}
