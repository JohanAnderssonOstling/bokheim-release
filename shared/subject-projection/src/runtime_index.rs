//! Shared taxonomy index compiler and artifact format, used by build.rs and runtime overlays.
use super::compact_whitespace;
use super::lcc::{lcc_endpoint, lcc_match_keys, parse_lcc_call};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

#[path = "route_index.rs"]
mod route_index;
use route_index::RouteIndex;
#[path = "interval_index.rs"]
mod interval_index;
use interval_index::IntervalIndex;

pub const LCC_SYSTEM_ID: &str = "lcc";
pub const ROOT_SUBJECT_PATH: &str = "Subject";
pub const UNIFIED_VERSION: &str = "1";

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct UnifiedTaxonomyRuntime {
    /// Persisted overlays retain their definitions so another overlay can be
    /// composed in memory. The immutable bundled taxonomy discards this
    /// redundant source representation from its precomputed lookup indexes.
    pub(super) concepts: Option<Arc<Vec<UnifiedConceptDefinition>>>,
    pub(super) routes: Arc<RouteIndex>,
    pub(super) selectors: Arc<BTreeMap<String, SelectorIndex>>,
    pub(super) ancestors: Arc<BTreeMap<i64, BTreeSet<i64>>>,
    pub(super) labels: Arc<BTreeMap<String, BTreeSet<i64>>>,
    pub(super) revision_id: String,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct SelectorIndex {
    #[serde(serialize_with = "serialize_sorted_map")]
    pub(super) exact: HashMap<String, BTreeSet<i64>>,
    pub(super) prefixes: Vec<(String, i64)>,
    pub(super) lcc_ranges: Vec<LccSelector>,
    pub(super) lcc_intervals: IntervalIndex,
    pub(super) lcc_subclasses: BTreeSet<String>,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct LccSelector {
    pub(super) start_letters: String,
    pub(super) start_number: f64,
    pub(super) end_letters: String,
    pub(super) end_number: f64,
    pub(super) start_cutters: Vec<(String, String)>,
    pub(super) end_cutters: Vec<(String, String)>,
    pub(super) concept_id: i64,
}

// Stored schedule boundaries must be fully understood. Book-call recovery
// remains tolerant in lcc.rs, but must never silently truncate stored ranges.
fn stored_lcc_endpoint(value: &str, upper: bool) -> Option<super::lcc::LccEndpoint> {
    lcc_endpoint(value, upper)
}

// Cutter digits are decimal fractions. Compare only as many components as
// the schedule endpoint supplies; later author Cutters and dates are filing
// details within that endpoint, not reasons to exclude its books.
pub(super) fn cutter_components(remainder: &str) -> Vec<(String, String)> {
    let mut rest = remainder;
    let mut result = Vec::new();
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '.');
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        if letters == 0 {
            break;
        }
        let label = rest[..letters].to_ascii_uppercase();
        rest = &rest[letters..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let fraction = rest[..digits].trim_end_matches('0');
        result.push((label, if digits > 0 && fraction.is_empty() { "0".to_owned() } else { fraction.to_owned() }));
        rest = &rest[digits..];
    }
    result
}

pub(super) fn compare_cutter_endpoint(call: &[(String, String)], endpoint: &[(String, String)]) -> std::cmp::Ordering {
    for (index, (letters, digits)) in endpoint.iter().enumerate() {
        let Some((call_letters, call_digits)) = call.get(index) else { return std::cmp::Ordering::Less };
        let order = call_letters.cmp(letters).then_with(|| if digits.is_empty() { std::cmp::Ordering::Equal } else { call_digits.cmp(digits) });
        if !order.is_eq() {
            return order;
        }
    }
    std::cmp::Ordering::Equal
}

// An upper endpoint covers its whole filing family, including omitted deeper
// Cutters. A shorter endpoint is therefore broader, not earlier.
pub(super) fn compare_upper_cutters(inner: &[(String, String)], outer: &[(String, String)]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for ((a, digits_a), (b, digits_b)) in inner.iter().zip(outer) {
        let order = a.cmp(b);
        if !order.is_eq() {
            return order;
        }
        let order = match (digits_a.is_empty(), digits_b.is_empty()) {
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => digits_a.cmp(digits_b),
        };
        if !order.is_eq() {
            return order;
        }
    }
    outer.len().cmp(&inner.len())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedConceptRoute {
    pub(super) concept_id: i64,
    pub(super) path: String,
    pub(super) parent_path: String,
    pub(super) label: String,
    pub(super) sort_order: i64,
}

impl UnifiedConceptRoute {
    pub fn concept_id(&self) -> i64 {
        self.concept_id
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn parent_path(&self) -> &str {
        &self.parent_path
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn sort_order(&self) -> i64 {
        self.sort_order
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedConceptDefinition {
    pub(super) concept_id: i64,
    pub(super) preferred_label: String,
    pub(super) parent_ids: Vec<i64>,
    pub(super) source_selectors: BTreeMap<String, Vec<String>>,
}

impl UnifiedConceptDefinition {
    pub fn new(concept_id: i64, preferred_label: String, parent_ids: Vec<i64>, mut source_selectors: BTreeMap<String, Vec<String>>) -> Self {
        for (system, selectors) in &mut source_selectors {
            if system == LCC_SYSTEM_ID {
                for selector in selectors.iter_mut() {
                    *selector = crate::selector_format::canonical_lcc_selector(selector);
                }
                selectors.sort();
                selectors.dedup();
            } else {
                selectors.sort();
            }
        }
        Self { concept_id, preferred_label, parent_ids, source_selectors }
    }
    pub fn concept_id(&self) -> i64 {
        self.concept_id
    }
    pub fn preferred_label(&self) -> &str {
        &self.preferred_label
    }
    pub fn parent_ids(&self) -> &[i64] {
        &self.parent_ids
    }

    pub fn source_selectors(&self, system_id: &str) -> &[String] {
        self.source_selectors.get(system_id).map(Vec::as_slice).unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LccOrderKey {
    letters: String,
    whole: u64,
    fraction: String,
    cutters: Vec<(String, String)>,
}

fn lcc_order_key(selector: &str) -> Option<LccOrderKey> {
    let selector = selector.trim().to_ascii_uppercase();
    let start = selector.split("..").next()?.split('-').next()?.trim_end_matches('*').trim();
    if !start.is_empty() && start.len() <= 4 && start.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Some(LccOrderKey { letters: start.to_owned(), whole: 0, fraction: String::new(), cutters: vec![] });
    }
    let call = parse_lcc_call(start)?;
    let number = start[..start.len() - call.remainder.len()].trim_start_matches(|c: char| c.is_ascii_alphabetic() || c.is_ascii_whitespace() || c == '\'');
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    Some(LccOrderKey { letters: call.letters, whole: whole.parse().ok()?, fraction: fraction.trim_end_matches('0').to_owned(), cutters: cutter_components(call.remainder) })
}

/// Derived browse order, precomputed for the bundled taxonomy. Equal LCC keys receive the same
/// rank so callers can break ties by label. Subjects without LCC selectors
/// sort alphabetically after classified subjects.
pub fn lcc_subject_sort_orders(concepts: &[UnifiedConceptDefinition]) -> BTreeMap<i64, i64> {
    let keys = concepts.iter().map(|concept| (concept.concept_id, concept.source_selectors(LCC_SYSTEM_ID).iter().filter_map(|selector| lcc_order_key(selector)).min())).collect::<BTreeMap<_, _>>();
    let ranks = keys.values().flatten().cloned().collect::<BTreeSet<_>>().into_iter().enumerate().map(|(rank, key)| (key, rank as i64)).collect::<BTreeMap<_, _>>();
    keys.into_iter().map(|(id, key)| (id, key.map_or(i64::MAX, |key| ranks[&key]))).collect()
}

pub(super) fn normalize_source_label(value: &str) -> String {
    compact_whitespace(value).to_lowercase()
}

pub(super) fn build_runtime(concepts: Vec<UnifiedConceptDefinition>, retain_concepts: bool) -> Result<UnifiedTaxonomyRuntime, String> {
    validate(&concepts)?;
    let mut fingerprint = String::new();
    for concept in &concepts {
        use std::fmt::Write as _;
        write!(&mut fingerprint, "{}\u{1f}{}\u{1f}{}", concept.concept_id, concept.preferred_label, concept.parent_ids.iter().map(i64::to_string).collect::<Vec<_>>().join("|")).unwrap();
        for (system, selectors) in &concept.source_selectors {
            write!(&mut fingerprint, "\u{1f}{system}={}", selectors.join("|")).unwrap();
        }
        writeln!(&mut fingerprint).unwrap();
    }
    let concepts = Arc::new(concepts);
    let routes = Arc::new(RouteIndex::compile(build_routes(&concepts)?));
    let selectors = Arc::new(build_selectors(&concepts)?);
    let parents = concepts.iter().map(|c| (c.concept_id, c.parent_ids.clone())).collect::<BTreeMap<_, _>>();
    fn ancestors_of(id: i64, parents: &BTreeMap<i64, Vec<i64>>, cache: &mut BTreeMap<i64, BTreeSet<i64>>) -> BTreeSet<i64> {
        if let Some(ancestors) = cache.get(&id) {
            return ancestors.clone();
        }
        let mut ancestors = BTreeSet::new();
        for parent in &parents[&id] {
            ancestors.insert(*parent);
            ancestors.extend(ancestors_of(*parent, parents, cache));
        }
        cache.insert(id, ancestors.clone());
        ancestors
    }
    let mut ancestors = BTreeMap::new();
    for id in parents.keys() {
        ancestors_of(*id, &parents, &mut ancestors);
    }
    let ancestors = Arc::new(ancestors);
    let mut labels = BTreeMap::<String, BTreeSet<i64>>::new();
    for concept in concepts.iter() {
        labels.entry(normalize_source_label(&concept.preferred_label)).or_default().insert(concept.concept_id);
    }
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([0xa7, 0xed, 0x9f, 0x7d, 0xa1, 0x2a, 0x59, 0xf5, 0xb5, 0x3d, 0x67, 0x4d, 0xe0, 0x80, 0x7c, 0x66]);
    let revision_id = format!("{UNIFIED_VERSION}:{}", uuid::Uuid::new_v5(&NAMESPACE, fingerprint.as_bytes()));
    let concepts = retain_concepts.then_some(concepts);
    Ok(UnifiedTaxonomyRuntime { concepts, routes, selectors, ancestors, labels: Arc::new(labels), revision_id })
}

pub(super) fn validate(concepts: &[UnifiedConceptDefinition]) -> Result<(), String> {
    if concepts.iter().any(|concept| concept.concept_id <= 0 || concept.preferred_label.trim().is_empty()) {
        return Err("every unified concept requires an ID and preferred label".to_owned());
    }
    let ids = concepts.iter().map(|concept| concept.concept_id).collect::<BTreeSet<_>>();
    if ids.len() != concepts.len() {
        return Err("unified taxonomy concept IDs must be unique".to_owned());
    }
    let labels_by_id = concepts.iter().map(|concept| (concept.concept_id, normalized_concept_label(&concept.preferred_label))).collect::<BTreeMap<_, _>>();
    let mut sibling_labels = BTreeMap::<(i64, String), i64>::new();
    let mut selector_labels = BTreeMap::<(&str, &str, String), i64>::new();
    for concept in concepts {
        for parent in &concept.parent_ids {
            if !ids.contains(parent) {
                return Err(format!("unified concept {} references missing parent {parent}", concept.concept_id));
            }
            let label = labels_by_id.get(&concept.concept_id).expect("concept label exists");
            let parent_label = labels_by_id.get(parent).expect("parent label exists");
            if label == parent_label {
                return Err(format!("unified concept {} duplicates the preferred label of direct parent {parent}", concept.concept_id));
            }
            if let Some(existing) = sibling_labels.insert((*parent, label.clone()), concept.concept_id) {
                if existing != concept.concept_id {
                    return Err(format!("unified concepts {existing} and {} duplicate a preferred label beneath parent {parent}", concept.concept_id));
                }
            }
        }
        let label = labels_by_id.get(&concept.concept_id).expect("concept label exists");
        for (system, selectors) in &concept.source_selectors {
            for selector in selectors {
                // Range overlaps, including equal bounds, are resolved by
                // specificity or exposed as ambiguous by match_candidates.
                if system == LCC_SYSTEM_ID && selector.contains("..") {
                    continue;
                }
                if let Some(existing) = selector_labels.insert((system.as_str(), selector.as_str(), label.clone()), concept.concept_id) {
                    if existing != concept.concept_id {
                        return Err(format!("unified concepts {existing} and {} duplicate source selector {system}:{selector} with the same preferred label", concept.concept_id));
                    }
                }
            }
        }
    }
    let mut reachable = concepts.iter().filter(|concept| concept.parent_ids.is_empty()).map(|concept| concept.concept_id).collect::<BTreeSet<_>>();
    loop {
        let before = reachable.len();
        for concept in concepts {
            if concept.parent_ids.iter().all(|parent| reachable.contains(parent)) {
                reachable.insert(concept.concept_id);
            }
        }
        if reachable.len() == before {
            break;
        }
    }
    if reachable.len() != concepts.len() {
        return Err("unified taxonomy parent graph contains a cycle".to_owned());
    }
    Ok(())
}

fn normalized_concept_label(value: &str) -> String {
    let characters = value.to_lowercase().chars().collect::<Vec<_>>();
    characters
        .iter()
        .enumerate()
        .map(|(index, character)| {
            if character.is_alphanumeric() || matches!(character, '#' | '+') {
                *character
            } else if *character == '.' && characters[..index].iter().all(|previous| previous.is_whitespace()) && characters.get(index + 1).is_some_and(|next| next.is_alphanumeric()) {
                // A leading dot is part of names such as `.NET`; a trailing
                // dot remains decorative punctuation for duplicate checks.
                *character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_routes(concepts: &[UnifiedConceptDefinition]) -> Result<BTreeMap<String, UnifiedConceptRoute>, String> {
    let sort_orders = lcc_subject_sort_orders(concepts);
    let mut child_counts = HashMap::<i64, usize>::new();
    for concept in concepts {
        for parent_id in &concept.parent_ids {
            *child_counts.entry(parent_id.clone()).or_insert(0) += 1;
        }
    }

    let mut routes = BTreeMap::new();
    let mut known = BTreeMap::<i64, Vec<String>>::new();
    let mut routed_concepts = BTreeSet::new();

    let is_root_with_no_children = |concept: &UnifiedConceptDefinition| concept.parent_ids.is_empty() && child_counts.get(&concept.concept_id()).copied().unwrap_or_default() == 0 && concept.source_selectors.values().all(Vec::is_empty);

    for concept in concepts.iter().filter(|concept| concept.parent_ids.is_empty()) {
        if is_root_with_no_children(concept) {
            continue;
        }
        let path = concept.preferred_label.clone();
        routes.insert(
            path.clone(),
            UnifiedConceptRoute { concept_id: concept.concept_id.clone(), path: path.clone(), parent_path: ROOT_SUBJECT_PATH.to_owned(), label: concept.preferred_label.clone(), sort_order: sort_orders[&concept.concept_id] },
        );
        known.entry(concept.concept_id.clone()).or_default().push(path);
        routed_concepts.insert(concept.concept_id.clone());
    }

    loop {
        let mut added = false;
        for concept in concepts.iter().filter(|concept| !concept.parent_ids.is_empty()) {
            if is_root_with_no_children(concept) {
                continue;
            }
            for parent_id in &concept.parent_ids {
                let Some(parent_paths) = known.get(parent_id).cloned() else { continue };
                for parent_path in parent_paths {
                    let path = format!("{parent_path} / {}", concept.preferred_label);
                    if routes.contains_key(&path) {
                        // A multi-parent diamond can discover the same route
                        // more than once. Keep one navigable route while still
                        // recording the concept's projection onto that route.
                        let concept_paths = known.entry(concept.concept_id.clone()).or_default();
                        if !concept_paths.contains(&path) {
                            concept_paths.push(path);
                        }
                        routed_concepts.insert(concept.concept_id.clone());
                        continue;
                    }
                    routes.insert(path.clone(), UnifiedConceptRoute { concept_id: concept.concept_id.clone(), path: path.clone(), parent_path, label: concept.preferred_label.clone(), sort_order: sort_orders[&concept.concept_id] });
                    known.entry(concept.concept_id.clone()).or_default().push(path);
                    routed_concepts.insert(concept.concept_id.clone());
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    let routable_concepts = concepts.iter().filter(|concept| !is_root_with_no_children(concept)).count();
    if routed_concepts.len() != routable_concepts {
        return Err("every unified concept parent must resolve and the graph must be acyclic".to_owned());
    }
    Ok(routes)
}

fn build_selectors(concepts: &[UnifiedConceptDefinition]) -> Result<BTreeMap<String, SelectorIndex>, String> {
    let mut indexes = BTreeMap::<String, SelectorIndex>::new();
    for concept in concepts {
        for (system_id, selectors) in &concept.source_selectors {
            let index = indexes.entry(system_id.clone()).or_default();
            for selector in selectors {
                let selector = selector.trim().to_ascii_uppercase();
                if let Some(prefix) = selector.strip_suffix('*') {
                    if prefix.is_empty() {
                        return Err(format!("unified concept {} has an empty prefix selector", concept.concept_id));
                    }
                    index.prefixes.push((prefix.to_owned(), concept.concept_id.clone()));
                } else if system_id == LCC_SYSTEM_ID && selector.contains("..") {
                    if selector.contains('@') {
                        return Err(format!("invalid legacy LCC range suffix: {selector}"));
                    }
                    let (start, end) = selector.split_once("..").ok_or_else(|| format!("invalid LCC selector {selector}"))?;
                    let start = stored_lcc_endpoint(start, false).ok_or_else(|| format!("invalid LCC range start {start}"))?;
                    let end = stored_lcc_endpoint(end, true).ok_or_else(|| format!("invalid LCC range end {end}"))?;
                    index.lcc_subclasses.insert(start.letters.clone());
                    index.lcc_subclasses.insert(end.letters.clone());
                    index.lcc_ranges.push(LccSelector {
                        start_cutters: cutter_components(&start.remainder),
                        end_cutters: cutter_components(&end.remainder),
                        start_letters: start.letters,
                        start_number: start.number,
                        end_letters: end.letters,
                        end_number: end.number,
                        concept_id: concept.concept_id.clone(),
                    });
                } else {
                    if system_id == LCC_SYSTEM_ID && selector.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                        index.lcc_subclasses.insert(selector.clone());
                    }
                    let selector = if system_id == LCC_SYSTEM_ID { lcc_match_keys(&selector).and_then(|keys| keys.last().cloned()).unwrap_or(selector) } else { selector };
                    index.exact.entry(selector).or_default().insert(concept.concept_id.clone());
                }
            }
        }
    }
    for index in indexes.values_mut() {
        for subclass in super::lcc::outline_subclasses() {
            if index.lcc_ranges.iter().any(|r| r.start_letters <= subclass && subclass <= r.end_letters) {
                index.lcc_subclasses.insert(subclass);
            }
        }
        index.lcc_intervals = IntervalIndex::build(&index.lcc_ranges);
    }
    Ok(indexes)
}

// HashMap iteration is randomized. Keep build artifacts byte-for-byte deterministic.
fn serialize_sorted_map<K: Ord + Serialize, V: Serialize, S: serde::Serializer>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error> {
    map.iter().collect::<BTreeMap<_, _>>().serialize(serializer)
}
