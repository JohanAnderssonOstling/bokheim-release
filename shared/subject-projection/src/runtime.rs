use super::lcc::{canonical_lcc_notation, compare_lcc_key, lcc_match_keys, parse_lcc_call};
use super::runtime_index::{build_runtime, LccSelector};
use super::runtime_index::{compare_cutter_endpoint, compare_upper_cutters, cutter_components, UnifiedTaxonomyRuntime};
pub use super::runtime_index::{lcc_subject_sort_orders, UnifiedConceptDefinition, UnifiedConceptRoute};
use super::{compact_whitespace, DDC_SYSTEM_ID, LCC_SYSTEM_ID};
use crate::LccMatchReport;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

/// Release identifier of the bundled taxonomy image. Keep this cheap constant
/// separate from the runtime so an already-projected library can be opened
/// without materializing the complete taxonomy matcher.
pub const BUNDLED_UNIFIED_TAXONOMY_REVISION_ID: &str = include_str!(concat!(env!("OUT_DIR"), "/unified-taxonomy-revision-id.txt"));

static RUNTIME: OnceLock<RwLock<Arc<UnifiedTaxonomyRuntime>>> = OnceLock::new();

#[derive(Deserialize)]
struct ReviewedPathAliases {
    aliases: Vec<ReviewedPathAlias>,
}

#[derive(Deserialize)]
struct ReviewedPathAlias {
    path: String,
    concept_id: i64,
}

fn reviewed_path_aliases() -> &'static BTreeMap<String, i64> {
    static ALIASES: OnceLock<BTreeMap<String, i64>> = OnceLock::new();
    ALIASES.get_or_init(|| {
        let reviewed: ReviewedPathAliases = serde_json::from_str(include_str!("../data/reviewed-literature-path-aliases.json")).expect("reviewed path aliases must be valid");
        let mut aliases = BTreeMap::new();
        for alias in reviewed.aliases {
            assert!(aliases.insert(alias.path, alias.concept_id).is_none(), "reviewed path aliases must be unique");
        }
        aliases
    })
}

fn concept_for_path(runtime: &UnifiedTaxonomyRuntime, path: &str) -> Option<i64> {
    if let Some(route) = runtime.routes.find(path) {
        return Some(runtime.routes.concept_id(route));
    }
    let identity = *reviewed_path_aliases().get(path)?;
    // A selected/installed taxonomy may omit a subject. Never manufacture
    // assignments to identities absent from the active graph.
    runtime.routes.paths_for_concept(identity).next().map(|_| identity)
}

/// An owned, immutable taxonomy matcher suitable for servers and tools that
/// must not replace the process-wide client taxonomy.
#[derive(Clone)]
pub struct UnifiedTaxonomy {
    runtime: Arc<UnifiedTaxonomyRuntime>,
}

impl UnifiedTaxonomy {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        bincode::serde::encode_to_vec(self.runtime.as_ref(), bincode::config::standard()).map_err(|e| e.to_string())
    }

    pub fn from_concepts(concepts: Vec<UnifiedConceptDefinition>) -> Result<Self, String> {
        Ok(Self { runtime: Arc::new(build_runtime(concepts)?) })
    }

    pub fn revision_id(&self) -> &str {
        &self.runtime.revision_id
    }

    pub fn matching_concept_ids(&self, system_id: &str, code: &str) -> Vec<i64> {
        matching_concepts(&self.runtime, system_id, code).into_iter().collect()
    }

    pub fn most_specific_concept_ids(&self, concepts: impl IntoIterator<Item = i64>) -> BTreeSet<i64> {
        most_specific_concept_ids_in(&self.runtime, concepts)
    }

    /// Returns candidates for one code, or independently resolved subjects for an explicit LCC list.
    pub fn match_candidates(&self, system_id: &str, code: &str) -> Vec<i64> {
        matching_candidates(&self.runtime, system_id, code).into_iter().collect()
    }

    /// Returns candidates for one code, or independently resolved subjects for an explicit list.
    /// Resolved subjects, recovery methods and fragments that still need review.
    pub fn lcc_match_report(&self, code: &str) -> LccMatchReport {
        lcc_match_report(&self.runtime, code)
    }

    /// Unrelated exact-selector conflicts require curation; ancestors are shadowed.
    pub fn lcc_selector_conflicts(&self) -> BTreeMap<String, Vec<i64>> {
        self.runtime
            .selectors
            .get(LCC_SYSTEM_ID)
            .map(|index| {
                index
                    .exact
                    .iter()
                    .filter_map(|(code, ids)| {
                        let shadowed = ids.iter().flat_map(|id| self.runtime.ancestors(*id).iter().copied()).collect::<BTreeSet<_>>();
                        let specific = ids.iter().copied().filter(|id| !shadowed.contains(id)).collect::<Vec<_>>();
                        (specific.len() > 1).then(|| (code.clone(), specific))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn has_exact_source_code(&self, system_id: &str, code: &str) -> bool {
        has_exact_source_code(&self.runtime, system_id, code)
    }
}

pub(super) fn current() -> Arc<UnifiedTaxonomyRuntime> {
    let runtime = RUNTIME.get_or_init(|| RwLock::new(default_runtime()));
    // The protected value is an immutable Arc snapshot. A failed replacement
    // leaves a valid snapshot available for subsequent readers.
    runtime
        .read()
        .unwrap_or_else(|error| {
            runtime.clear_poison();
            error.into_inner()
        })
        .clone()
}

pub fn install_unified_taxonomy_concepts(concepts: Vec<UnifiedConceptDefinition>) -> Result<(), String> {
    let replacement = Arc::new(build_runtime(concepts)?);
    install_runtime(replacement)
}

fn install_runtime(replacement: Arc<UnifiedTaxonomyRuntime>) -> Result<(), String> {
    let runtime = RUNTIME.get_or_init(|| RwLock::new(replacement.clone()));
    *runtime.write().map_err(|_| "unified taxonomy runtime lock is poisoned".to_owned())? = replacement;
    Ok(())
}

pub(crate) fn install_from_connection(db: &Connection) -> Result<(), String> {
    install_runtime(runtime_from_connection(db)?)
}

fn runtime_from_connection(db: &Connection) -> Result<Arc<UnifiedTaxonomyRuntime>, String> {
    let prepared: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='client_matcher')", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if prepared {
        let image: Option<Vec<u8>> = db.query_row("SELECT image FROM client_matcher WHERE singleton=1 AND version=?1", [crate::UNIFIED_MATCHER_VERSION], |row| row.get(0)).optional().map_err(|e| e.to_string())?;
        if let Some(image) = image {
            if let Ok(runtime) = decode_runtime(&image) {
                return Ok(runtime);
            }
        }
    }
    // Older prepared references remain usable. Compile their matcher only
    // when matching is actually requested, on either native or web.
    Ok(Arc::new(build_runtime(crate::read_unified_taxonomy_connection(db, "installed taxonomy")?)?))
}

pub fn unified_taxonomy_revision_id() -> String {
    current().revision_id.clone()
}

/// Returns the active content revision only when a matcher has already been built.
/// This is intentionally non-initializing for startup-sensitive callers.
pub fn loaded_unified_taxonomy_revision_id() -> Option<String> {
    RUNTIME.get().and_then(|runtime| runtime.read().ok().map(|runtime| runtime.revision_id.clone()))
}

pub fn unified_subject_paths(system_id: &str, code: &str) -> Vec<String> {
    let runtime = current();
    if system_id == crate::UNIFIED_SYSTEM_ID {
        if runtime.routes.find(code).is_some() {
            return vec![code.to_owned()];
        }
        return concept_for_path(&runtime, code).into_iter().flat_map(|id| runtime.routes.paths_for_concept(id)).collect();
    }
    matching_concepts(&runtime, system_id, code).into_iter().flat_map(|id| runtime.routes.paths_for_concept(id)).collect::<BTreeSet<_>>().into_iter().collect()
}

pub(super) fn resolved_subject_concepts(system_id: &str, code: &str) -> BTreeSet<i64> {
    let runtime = current();
    if system_id == crate::UNIFIED_SYSTEM_ID {
        return concept_for_path(&runtime, code).into_iter().collect();
    }
    matching_concepts(&runtime, system_id, code)
}

/// Remove only assigned ancestors of other assignments. Sibling branches and
/// unrelated roots remain independent, regardless of their depth or source code.
pub fn most_specific_concept_ids(concepts: impl IntoIterator<Item = i64>) -> BTreeSet<i64> {
    most_specific_concept_ids_in(&current(), concepts)
}

fn most_specific_concept_ids_in(runtime: &UnifiedTaxonomyRuntime, concepts: impl IntoIterator<Item = i64>) -> BTreeSet<i64> {
    let mut concepts = concepts.into_iter().collect::<BTreeSet<_>>();
    let ancestors = concepts.iter().filter_map(|id| runtime.ancestors.binary_search_by_key(id, |(id, _)| *id).ok()).flat_map(|index| runtime.ancestors[index].1.iter().copied()).collect::<BTreeSet<_>>();
    concepts.retain(|id| !ancestors.contains(id));
    concepts
}

/// True only when the installed taxonomy names this precise external code.
/// Falling inside a broader range is deliberately not exact coverage.
pub fn unified_has_exact_source_code(system_id: &str, code: &str) -> bool {
    has_exact_source_code(&current(), system_id, code)
}

fn has_exact_source_code(runtime: &UnifiedTaxonomyRuntime, system_id: &str, code: &str) -> bool {
    let Some(index) = runtime.selectors.get(system_id) else { return false };
    let code = compact_whitespace(code).to_ascii_uppercase();
    let code = if system_id == LCC_SYSTEM_ID { lcc_match_keys(&code).and_then(|keys| keys.last().cloned()).unwrap_or(code) } else { code };
    index.exact.contains_key(&code)
}

/// Returns the nearest stable taxonomy branches used to compare external
/// classifications. The first two levels identify the broad domain; the
/// third distinguishes branches such as Classical Greek & Latin Literatures
/// from Romance-Language Literatures.
pub fn unified_subject_similarity_keys(system_id: &str, code: &str) -> Vec<String> {
    unified_subject_paths(system_id, code)
        .into_iter()
        .filter_map(|path| {
            let components = path.split(" / ").take(3).collect::<Vec<_>>();
            (components.len() == 3).then(|| components.join(" / "))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Returns the ID assigned in the immutable taxonomy; unknown paths have no ID.
pub fn unified_concept_id(path: &str) -> Option<i64> {
    let runtime = current();
    concept_for_path(&runtime, path)
}

pub fn unified_subject_sort_order(path: &str) -> i64 {
    let runtime = current();
    runtime.routes.find(path).map_or(i64::MAX, |id| runtime.routes.sort_order(id))
}

pub fn unified_taxonomy_routes() -> Vec<UnifiedConceptRoute> {
    current().routes.values().collect()
}
/// Whether the class number has numeric coverage in the local taxonomy.
/// A bare letter prefix (or a range spanning different subclasses) cannot
/// validate an extracted number: E3002 must not pass merely because E is known.
/// This is conservative coverage, not proof of an official item assignment.
pub fn unified_lcc_has_numeric_coverage(code: &str) -> bool {
    let Some(canonical) = canonical_lcc_notation(code) else { return false };
    let Some(call) = parse_lcc_call(&canonical) else { return false };
    let runtime = current();
    let Some(index) = runtime.selectors.get(LCC_SYSTEM_ID) else { return false };
    if lcc_match_keys(&canonical).is_some_and(|keys| keys.iter().any(|key| index.exact.contains_key(key) && parse_lcc_call(key).is_some_and(|known| known.number == call.number))) {
        return true;
    }
    index.lcc_ranges.iter().any(|range| range.start_letters == call.letters && range.end_letters == call.letters && range.start_number <= call.number && call.number <= range.end_number)
}

pub fn unified_lcc_match_report(code: &str) -> LccMatchReport {
    lcc_match_report(&current(), code)
}

pub(super) fn lcc_match_report(runtime: &UnifiedTaxonomyRuntime, code: &str) -> LccMatchReport {
    if matching_candidates(runtime, LCC_SYSTEM_ID, code).len() > 1 {
        return LccMatchReport { original: code.to_owned(), assignments: Vec::new(), unresolved_fragments: vec![code.to_owned()] };
    }
    crate::lcc_recovery::report(code, |part| strict_matching_concepts(runtime, LCC_SYSTEM_ID, part).into_iter().collect(), |letters| runtime.selectors.get(LCC_SYSTEM_ID).is_some_and(|index| index.lcc_subclasses.contains(letters)))
}

pub(super) fn matching_concepts(runtime: &UnifiedTaxonomyRuntime, system_id: &str, code: &str) -> BTreeSet<i64> {
    let candidates = matching_candidates(runtime, system_id, code);
    // Conflicting complete selectors require curation; recovery must not
    // silently bypass an existing ambiguity by shortening a valid code.
    match candidates.len() {
        0 if system_id == LCC_SYSTEM_ID => lcc_match_report(runtime, code).assignments.into_iter().map(|a| a.concept_id).collect(),
        0 | 1 => candidates,
        _ => BTreeSet::new(),
    }
}

fn strict_matching_concepts(runtime: &UnifiedTaxonomyRuntime, system_id: &str, code: &str) -> BTreeSet<i64> {
    let candidates = matching_candidates(runtime, system_id, code);
    if candidates.len() > 1 {
        BTreeSet::new()
    } else {
        candidates
    }
}

fn matching_candidates(runtime: &UnifiedTaxonomyRuntime, system_id: &str, code: &str) -> BTreeSet<i64> {
    let Some(index) = runtime.selectors.get(system_id) else { return BTreeSet::new() };
    let canonical = if system_id == LCC_SYSTEM_ID {
        let Some(code) = canonical_lcc_notation(code) else { return BTreeSet::new() };
        code
    } else if system_id == DDC_SYSTEM_ID {
        let Some(code) = super::ddc::canonical_ddc_notation(code) else { return BTreeSet::new() };
        code[..3].to_owned()
    } else {
        code.trim().to_ascii_uppercase()
    };
    let separated = (system_id == LCC_SYSTEM_ID).then(|| super::lcc::apostrophe_call_base(&canonical)).flatten();
    let match_code = if system_id == LCC_SYSTEM_ID {
        // Display separators and item suffixes can occur in the same call.
        // Strip validated item enumeration after separator normalization too,
        // so a volume range cannot be mistaken for a classification range.
        let base = separated.as_deref().unwrap_or(&canonical);
        super::lcc::lcc_item_base(base).unwrap_or(base)
    } else {
        &canonical
    };
    // kind, exact/prefix specificity, single subclass, numeric width
    type Rank<'a> = (u8, usize, bool, f64, Option<&'a LccSelector>);
    let mut matches = Vec::<(i64, Rank<'_>)>::new();
    let keys = if system_id == LCC_SYSTEM_ID { lcc_match_keys(&canonical).unwrap_or_default() } else { vec![canonical.clone()] };
    for (specificity, key) in keys.iter().enumerate() {
        if let Some(ids) = index.exact.get(key) {
            matches.extend(ids.iter().map(|id| (*id, (2, specificity, false, 0.0, None))));
        }
    }

    let call = (system_id == LCC_SYSTEM_ID).then(|| parse_lcc_call(match_code)).flatten();
    let allow_range_matching = system_id != LCC_SYSTEM_ID || (!match_code.contains('-') && call.as_ref().is_some_and(|call| index.lcc_subclasses.contains(&call.letters)));
    if allow_range_matching {
        if let Some(call) = &call {
            let cutters = cutter_components(call.remainder);
            index.lcc_intervals.containing(&index.lcc_ranges, (&call.letters, call.number), (&call.letters, call.number), |range| {
                if compare_lcc_key(&range.start_letters, range.start_number, &call.letters, call.number).is_le()
                    && compare_lcc_key(&call.letters, call.number, &range.end_letters, range.end_number).is_le()
                    && (call.letters != range.start_letters || call.number != range.start_number || compare_cutter_endpoint(&cutters, &range.start_cutters).is_ge())
                    && (call.letters != range.end_letters || call.number != range.end_number || compare_cutter_endpoint(&cutters, &range.end_cutters).is_le())
                {
                    let single = range.start_letters == call.letters && range.end_letters == call.letters;
                    matches.push((range.concept_id, (1, range.start_cutters.len().max(range.end_cutters.len()), single, if single { range.end_number - range.start_number } else { f64::INFINITY }, Some(range))));
                }
            });
        }
        for (prefix, id) in &index.prefixes {
            let accepts = if system_id != LCC_SYSTEM_ID || prefix.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                canonical.starts_with(prefix)
            } else {
                lcc_match_keys(prefix).and_then(|keys| keys.last().cloned()).is_some_and(|key| keys.contains(&key))
            };
            if accepts {
                matches.push((*id, (0, prefix.len(), false, 0.0, None)));
            }
        }
    }
    let shadowed = matches.iter().flat_map(|(id, _)| runtime.ancestors(*id).iter().copied()).collect::<BTreeSet<_>>();
    matches.retain(|(id, _)| !shadowed.contains(id));
    let compare = |left: &Rank<'_>, right: &Rank<'_>| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2)).then_with(|| right.3.total_cmp(&left.3));
    let Some(best) = matches.iter().map(|(_, rank)| rank).max_by(|a, b| compare(a, b)) else { return BTreeSet::new() };
    let tied = matches.iter().filter(|(_, rank)| compare(rank, best).is_eq()).collect::<Vec<_>>();
    // Numeric width alone cannot distinguish nested Cutter intervals at the
    // same class number. Prefer strict containment, retaining genuine ties
    // between equal or merely overlapping intervals.
    tied.iter()
        .filter(|(id, rank)| !tied.iter().any(|(other_id, other_rank)| id != other_id && rank.4.zip(other_rank.4).is_some_and(|(outer, inner)| lcc_range_contains(outer, inner) && !lcc_range_contains(inner, outer))))
        .map(|(id, _)| *id)
        .collect()
}

fn lcc_range_contains(outer: &LccSelector, inner: &LccSelector) -> bool {
    let start = compare_lcc_key(&outer.start_letters, outer.start_number, &inner.start_letters, inner.start_number);
    let end = compare_lcc_key(&inner.end_letters, inner.end_number, &outer.end_letters, outer.end_number);
    start.is_le() && end.is_le() && (!start.is_eq() || compare_cutter_endpoint(&inner.start_cutters, &outer.start_cutters).is_ge()) && (!end.is_eq() || compare_upper_cutters(&inner.end_cutters, &outer.end_cutters).is_le())
}

fn default_runtime() -> Arc<UnifiedTaxonomyRuntime> {
    if let Some(path) = crate::installed::active_path() {
        let db = crate::installed::open_read_only(path).expect("selected taxonomy must be readable");
        return runtime_from_connection(&db).expect("selected taxonomy matcher must be readable");
    }
    bundled_runtime()
}

fn decode_runtime(bytes: &[u8]) -> Result<Arc<UnifiedTaxonomyRuntime>, String> {
    let (runtime, consumed): (UnifiedTaxonomyRuntime, usize) = bincode::serde::decode_from_slice(bytes, bincode::config::standard()).map_err(|e| e.to_string())?;
    if consumed != bytes.len() {
        return Err("taxonomy matcher has trailing bytes".into());
    }
    Ok(Arc::new(runtime))
}

#[cfg(not(target_arch = "wasm32"))]
fn bundled_runtime() -> Arc<UnifiedTaxonomyRuntime> {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/unified-taxonomy-matcher.bin"));
    let runtime = decode_runtime(bytes).expect("bundled taxonomy matcher must be valid");
    assert_eq!(runtime.revision_id, BUNDLED_UNIFIED_TAXONOMY_REVISION_ID, "bundled taxonomy revision constant must track the compiled matcher");
    runtime
}

#[cfg(all(test, not(target_arch = "wasm32")))]
fn default_concepts() -> Vec<UnifiedConceptDefinition> {
    crate::taxonomy_sqlite::read_bundled_unified_taxonomy().expect("bundled unified taxonomy SQLite must be readable")
}

#[cfg(target_arch = "wasm32")]
fn bundled_runtime() -> Arc<UnifiedTaxonomyRuntime> {
    panic!("the browser host selects the taxonomy before using it")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_index::validate;

    #[test]
    fn prepared_matcher_loads_without_reading_concept_tables() {
        let expected = UnifiedTaxonomy::from_concepts(vec![concept(1, "Prepared", &[], &["QA76"])]).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE client_matcher(singleton INTEGER PRIMARY KEY, version INTEGER NOT NULL, image BLOB NOT NULL);").unwrap();
        db.execute("INSERT INTO client_matcher VALUES(1,?1,?2)", rusqlite::params![crate::UNIFIED_MATCHER_VERSION, expected.encode().unwrap()]).unwrap();
        let loaded = runtime_from_connection(&db).unwrap();
        assert_eq!(loaded.as_ref(), expected.runtime.as_ref());
    }

    #[test]
    fn most_specific_assignments_keep_divergent_branches_and_shared_routes() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "Literature", &[], &[]),
            concept(2, "English", &[1], &[]),
            concept(3, "English periods", &[2], &[]),
            concept(4, "Mystery", &[1], &[]),
            concept(5, "History", &[], &[]),
            concept(6, "Shared topic", &[3, 5], &[]),
        ])
        .unwrap();
        assert_eq!(taxonomy.most_specific_concept_ids([1, 2, 3, 4, 5, 3]), BTreeSet::from([3, 4, 5]));
        assert_eq!(taxonomy.most_specific_concept_ids([1, 2, 3, 4, 5, 6]), BTreeSet::from([4, 6]));
        assert_eq!(taxonomy.most_specific_concept_ids([2, 4]), BTreeSet::from([2, 4]));
        assert_eq!(taxonomy.most_specific_concept_ids([2]), BTreeSet::from([2]));
        assert_eq!(taxonomy.most_specific_concept_ids([]), BTreeSet::new());
        // Unknown identities must still reach normal database validation.
        assert_eq!(taxonomy.most_specific_concept_ids([0, 2]), BTreeSet::from([0, 2]));
    }

    #[test]
    fn compact_indexes_preserve_shared_ancestry_and_exact_selector_conflicts() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "Root", &[], &[]),
            concept(2, "Branch", &[1], &["QA10"]),
            concept(3, "Other branch", &[1], &[]),
            concept(4, "Shared leaf", &[2, 3], &["QA10", "QA10.0"]),
            concept(5, "Unrelated", &[], &["QA10"]),
        ])
        .unwrap();
        assert_eq!(taxonomy.runtime.ancestors(4), &[1, 2, 3]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA10"), vec![4, 5]);
        assert_eq!(taxonomy.lcc_selector_conflicts()["QA10"], vec![4, 5]);

        let bytes = bincode::serde::encode_to_vec(taxonomy.runtime.as_ref(), bincode::config::standard()).unwrap();
        let (decoded, consumed): (UnifiedTaxonomyRuntime, usize) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
        assert_eq!(consumed, bytes.len());
        assert_eq!(&decoded, taxonomy.runtime.as_ref());
        let restored = UnifiedTaxonomy { runtime: Arc::new(decoded) };
        assert_eq!(restored.match_candidates(LCC_SYSTEM_ID, "QA10"), vec![4, 5]);
        assert_eq!(restored.lcc_selector_conflicts(), taxonomy.lcc_selector_conflicts());
    }

    #[test]
    fn reviewed_history_ranges_cover_their_complete_boundaries() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(10305, "Eastern campaigns", &[], &["D551..D552.Z"]), concept(54459, "Kazakh political history", &[], &["DK908.67..DK908.68", "DK908.682"])]).unwrap();
        for code in ["D551", "D552.A1", "D552.L5", "D552.T3", "D552.Z9"] {
            assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, code), vec![10305], "{code}");
        }
        for code in ["D550", "D553", "DK908.681", "DK908.69"] {
            assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, code).is_empty(), "{code}");
        }
        for code in ["DK908.67", "DK908.68", "DK908.682"] {
            assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, code), vec![54459], "{code}");
        }
    }

    #[test]
    fn malformed_stored_boundaries_are_rejected_without_truncation() {
        for selector in ["D551..D552.A-Z", "DK908.67..DK908.68.2"] {
            let result = UnifiedTaxonomy::from_concepts(vec![concept(1, "Malformed", &[], &[selector])]);
            assert!(result.err().unwrap().contains("invalid LCC range end"), "{selector}");
        }
    }

    #[test]
    fn every_bundled_range_preserves_structured_boundary_semantics() {
        for concept in default_concepts() {
            for selector in concept.source_selectors(LCC_SYSTEM_ID) {
                let Some((start, end)) = selector.split_once("..") else { continue };
                for (text, upper) in [(start, false), (end, true)] {
                    let parsed = super::super::lcc::lcc_endpoint(text, upper).expect("stored boundary must parse completely");
                    let cutters = cutter_components(&parsed.remainder);
                    let mut formatted = parsed.letters.clone();
                    if parsed.code != parsed.letters {
                        formatted.push_str(&parsed.number.to_string());
                    }
                    for (letters, fraction) in &cutters {
                        formatted.push('.');
                        formatted.push_str(letters);
                        formatted.push_str(fraction);
                    }
                    let restored = super::super::lcc::lcc_endpoint(&formatted, upper).unwrap();
                    assert_eq!(parsed.letters, restored.letters, "{text}");
                    assert_eq!(parsed.number, restored.number, "{text}");
                    assert_eq!(cutters, cutter_components(&restored.remainder), "{text}");
                }
            }
        }
    }

    #[test]
    fn bundled_matcher_equals_runtime_compilation() {
        let rebuilt = build_runtime(default_concepts()).expect("compile bundled definitions");
        let bundled = default_runtime();
        assert_eq!(bundled.as_ref(), &rebuilt);
        // Also verifies deterministic encoding despite randomized HashMap seeds.
        let encoded = bincode::serde::encode_to_vec(&rebuilt, bincode::config::standard()).unwrap();
        assert_eq!(encoded.as_slice(), include_bytes!(concat!(env!("OUT_DIR"), "/unified-taxonomy-matcher.bin")));
    }

    #[cfg(feature = "authoritative-taxonomy")]
    #[test]
    fn bundled_computer_science_calls_do_not_fall_through_to_mathematics() {
        let taxonomy = UnifiedTaxonomy::from_concepts(default_concepts()).unwrap();
        for code in ["QA75.5", "QA75.51", "QA76.76", "QA76.76 .F634 2005", "QA76.94", "QA76.95"] {
            let ids = taxonomy.matching_concept_ids(LCC_SYSTEM_ID, code);
            assert!(!ids.is_empty(), "{code}");
            for id in ids {
                assert!(id == 36 || taxonomy.runtime.ancestors(id).contains(&36), "{code} must resolve under Computing");
                assert!(id != 4916 && !taxonomy.runtime.ancestors(id).contains(&4916), "{code} must not resolve under Mathematics");
            }
        }
        assert_eq!(taxonomy.matching_concept_ids(LCC_SYSTEM_ID, "QA76.76.D47 F634 2005"), vec![168]);
        // The new interval must not take adjacent mathematics classifications.
        for code in ["QA75.49", "QA77", "QA99", "QA699"] {
            let ids = taxonomy.matching_concept_ids(LCC_SYSTEM_ID, code);
            assert!(!ids.is_empty(), "{code}");
            assert!(ids.iter().all(|id| *id == 4916 || taxonomy.runtime.ancestors(*id).contains(&4916)), "{code} must remain under Mathematics");
        }
    }

    #[test]
    fn nested_cutter_intervals_choose_the_more_specific_subject() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "County", &[], &["DA670.C8..DA670.C89"]),
            concept(2, "Hills", &[], &["DA670.C83..DA670.C839"]),
            concept(3, "Local history", &[], &["DS54.95.A..DS54.95.Z"]),
            concept(4, "City", &[], &["DS54.95.L37..DS54.95.L379"]),
        ])
        .unwrap();
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "DA670.C83 S35x"), vec![2]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "DA670.C85 .A1"), vec![1]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "DS54.95.L373 A58 2011"), vec![4]);
    }

    #[test]
    fn cutter_interval_ties_and_partial_overlaps_remain_ambiguous() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "First", &[], &["QA1.C2..QA1.C6"]),
            concept(2, "Second", &[], &["QA1.C4..QA1.C8"]),
            concept(3, "Equal first", &[], &["QA2.C8..QA2.C89"]),
            concept(4, "Equal second", &[], &["QA2.C80..QA2.C890"]),
        ])
        .unwrap();
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA1.C5"), vec![1, 2]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA2.C83"), vec![3, 4]);
    }

    #[test]
    fn equivalent_range_labels_do_not_choose_between_conflicting_subjects() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(17, "Mathematics", &[], &["QA1-939"]), concept(19, "Other", &[], &["QA1-QA939"])]).unwrap();
        assert!(taxonomy.matching_concept_ids("lcc", "QA500").is_empty());
    }

    #[test]
    fn incoming_ranges_require_full_containment_and_keep_individual_specificity() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "Mathematics", &[], &["QA1..QA939"]),
            concept(2, "Computing", &[1], &["QA75.5..QA76.95"]),
            concept(3, "Programming", &[2], &["QA76.6..QA76.66"]),
            concept(4, "Exact topic", &[3], &["QA76.65"]),
        ])
        .unwrap();
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA76.65 .R3 2024"), vec![4]);
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA75.5-76.95").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA76.6-76.66").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA76.65-76.65").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA900-950").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA950-900").is_empty());
    }

    #[test]
    fn incoming_ranges_do_not_bridge_gaps_or_cut_through_cutter_boundaries() {
        let taxonomy =
            UnifiedTaxonomy::from_concepts(vec![concept(1, "Separated coverage", &[], &["QA1..QA4", "QA8..QA10"]), concept(2, "Cutter boundary", &[], &["QB1.A1..QB10.C5"]), concept(3, "Structural subclass", &[], &["QC", "QC*"])]).unwrap();
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA3-9").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QB1-9").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QB2-10").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QB2-9").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QC2-9").is_empty());
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QX2-9").is_empty());
    }

    #[test]
    fn equally_specific_lcc_matches_are_unresolved() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(3, "Management", &[], &["HD69"]), concept(12, "Consultants", &[3], &["HD69.C6"]), concept(23, "Consulting", &[3], &["HD 69 .C6"])]).unwrap();
        assert!(taxonomy.matching_concept_ids("lcc", "HD69.C6 R38").is_empty());
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "HD69.C6 R38"), vec![12, 23]);
        assert_eq!(taxonomy.lcc_selector_conflicts()["HD69.C6"], vec![12, 23]);
    }

    #[test]
    fn specificity_unrelated_ties_are_unresolved_for_every_code_system() {
        let mut definitions = vec![concept(15, "Left", &[], &["QA1..QA10"]), concept(21, "Right", &[], &["QA1..QA10"])];
        for definition in &mut definitions {
            definition.source_selectors.insert("bisac".into(), vec!["COM000000".into()]);
        }
        let taxonomy = UnifiedTaxonomy::from_concepts(definitions).unwrap();
        assert!(taxonomy.matching_concept_ids("lcc", "QA5.A1").is_empty());
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA5.A1"), vec![15, 21]);
        assert!(taxonomy.matching_concept_ids("bisac", "COM000000").is_empty());
    }

    #[test]
    fn specificity_uses_all_parent_paths_and_not_absolute_depth() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "First root", &[], &["QA10"]),
            concept(2, "Second root", &[], &["QA10"]),
            concept(3, "Shared child", &[1, 2], &["QA10"]),
            concept(4, "Other root", &[], &[]),
            concept(5, "Other child", &[4], &[]),
            concept(6, "Unrelated deeper topic", &[5], &["QA10"]),
        ])
        .unwrap();
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "QA10"), vec![3, 6]);
        assert!(taxonomy.matching_concept_ids("lcc", "QA10").is_empty());
    }

    #[test]
    fn specificity_bisac_descendants_win_and_unrelated_ties_remain_diagnostic() {
        let mut definitions = vec![concept(1, "Parent", &[], &[]), concept(2, "Child", &[1], &[])];
        for definition in &mut definitions {
            definition.source_selectors.insert("bisac".into(), vec!["COM000000".into()]);
        }
        let taxonomy = UnifiedTaxonomy::from_concepts(definitions.clone()).unwrap();
        assert_eq!(taxonomy.matching_concept_ids("bisac", "COM000000"), vec![2]);
        let mut unrelated = concept(3, "Unrelated", &[], &[]);
        unrelated.source_selectors.insert("bisac".into(), vec!["COM000000".into()]);
        definitions.push(unrelated);
        let taxonomy = UnifiedTaxonomy::from_concepts(definitions).unwrap();
        assert!(taxonomy.matching_concept_ids("bisac", "COM000000").is_empty());
        assert_eq!(taxonomy.match_candidates("bisac", "COM000000"), vec![2, 3]);
    }

    #[test]
    fn printed_paren_range_spellings_are_rejected() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Comparative law", &[], &["K520..K5582"]), concept(2, "Treaties", &[1], &["K524..K525"])]).unwrap();
        // Parenthesized range spellings are below the coverage threshold: rejected, not expanded.
        assert!(taxonomy.matching_concept_ids("lcc", "K(520) 5582").is_empty());
        assert!(taxonomy.matching_concept_ids("lcc", "K(520)-5582").is_empty());
        assert!(taxonomy.matching_concept_ids("lcc", "K(521) 5582").is_empty());
    }

    #[test]
    fn clipped_tail_recovery_does_not_merge_away_complete_subjects() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Epistemology", &[], &["BD143..BD237"]), concept(2, "Psychology", &[], &["BF1..BF990"]), concept(3, "Logic", &[], &["BC1..BC999"])]).unwrap();
        let report = taxonomy.lcc_match_report("BD143-237BF201Q342BC");
        assert_eq!(report.assignments.iter().map(|a| a.concept_id).collect::<Vec<_>>(), [2], "{report:?}");
        assert_eq!(report.unresolved_fragments, ["BD143-237", "BC"]);
    }

    #[test]
    fn partial_lists_recover_complete_unicode_and_marc_members() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Computing", &[], &["QA75..QA76.99"])]).unwrap();
        for raw in ["QA76; 未分類"] {
            let report = taxonomy.lcc_match_report(raw);
            assert_eq!(report.assignments.iter().map(|a| a.concept_id).collect::<Vec<_>>(), [1], "{report:?}");
            assert!(!report.unresolved_fragments.is_empty());
            assert!(report.assignments[0].evidence.iter().all(|e| raw.contains(&e.code)));
        }
        assert!(taxonomy.lcc_match_report("未分類 QA76").assignments.is_empty());
    }

    fn concept(id: i64, label: &str, parents: &[i64], lcc: &[&str]) -> UnifiedConceptDefinition {
        let source_selectors = if lcc.is_empty() { BTreeMap::new() } else { BTreeMap::from([(LCC_SYSTEM_ID.to_owned(), lcc.iter().map(|value| (*value).to_owned()).collect())]) };
        UnifiedConceptDefinition::new(id.to_owned(), label.to_owned(), parents.iter().map(|value| (*value).to_owned()).collect(), source_selectors)
    }

    #[test]
    fn topic_cutter_ranges_respect_decimal_bounds_and_filing_suffixes() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Germany", &[], &["DD801..DD801"]), concept(2, "Saxony", &[1], &["DD801.S31..DD801.S46"]), concept(3, "Württemberg", &[1], &["DD801.W6..DD801.W82"])]).unwrap();
        for code in ["DD801.S31", "DD801.S4", "DD801.S46 .B33 2007", "DD801.S460 B33 2007"] {
            assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, code), vec![2], "{code}");
        }
        for code in ["DD801", "DD801.A28", "DD801.S3", "DD801.S469", "DD801.W9"] {
            assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, code), vec![1], "{code}");
        }
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "DD801.W7 .B2 2005"), vec![3]);
    }

    #[test]
    fn placeholder_main_class_does_not_supply_a_subclass() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Language and literature", &[], &["(P)"]), concept(2, "Kannada author", &[1], &["PL4659.L33"])]).unwrap();
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "MLCxxx (P)"), vec![1]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "PL4659.L33"), vec![2]);
        assert!(taxonomy.match_candidates(LCC_SYSTEM_ID, "MLCxxx").is_empty());
    }

    #[test]
    fn single_pz_title_letters_preserve_cutter_bounds_and_exact_evidence() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Children's literature", &[], &["PZ7..PZ7"]), concept(2, "Author group", &[1], &["PZ7.S26..PZ7.S27"]), concept(3, "Exact original", &[2], &["PZ7.S268 E"])]).unwrap();
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "PZ7.S268 E"), vec![3]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "PZ7.S268 F"), vec![2]);
        assert_eq!(taxonomy.match_candidates(LCC_SYSTEM_ID, "PZ7.S28 E"), vec![1]);
    }

    #[test]
    fn item_shaped_list_tails_do_not_create_spurious_subjects() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Psychoanalysis", &[], &["BF175..BF175"]), concept(2, "Philosophy", &[], &["B1..B100"])]).unwrap();
        assert_eq!(taxonomy.matching_concept_ids(LCC_SYSTEM_ID, "BF175; B43"), vec![1]);
        assert_eq!(taxonomy.matching_concept_ids(LCC_SYSTEM_ID, "BF175 B43"), vec![1]);
    }

    #[test]
    fn validation_rejects_parallel_taxonomy_structures() {
        let redundant_wrapper = vec![concept(22, "History", &[], &[]), concept(8, "History.", &[22], &[])];
        assert!(validate(&redundant_wrapper).unwrap_err().contains("direct parent"));

        let duplicate_siblings = vec![concept(22, "Humanities", &[], &[]), concept(15, "By period", &[22], &[]), concept(21, "By Period.", &[22], &[])];
        assert!(validate(&duplicate_siblings).unwrap_err().contains("beneath parent"));

        let duplicate_source_record = vec![concept(22, "Humanities", &[], &[]), concept(15, "Ethnography", &[22], &["GN301"]), concept(20, "Social Sciences", &[], &[]), concept(21, "Ethnography.", &[20], &["GN301"])];
        assert!(validate(&duplicate_source_record).unwrap_err().contains("duplicate source selector"));
    }

    #[test]
    fn pz_work_marks_match_author_exceptions_and_numeric_ranges() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Juvenile fiction", &[], &["PZ5..PZ10.5"]), concept(2, "Author collection", &[1], &["PZ7.R79835"])]).unwrap();
        assert_eq!(taxonomy.matching_concept_ids("lcc", "PZ7.R79835 Ham 1999"), vec![2]);
        assert_eq!(taxonomy.matching_concept_ids("lcc", "PZ7.T47 Wh"), vec![1]);
        assert_eq!(taxonomy.matching_concept_ids("lcc", "PZ7.R79835 BOX NO. 4"), vec![2]);
    }

    #[test]
    fn volume_ranges_do_not_become_classification_ranges_or_cutters() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![
            concept(1, "Mathematics", &[], &["QA1..QA999"]),
            concept(2, "Series", &[1], &["QA3.L28"]),
            concept(3, "Bounded author", &[], &["QB1.A1..QB1.C5"]),
            concept(4, "Complete item", &[2], &["QA3.L28 VOL. 1"]),
        ])
        .unwrap();
        for (code, id) in [("QA3 .L28 vol. 1-3", 2), ("QA3.L28 vol. 1", 4), ("QA4.A1 no. 2", 1), ("QB1.C5 vol. 1-3", 3), ("QA3.L28 Suppl. 2", 2)] {
            assert_eq!(taxonomy.matching_concept_ids("lcc", code), vec![id], "{code}");
        }
        assert!(taxonomy.matching_concept_ids("lcc", "QB1.D1 vol. 1").is_empty());
    }

    #[test]
    fn custodial_assignments_do_not_replace_subjects_or_author_matches() {
        let taxonomy = UnifiedTaxonomy::from_concepts(vec![concept(1, "Literature", &[], &["PL2000..PL3000"]), concept(2, "Author", &[1], &["PL2780.F4"])]).unwrap();
        for code in ["PL2780.F4 1981b <Orien China>", "PL2780.F4 vol. 2 <Asian Japan>"] {
            assert_eq!(taxonomy.matching_concept_ids("lcc", code), vec![2]);
        }
        assert_eq!(taxonomy.matching_concept_ids("lcc", "PL2800.A1 <Hebr>"), vec![1]);
        assert_eq!(taxonomy.matching_concept_ids("lcc", "PL2780.F4 <UNKNOWN>"), vec![2]);
    }
}
