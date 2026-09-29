//! Conservative storage pruning: reuse existing ranges and base codes without widening coverage.
use crate::lcc::{canonical_lcc_notation, lcc_match_keys, parse_lcc_call};
use crate::{UnifiedConceptDefinition, UnifiedTaxonomy};
use std::collections::{BTreeMap, BTreeSet};

/// Exact selectors whose removal together preserves LCC matching. Retains
/// ambiguous keys, printed labels, and exceptions beneath another exact owner.
/// A surviving broader exact selector can cover its redundant Cutter entries.
/// No new ranges are inferred from sparse observed codes.
pub fn redundant_lcc_exact_selectors(concepts: &[UnifiedConceptDefinition]) -> Result<Vec<(i64, String)>, String> {
    let mut exact = BTreeMap::<String, BTreeSet<i64>>::new();
    let mut range_definitions = Vec::new();
    for concept in concepts {
        let mut ranges = Vec::new();
        for selector in concept.source_selectors("lcc") {
            if selector.ends_with('*') {
                return Err("pruning a taxonomy with LCC prefix selectors requires a separate audit".into());
            }
            if selector.contains("..") || selector.bytes().all(|byte| byte.is_ascii_alphabetic()) {
                ranges.push(selector.clone());
            } else if let Some(keys) = lcc_match_keys(selector) {
                if let Some(key) = keys.last() {
                    exact.entry(key.clone()).or_default().insert(concept.concept_id());
                }
            }
        }
        range_definitions.push(UnifiedConceptDefinition::new(concept.concept_id(), concept.preferred_label().to_owned(), concept.parent_ids().to_vec(), BTreeMap::from([("lcc".into(), ranges)])));
    }
    let ranges = UnifiedTaxonomy::from_concepts(range_definitions)?;
    let mut redundant = Vec::new();
    for concept in concepts {
        for selector in concept.source_selectors("lcc") {
            if selector.contains("..") {
                continue;
            }
            let Some(canonical) = canonical_lcc_notation(selector) else {
                continue;
            };
            if canonical.contains('-') || parse_lcc_call(&canonical).is_none() {
                continue;
            }
            let Some(keys) = lcc_match_keys(&canonical) else {
                continue;
            };
            let Some(key) = keys.last() else {
                continue;
            };
            let owner = BTreeSet::from([concept.concept_id()]);
            if exact.get(key) != Some(&owner) {
                continue;
            }
            // Walk to the nearest exact ancestor. If it is itself removed,
            // the same rule ensures its replacement has this same owner.
            let ancestor = keys[..keys.len() - 1].iter().rev().find_map(|key| exact.get(key));
            let covered = match ancestor {
                Some(owners) => owners == &owner,
                None => ranges.lcc_match_candidates(&canonical) == [concept.concept_id()],
            };
            if covered {
                redundant.push((concept.concept_id(), selector.clone()));
            }
        }
    }
    Ok(redundant)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn concept(id: i64, codes: &[&str]) -> UnifiedConceptDefinition {
        UnifiedConceptDefinition::new(id, format!("Subject {id}"), vec![], BTreeMap::from([("lcc".into(), codes.iter().map(|code| (*code).into()).collect())]))
    }
    #[test]
    fn pruning_keeps_cutters_ambiguity_and_printed_ranges() {
        let definitions = vec![concept(1, &["QA1..QA100", "QA10", "QA10.A1", "QA20.A1.B2", "QA20.A1.B2.C3", "QA30", "QA1-100"]), concept(2, &["QA20", "QA30"])];
        let removed = redundant_lcc_exact_selectors(&definitions).unwrap();
        assert_eq!(removed, [(1, "QA10".into()), (1, "QA10.A1".into()), (1, "QA20.A1.B2.C3".into())]);
        let before = UnifiedTaxonomy::from_concepts(definitions.clone()).unwrap();
        let after = UnifiedTaxonomy::from_concepts(
            definitions
                .into_iter()
                .map(|definition| {
                    let selectors = definition.source_selectors("lcc").iter().filter(|selector| !removed.contains(&(definition.concept_id(), (*selector).clone()))).cloned().collect();
                    UnifiedConceptDefinition::new(definition.concept_id(), definition.preferred_label().into(), vec![], BTreeMap::from([("lcc".into(), selectors)]))
                })
                .collect(),
        )
        .unwrap();
        for code in ["QA10", "QA10.A1", "QA10.A1 .B2 2025", "QA10.Z9", "QA20.A1.B2", "QA20.A1.B2.Z9", "QA20.A1.B2.C3.Z9", "QA20.C3", "QA30", "QA30.A1", "QA1-100", "QA101"] {
            assert_eq!(before.lcc_match_candidates(code), after.lcc_match_candidates(code), "{code}");
        }
    }
}
