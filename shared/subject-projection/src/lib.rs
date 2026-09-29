//! Controlled subject projection built from source-system parsers and the
//! editable SQLite taxonomy graph.

mod bisac;
mod ddc;
mod fast;
mod lcc;
mod lcc_recovery;
mod pruning;
mod range_storage;
mod runtime;
mod runtime_index;
mod selector_format;
mod taxonomy_reader;
mod taxonomy_sqlite;
#[cfg(not(target_arch = "wasm32"))]
pub mod installed;

pub use book_model::BookSubject;
pub use lcc_recovery::{LccMatchAssignment, LccMatchEvidence, LccMatchMethod, LccMatchReport};
pub use pruning::redundant_lcc_exact_selectors;
use std::collections::{BTreeMap, BTreeSet};

pub use bisac::{bisac_code_for_path, bisac_subject_system, match_bisac_subjects, match_bisac_subjects_with_minimum_adjacent, SubjectAssignment, SubjectNode, SubjectSystem};
pub use lcc::{canonical_lcc_notation, lcc_subject_path, lcc_subject_paths};
pub use runtime::{
    install_unified_taxonomy_concepts, install_unified_taxonomy_overlay, lcc_subject_sort_orders, loaded_unified_taxonomy_revision_id, unified_concept_definitions, unified_concept_id, unified_has_exact_source_code,
    unified_lcc_has_numeric_coverage, unified_lcc_match_report, unified_lcc_subject_paths, unified_subject_paths, unified_subject_similarity_keys, unified_subject_sort_order, unified_taxonomy_revision_id, unified_taxonomy_routes,
    UnifiedConceptDefinition, UnifiedConceptRoute, UnifiedTaxonomy, BUNDLED_UNIFIED_TAXONOMY_REVISION_ID, DEFAULT_UNIFIED_TAXONOMY_SQLITE,
};
pub use taxonomy_sqlite::{
    has_unified_taxonomy_overlay, has_unified_taxonomy_overlay_in, is_unified_taxonomy_client_seed, is_unified_taxonomy_client_seed_in, merge_unified_taxonomy_connection, merge_unified_taxonomy_sqlite,
    merge_unified_taxonomy_sqlite_with_resolutions, published_taxonomy_release_id, read_resolved_unified_taxonomy_codes, read_resolved_unified_taxonomy_codes_in, read_unified_taxonomy_connection, read_unified_taxonomy_sqlite,
    reset_unified_taxonomy_connection, unified_taxonomy_server_release_id, unified_taxonomy_server_release_id_in,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnifiedCodeResolution {
    system_id: String,
    code: String,
    matched_concept_ids: Vec<i64>,
}

impl UnifiedCodeResolution {
    pub fn new(system_id: String, code: String, matched_concept_ids: Vec<i64>) -> Self {
        Self { system_id, code, matched_concept_ids }
    }

    pub fn system_id(&self) -> &str {
        &self.system_id
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn matched_concept_ids(&self) -> &[i64] {
        &self.matched_concept_ids
    }
}

pub const BISAC_SYSTEM_ID: &str = "bisac";
pub const BISAC_VERSION: &str = "2021";
pub const BISAC_MATCHER_VERSION: i64 = 1;
pub const DDC_SYSTEM_ID: &str = "ddc";
pub const DDC_MATCHER_VERSION: i64 = 1;
pub use runtime_index::{LCC_SYSTEM_ID, ROOT_SUBJECT_PATH, UNIFIED_VERSION};
pub const LCC_VERSION: &str = "2024-outline-r2";
pub const LCC_MATCHER_VERSION: i64 = 1;
pub const UNIFIED_SYSTEM_ID: &str = "unified";
pub const UNIFIED_MATCHER_VERSION: i64 = 134;
const GENERIC_HISTORY_CONCEPT_ID: i64 = 3319;

/// The original source declaration and the LCC evidence for one unified subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedLccEvidence {
    pub position: usize,
    pub original: String,
    pub concept_id: i64,
    pub evidence: Vec<LccMatchEvidence>,
    pub unresolved_fragments: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedSubjectAssignment {
    pub system_id: String,
    pub subject_path: String,
    pub matcher_version: i64,
    pub evidence_positions: Vec<usize>,
    pub lcc_evidence: Vec<ProjectedLccEvidence>,
}

impl ProjectedSubjectAssignment {
    pub fn system_id(&self) -> &str {
        &self.system_id
    }
    pub fn subject_path(&self) -> &str {
        &self.subject_path
    }
    pub fn matcher_version(&self) -> i64 {
        self.matcher_version
    }
    pub fn evidence_positions(&self) -> &[usize] {
        &self.evidence_positions
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectedSubjectCode {
    pub system_id: String,
    pub code: String,
    pub evidence_positions: Vec<usize>,
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

pub fn project_subjects(subjects: &[BookSubject]) -> Vec<ProjectedSubjectAssignment> {
    let mut projected = BTreeMap::<(String, String, i64), BTreeSet<usize>>::new();
    for assignment in match_bisac_subjects(subjects) {
        projected.entry((BISAC_SYSTEM_ID.to_owned(), assignment.node_path().to_owned(), BISAC_MATCHER_VERSION)).or_default().extend(assignment.evidence_positions());
    }
    for (position, subject) in subjects.iter().enumerate() {
        let Some((system_id, authority_label)) = external_subject_system(subject.authority()) else { continue };
        let notation = subject.code().unwrap_or_else(|| subject.name());
        if system_id == LCC_SYSTEM_ID {
            let paths = lcc_subject_paths(notation);
            if !paths.is_empty() {
                for path in paths {
                    projected.entry((system_id.to_owned(), path, LCC_MATCHER_VERSION)).or_default().insert(position);
                }
                continue;
            }
        }
        let (subject_path, matcher_version) = match system_id {
            LCC_SYSTEM_ID => (lcc_subject_path(notation).unwrap_or_else(|| format!("LCC / Unclassified / {}", compact_whitespace(notation))), LCC_MATCHER_VERSION),
            _ => {
                let components = subject.name().split('/').map(compact_whitespace).filter(|component| !component.is_empty()).collect::<Vec<_>>();
                if components.is_empty() {
                    continue;
                }
                (["LCC".to_owned(), authority_label.to_owned()].into_iter().chain(components).collect::<Vec<_>>().join(" / "), 1)
            }
        };
        projected.entry((system_id.to_owned(), subject_path, matcher_version)).or_default().insert(position);
    }
    projected
        .into_iter()
        .map(|((system_id, subject_path, matcher_version), evidence)| ProjectedSubjectAssignment { system_id, subject_path, matcher_version, evidence_positions: evidence.into_iter().collect(), lcc_evidence: Vec::new() })
        .collect()
}

pub fn project_subject_codes(subjects: &[BookSubject]) -> Vec<ProjectedSubjectCode> {
    let mut projected = BTreeMap::<(String, String), BTreeSet<usize>>::new();
    for assignment in match_bisac_subjects(subjects) {
        let Some(code) = bisac_code_for_path(assignment.node_path()) else { continue };
        projected.entry((BISAC_SYSTEM_ID.to_owned(), code.to_owned())).or_default().extend(assignment.evidence_positions());
    }
    let declared_lcc = subjects.iter().any(|subject| external_subject_system(subject.authority()).is_some_and(|(system, _)| system == LCC_SYSTEM_ID));
    // DDC is a fallback classification. Once the record has an explicit LCC
    // or a resolved BISAC classification, projecting DDC as well creates
    // unrelated subject branches from secondary metadata (for example a
    // mathematics DDC code on a history title). Keep the strongest source
    // only for unified navigation; raw subjects remain unchanged.
    let has_bisac = projected.keys().any(|(system, _)| system == BISAC_SYSTEM_ID);
    let ddc = (!declared_lcc && !has_bisac)
        .then(|| {
            subjects
                .iter()
                .enumerate()
                .filter_map(|(position, subject)| {
                    (external_subject_system(subject.authority())?.0 == DDC_SYSTEM_ID).then(|| ddc::canonical_ddc_notation(subject.code().unwrap_or_else(|| subject.name()))).flatten().map(|code| (position, code))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let maximum_ddc_specificity = ddc.iter().map(|(_, code)| ddc::specificity(code)).max();
    for (position, code) in ddc {
        if Some(ddc::specificity(&code)) == maximum_ddc_specificity {
            projected.entry((DDC_SYSTEM_ID.to_owned(), code)).or_default().insert(position);
        }
    }
    for (position, subject) in subjects.iter().enumerate() {
        if subject.authority() == Some(UNIFIED_SYSTEM_ID) && subject.source() == "manual:subject" {
            if let Some(path) = subject.code().filter(|path| !path.trim().is_empty()) {
                projected.entry((UNIFIED_SYSTEM_ID.to_owned(), path.to_owned())).or_default().insert(position);
            }
            continue;
        }
        if let Some(code) = subject_lcc_code(subject) {
            projected.entry((LCC_SYSTEM_ID.to_owned(), code)).or_default().insert(position);
        }
    }
    projected.into_iter().map(|((system_id, code), evidence)| ProjectedSubjectCode { system_id, code, evidence_positions: evidence.into_iter().collect() }).collect()
}

/// Infer only complete classification values in otherwise uncontrolled embedded
/// subjects. Keep the original declaration and its evidence position unchanged.
fn subject_lcc_code(subject: &BookSubject) -> Option<String> {
    if let Some(authority) = subject.authority() {
        return (external_subject_system(Some(authority))?.0 == LCC_SYSTEM_ID).then(|| canonical_lcc_notation(subject.code().unwrap_or_else(|| subject.name()))).flatten();
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

pub fn project_unified_subjects(subjects: &[BookSubject]) -> Vec<ProjectedSubjectAssignment> {
    let mut projected = BTreeMap::<String, BTreeSet<usize>>::new();
    let runtime = runtime::current();
    for classification in project_subject_codes(subjects) {
        if classification.system_id() == LCC_SYSTEM_ID {
            continue;
        }
        for concept_id in runtime::matching_concepts(&runtime, classification.system_id(), classification.code()) {
            for path in runtime.routes.paths_for_concept(concept_id) {
                projected.entry(path).or_default().extend(classification.evidence_positions());
            }
        }
    }
    let mut lcc_evidence = BTreeMap::<String, Vec<ProjectedLccEvidence>>::new();
    for (position, subject) in subjects.iter().enumerate() {
        if subject.authority() == Some(UNIFIED_SYSTEM_ID) && subject.source() == "manual:subject" {
            if let Some(path) = subject.code().filter(|path| runtime.routes.contains_key(*path)) {
                projected.entry(path.to_owned()).or_default().insert(position);
            }
            continue;
        }
        let canonical = subject_lcc_code(subject);
        let declared = external_subject_system(subject.authority()).is_some_and(|(system, _)| system == LCC_SYSTEM_ID);
        if declared || canonical.is_some() {
            let original = subject.code().unwrap_or_else(|| subject.name());
            // Inference retains the strict boundary for uncontrolled subjects.
            // Explicitly declared LCC also permits recovery of malformed input.
            let input = if declared { original } else { original.split_once(':').filter(|(label, _)| label.trim().eq_ignore_ascii_case("lcc")).map_or(original, |(_, code)| code.trim()) };
            let mut report = runtime::lcc_match_report(&runtime, input);
            if input != original {
                for evidence in report.assignments.iter_mut().flat_map(|a| &mut a.evidence) {
                    if evidence.method == LccMatchMethod::Exact {
                        evidence.method = LccMatchMethod::Normalized;
                    }
                }
            }
            for assignment in report.assignments {
                for path in runtime.routes.paths_for_concept(assignment.concept_id) {
                    projected.entry(path.clone()).or_default().insert(position);
                    lcc_evidence.entry(path).or_default().push(ProjectedLccEvidence {
                        position,
                        original: original.to_owned(),
                        concept_id: assignment.concept_id,
                        evidence: assignment.evidence.clone(),
                        unresolved_fragments: report.unresolved_fragments.clone(),
                    });
                }
            }
            // An explicit classification authority is not an uncontrolled
            // topic label when its code cannot be resolved.
            continue;
        }
        let uncontrolled_dc = subject.source().trim().eq_ignore_ascii_case("dc:subject") && subject.authority().is_none() && subject.code().is_none();
        for normalized in normalized_subject_terms(subject) {
            // Plain Dublin Core terms are uncontrolled keywords. Common labels
            // such as a country or century occur throughout LCC and must not
            // independently activate every concept carrying that caption.
            if uncontrolled_dc && normalized != "history" {
                continue;
            }
            let Some(concepts) = runtime.labels.get(&normalized) else { continue };
            let dc_generic_history = uncontrolled_dc && normalized == "history" && concepts.contains(&GENERIC_HISTORY_CONCEPT_ID);
            let concepts = concepts.iter().filter(|concept_id| !dc_generic_history || **concept_id == GENERIC_HISTORY_CONCEPT_ID);
            for path in concepts.flat_map(|concept_id| runtime.routes.paths_for_concept(*concept_id)) {
                projected.entry(path).or_default().insert(position);
            }
        }
        for path in fast::matching_hierarchy_paths(&runtime, subject) {
            projected.entry(path).or_default().insert(position);
        }
    }
    projected
        .into_iter()
        .map(|(subject_path, evidence)| ProjectedSubjectAssignment {
            system_id: UNIFIED_SYSTEM_ID.to_owned(),
            lcc_evidence: lcc_evidence.remove(&subject_path).unwrap_or_default(),
            subject_path,
            matcher_version: UNIFIED_MATCHER_VERSION,
            evidence_positions: evidence.into_iter().collect(),
        })
        .collect()
}

fn normalized_subject_terms(subject: &BookSubject) -> BTreeSet<String> {
    let mut labels = BTreeSet::from([runtime::normalize_source_label(subject.name())]);
    let source = subject.source().trim().to_ascii_lowercase();
    let authority = subject.authority().map(str::trim).unwrap_or_default().to_ascii_lowercase();
    let delimiter = if source == "dc:subject" && authority.is_empty() && subject.code().is_none() {
        Some(";")
    } else if matches!(authority.as_str(), "lcgft" | "lc genre" | "lc_genre") || source.ends_with("/#lc_genre") {
        Some(">")
    } else {
        None
    };
    if let Some(delimiter) = delimiter {
        labels.extend(subject.name().split(delimiter).map(runtime::normalize_source_label).filter(|label| !label.is_empty()));
    }
    labels.retain(|label| !label.is_empty());
    labels
}

fn external_subject_system(authority: Option<&str>) -> Option<(&'static str, &'static str)> {
    match canonical_subject_system(authority?)? {
        "lcsh" => Some(("lcsh", "LCSH")),
        "lcgft" => Some(("lcgft", "LCGFT")),
        "lcc" => Some((LCC_SYSTEM_ID, "LCC")),
        "ddc" => Some((DDC_SYSTEM_ID, "DDC")),
        _ => None,
    }
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

fn split_path(value: &str) -> Vec<String> {
    value.replace("::", "/").replace("--", "/").replace(['>', '|', '›', '→'], "/").split('/').map(compact_whitespace).filter(|component| !component.is_empty()).collect()
}
fn normalize_path(value: &str) -> String {
    split_path(value).iter().map(|component| normalize_term(component)).collect::<Vec<_>>().join("/")
}
fn normalize_term(value: &str) -> String {
    compact_whitespace(value).to_lowercase()
}
fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn unresolved_declared_lcc_does_not_fall_through_to_topic_labels() {
        for raw in ["IN PROCESS", "CPB Box no. 7", "WB 18.2", "WY 18.2", "LAW", "History", "Internet"] {
            let subject = super::BookSubject::new(None, raw, "marc", Some("lcc".into()), Some(raw.into())).unwrap();
            assert!(super::project_unified_subjects(&[subject]).is_empty(), "{raw}");
        }
    }

    #[test]
    fn inferred_lcc_evidence_preserves_source_spelling_and_normalization() {
        let raw = ["qa76.73.r87", "LCC: QA76.73.R87"];
        let subjects = raw.iter().map(|s| super::BookSubject::new(None, *s, "dc:subject", None, None).unwrap()).collect::<Vec<_>>();
        let assignments = super::project_unified_subjects(&subjects);
        assert!(!assignments.is_empty());
        for assignment in assignments {
            assert_eq!(assignment.evidence_positions, [0, 1]);
            assert_eq!(assignment.lcc_evidence.len(), 2);
            for evidence in assignment.lcc_evidence {
                assert_eq!(evidence.original, raw[evidence.position]);
                assert!(evidence.evidence.iter().all(|e| e.method == super::LccMatchMethod::Normalized));
                assert!(evidence.unresolved_fragments.is_empty());
            }
        }
    }

    #[test]
    fn declared_damaged_lcc_reaches_unified_projection_with_original_position() {
        for raw in ["QA76; UNKNOWN", "QA76.76 .063"] {
            let subject = super::BookSubject::new(None, raw, "marc", Some("lcc".into()), Some(raw.into())).unwrap();
            let expected = super::unified_lcc_subject_paths(raw);
            assert!(!expected.is_empty(), "{raw}");
            let actual = super::project_unified_subjects(&[subject.clone()]);
            assert_eq!(actual.iter().map(|a| a.subject_path.clone()).collect::<std::collections::BTreeSet<_>>(), expected.into_iter().collect());
            assert!(actual.iter().all(|a| a.evidence_positions == [0]));
            assert_eq!(subject.code(), Some(raw));
            let uncontrolled = super::BookSubject::new(None, raw, "dc:subject", None, None).unwrap();
            assert!(super::project_unified_subjects(&[uncontrolled]).is_empty(), "{raw}");
        }
    }

    #[test]
    fn truncated_lcc_remains_unresolved_and_preserves_original_metadata() {
        let raw = "QA76.";
        let subject = BookSubject::new(None, raw, "marc", Some("lcc".into()), Some(raw.into())).unwrap();
        let report = unified_lcc_match_report(raw);
        assert!(report.assignments.is_empty());
        assert_eq!(report.original, raw);
        assert_eq!(report.unresolved_fragments, [raw]);
        assert!(unified_lcc_subject_paths(raw).is_empty());
        assert!(project_unified_subjects(&[subject.clone()]).is_empty());
        assert_eq!(subject.code(), Some(raw));
        assert_eq!(subject.name(), raw);
        let uncontrolled = BookSubject::new(None, raw, "dc:subject", None, None).unwrap();
        assert!(project_unified_subjects(&[uncontrolled]).is_empty());
    }

    #[test]
    fn embedded_lcc_values_project_without_changing_raw_metadata() {
        let values = ["QA76.73.R87", "LCC: HD69.C6 R38 1999", "QA75.5-76.95", "qa76.73.r87"];
        let subjects = values.iter().map(|value| BookSubject::new(None, *value, "dc:subject", None, None).unwrap()).collect::<Vec<_>>();
        let codes = project_subject_codes(&subjects).into_iter().filter(|code| code.system_id() == "lcc").collect::<Vec<_>>();
        assert_eq!(codes.len(), 3);
        let rust = codes.iter().find(|code| code.code() == "QA76.73.R87").unwrap();
        assert_eq!(rust.evidence_positions(), &[0, 3]);
        assert!(codes.iter().any(|code| code.code() == "HD69.C6 R38 1999"));
        assert!(codes.iter().any(|code| code.code() == "QA75.5-76.95"));
        for (subject, value) in subjects.iter().zip(values) {
            assert_eq!(subject.name(), value);
            assert!(subject.authority().is_none());
            assert!(subject.code().is_none());
        }
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

    #[test]
    fn embedded_declared_lcc_terms_use_the_same_single_subject_matcher() {
        let subjects = vec![BookSubject::new(None, "Business consultants", "dc:subject", Some("LCC".into()), Some("HD69.C6 R38 1999".into())).unwrap(), BookSubject::new(None, "HD69.C6 R38 1999", "subject", None, None).unwrap()];
        let codes = project_subject_codes(&subjects).into_iter().filter(|code| code.system_id() == "lcc").collect::<Vec<_>>();
        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].evidence_positions(), &[0, 1]);
        let definition = |id: i64, code: &str| UnifiedConceptDefinition::new(id, id.to_string(), vec![], BTreeMap::from([("lcc".into(), vec![code.into()])]));
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![definition(1, "HD69"), definition(2, "HD69.C6")]).unwrap();
        assert_eq!(taxonomy.matching_concept_ids("lcc", codes[0].code()), vec![2]);
    }

    use super::*;

    #[test]
    fn plain_dc_bisac_headings_resolve_through_codes_before_unified_projection() {
        runtime::install_authoritative_taxonomy_for_tests();
        for (heading, expected_code) in
            [("Fiction", "FIC000000"), ("History", "HIS000000"), ("Juvenile Fiction", "JUV000000"), ("Drama", "DRA000000"), ("Poetry", "POE000000"), ("Science", "SCI000000"), ("Fiction / Short Stories", "FIC029000")]
        {
            let subject = BookSubject::new(None, heading, "dc:subject", None, None).unwrap();
            let codes = project_subject_codes(std::slice::from_ref(&subject));
            assert!(codes.iter().any(|code| code.system_id() == BISAC_SYSTEM_ID && code.code() == expected_code), "{heading} must resolve to {expected_code}");

            let expected_paths = unified_subject_paths(BISAC_SYSTEM_ID, expected_code).into_iter().collect::<BTreeSet<_>>();
            assert!(!expected_paths.is_empty(), "{expected_code} must exist in the unified taxonomy");
            let projected_paths = project_unified_subjects(&[subject]).into_iter().map(|assignment| assignment.subject_path().to_owned()).collect::<BTreeSet<_>>();
            assert!(expected_paths.is_subset(&projected_paths), "{heading} must use its BISAC code's unified routes");
        }
    }

    #[test]
    fn source_system_uris_resolve_codes_and_unknown_systems_fall_back_to_bisac_paths() {
        let bisac = BookSubject::new(None, "History / United States / 20th Century", "test:subject", Some("https://vendor.example/subjects".to_owned()), None).unwrap();
        let bisac_codes = project_subject_codes(&[bisac]);
        assert!(bisac_codes.iter().any(|code| code.system_id() == BISAC_SYSTEM_ID && code.code() == "HIS036060"));
    }

    #[test]
    fn ddc_is_fallback_when_bisac_already_resolves_the_subject() {
        runtime::install_authoritative_taxonomy_for_tests();
        let subjects = vec![
            BookSubject::new(None, "History / United States / 20th Century", "dc:subject", Some("bisac".to_owned()), Some("HIS036060".to_owned())).unwrap(),
            BookSubject::new(None, "515.33", "openlibrary", Some("ddc".to_owned()), Some("515.33".to_owned())).unwrap(),
        ];

        let codes = project_subject_codes(&subjects);
        assert!(codes.iter().any(|code| code.system_id() == BISAC_SYSTEM_ID));
        assert!(codes.iter().all(|code| code.system_id() != DDC_SYSTEM_ID));
    }

    #[test]
    fn bisac_components_in_separate_dc_subjects_keep_combined_evidence_in_unified_projection() {
        runtime::install_authoritative_taxonomy_for_tests();
        let subjects = ["Fiction", "Short Stories"].into_iter().map(|heading| BookSubject::new(None, heading, "dc:subject", None, None).unwrap()).collect::<Vec<_>>();
        let bisac = match_bisac_subjects(&subjects);
        let short_stories = bisac.iter().find(|assignment| assignment.node_path() == "Fiction / Short Stories").expect("production subject components must infer the BISAC path");
        assert_eq!(short_stories.evidence_positions(), &[0, 1]);

        let expected = unified_subject_paths(BISAC_SYSTEM_ID, "FIC029000").into_iter().collect::<BTreeSet<_>>();
        let unified = project_unified_subjects(&subjects);
        for path in expected {
            let assignment = unified.iter().find(|assignment| assignment.subject_path() == path).expect("the inferred BISAC code must resolve into unified taxonomy");
            assert_eq!(assignment.evidence_positions(), &[0, 1]);
        }
    }

    #[test]
    fn lcsh_subjects_require_a_complete_adjacent_bisac_hierarchy() {
        for heading in ["History -- Great Britain", "Great Britain -- History -- Victoria, 1837-1901 -- Fiction", "Clothing and dress -- United States -- History"] {
            let subject = BookSubject::new(None, heading, "dc:subject", None, None).unwrap();
            let codes = project_subject_codes(&[subject]);
            assert!(codes.iter().all(|code| code.code() != "HIS015000"), "incomplete or reordered LCSH heading {heading} produced Great Britain history: {codes:?}");
        }

        let complete = BookSubject::new(None, "History -- Europe -- Great Britain", "marc", Some("lcsh".to_owned()), None).unwrap();
        assert!(project_subject_codes(&[complete]).iter().any(|code| code.system_id() == BISAC_SYSTEM_ID && code.code() == "HIS015000"));

        let fiction = BookSubject::new(None, "Fiction -- Short Stories", "dc:subject", None, None).unwrap();
        assert!(project_subject_codes(&[fiction]).iter().any(|code| code.system_id() == BISAC_SYSTEM_ID && code.code() == "FIC029000"));
    }

    #[test]
    fn bisac_hierarchy_inference_requires_every_component() {
        let partial = ["History", "Great Britain"].into_iter().map(|heading| BookSubject::new(None, heading, "dc:subject", None, None).unwrap()).collect::<Vec<_>>();
        assert!(project_subject_codes(&partial).iter().all(|code| code.code() != "HIS015000"), "a missing Europe component must not infer Great Britain history");

        let complete = ["History", "Europe", "Great Britain"].into_iter().map(|heading| BookSubject::new(None, heading, "dc:subject", None, None).unwrap()).collect::<Vec<_>>();
        assert!(project_subject_codes(&complete).iter().any(|code| code.system_id() == BISAC_SYSTEM_ID && code.code() == "HIS015000"), "the complete BISAC hierarchy should infer Great Britain history");
    }

    #[test]
    fn fine_lcc_identifiers_are_recorded_independently_of_local_mapping_detail() {
        let subject = BookSubject::new(None, "Regional law detail", "marc", Some("lcc".to_owned()), Some("kjcx 12.4 .a2".to_owned())).unwrap();
        let codes = project_subject_codes(&[subject]);

        assert_eq!(codes.len(), 1);
        assert_eq!(codes[0].system_id(), LCC_SYSTEM_ID);
        assert_eq!(codes[0].code(), "KJCX 12.4 .A2");
    }

    #[test]
    fn declared_lcc_suppresses_ddc_for_the_same_book() {
        let subjects =
            vec![BookSubject::new(None, "Computer science", "marc", Some("ddc".to_owned()), Some("004".to_owned())).unwrap(), BookSubject::new(None, "Computer science", "marc", Some("lcc".to_owned()), Some("QA76".to_owned())).unwrap()];
        let codes = project_subject_codes(&subjects);
        assert!(codes.iter().any(|code| code.system_id() == LCC_SYSTEM_ID && code.code() == "QA76"));
        assert!(codes.iter().all(|code| code.system_id() != DDC_SYSTEM_ID));
    }

    #[test]
    fn only_the_most_specific_declared_ddc_values_are_projected() {
        let subjects = [("500", "Science"), ("512.7", "Algebra"), ("512.73", "Algebra"), ("519.20", "Probability")]
            .into_iter()
            .map(|(code, name)| BookSubject::new(None, name, "marc", Some("ddc".to_owned()), Some(code.to_owned())).unwrap())
            .collect::<Vec<_>>();
        let codes = project_subject_codes(&subjects).into_iter().filter(|code| code.system_id() == DDC_SYSTEM_ID).collect::<Vec<_>>();
        assert_eq!(codes.iter().map(ProjectedSubjectCode::code).collect::<Vec<_>>(), ["512.73", "519.20"]);
        assert_eq!(codes[0].evidence_positions(), &[2]);
        assert_eq!(codes[1].evidence_positions(), &[3]);
    }

}
