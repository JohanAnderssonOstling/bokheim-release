//! Evidence-bearing recovery of complete classifications from damaged fields.
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LccMatchMethod {
    Exact,
    Normalized,
    RecoveredComponent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LccMatchEvidence {
    pub method: LccMatchMethod,
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LccMatchAssignment {
    pub concept_id: i64,
    pub evidence: Vec<LccMatchEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LccMatchReport {
    pub original: String,
    pub assignments: Vec<LccMatchAssignment>,
    pub unresolved_fragments: Vec<String>,
}

pub(super) fn report(original: &str, strict: impl Fn(&str) -> Vec<i64>, known_subclass: impl Fn(&str) -> bool) -> LccMatchReport {
    let mut result = LccMatchReport { original: original.to_owned(), assignments: Vec::new(), unresolved_fragments: Vec::new() };
    let exact = strict(original);
    if !exact.is_empty() {
        let canonical = crate::canonical_lcc_notation(original);
        let method = if canonical.as_deref() == Some(original.trim()) && canonical.as_deref().is_some_and(|s| crate::lcc::lcc_item_base(s).is_none()) { LccMatchMethod::Exact } else { LccMatchMethod::Normalized };
        result.assignments = exact.into_iter().map(|concept_id| LccMatchAssignment { concept_id, evidence: vec![LccMatchEvidence { method, code: original.to_owned() }] }).collect();
        return result;
    }
    // Keep recovery bounded. Individual list members may use the strict
    // parser's Unicode/MARC normalization; joined boundary scanning is ASCII.
    if original.len() > 512 {
        result.unresolved_fragments.push(original.to_owned());
        return result;
    }
    let is_start = |value: &str| {
        let letters = value.bytes().take_while(u8::is_ascii_alphabetic).count();
        letters > 0 && letters <= 4 && known_subclass(&value[..letters]) && value[letters..].trim_start().as_bytes().first().is_some_and(u8::is_ascii_digit)
    };
    let mut assignments: BTreeMap<i64, Vec<LccMatchEvidence>> = BTreeMap::new();
    for (part_index, raw) in original.split([';', '|', '\n']).enumerate() {
        let segment = raw.trim();
        if segment.is_empty() {
            continue;
        }
        let upper = segment.to_ascii_uppercase();
        let initial_letters = upper.bytes().take_while(u8::is_ascii_alphabetic).count();
        if (part_index > 0 && initial_letters == 1 && !upper.contains(['.', '-'])) || upper.split_whitespace().any(|word| matches!(word, "DATE" | "YEAR")) {
            result.unresolved_fragments.push(segment.to_owned());
            continue;
        }
        if !segment.is_ascii() || !is_start(&upper) {
            let ids = strict(segment);
            if !ids.is_empty() {
                for id in ids {
                    let evidence = LccMatchEvidence { method: LccMatchMethod::RecoveredComponent, code: segment.to_owned() };
                    let entries = assignments.entry(id).or_default();
                    if !entries.contains(&evidence) {
                        entries.push(evidence);
                    }
                }
                continue;
            }
        }
        if !segment.is_ascii() {
            result.unresolved_fragments.push(segment.to_owned());
            continue;
        }
        // A processing marker or another system cannot be turned into LCC by
        // searching arbitrary prose for an embedded class-looking token.
        if !is_start(&upper) {
            result.unresolved_fragments.push(segment.to_owned());
            continue;
        }
        let mut boundaries = vec![0];
        for at in 1..upper.len() {
            if !upper.as_bytes()[at].is_ascii_uppercase() || !(upper.as_bytes()[at - 1].is_ascii_digit() || upper.as_bytes()[at - 1] == b'.') {
                continue;
            }
            let letters = upper[at..].bytes().take_while(u8::is_ascii_alphabetic).count();
            // Undotted single-letter suffixes can be item Cutters. Preserve
            // that ambiguity until a separate boundary can establish a call.
            let range_boundary = letters == 1 && upper[..at].contains('-') && upper[at..].contains('-');
            let terminal_class = letters >= 2 && at + letters == upper.len() && known_subclass(&upper[at..]);
            if terminal_class || ((letters >= 2 || range_boundary) && is_start(&upper[at..])) {
                boundaries.push(at);
            }
        }
        boundaries.push(segment.len());
        let mut cursor = 0;
        while cursor + 1 < boundaries.len() {
            let start = boundaries[cursor];
            // A trailing bare subclass in a damaged joined field may be the
            // beginning of a clipped call. Keep it as a fragment, not a book's
            // independent broad classification.
            if cursor > 0 && upper[start..].bytes().all(|b| b.is_ascii_alphabetic()) && known_subclass(&upper[start..]) {
                result.unresolved_fragments.push(segment[start..].to_owned());
                cursor += 1;
                continue;
            }
            let mut recovered = None;
            // Once whole-field matching has failed, preserve independently
            // complete components. A longer accepted prefix can merely be a
            // broad interpretation that hides another valid classification.
            // Try a later boundary only if the first cut is incomplete (for
            // example a dot preceding a multi-letter Cutter).
            for next in cursor + 1..boundaries.len() {
                let candidate = segment[start..boundaries[next]].trim();
                let ids = strict(candidate);
                if !ids.is_empty() {
                    recovered = Some((next, candidate, ids));
                    break;
                }
            }
            if let Some((next, candidate, ids)) = recovered {
                for id in ids {
                    let evidence = LccMatchEvidence { method: LccMatchMethod::RecoveredComponent, code: candidate.to_owned() };
                    let entries = assignments.entry(id).or_default();
                    if !entries.contains(&evidence) {
                        entries.push(evidence);
                    }
                }
                cursor = next;
                continue;
            }
            let fragment = segment[start..boundaries[cursor + 1]].trim();
            // Unknown trailing text may follow a complete leading call. Only
            // whitespace establishes this boundary; never trim missing digits.
            let mut leading = None;
            for (at, _) in fragment.match_indices(char::is_whitespace).rev() {
                let candidate = fragment[..at].trim_end();
                if candidate.to_ascii_uppercase().ends_with(" BOX") {
                    continue;
                }
                let ids = strict(candidate);
                if !ids.is_empty() {
                    leading = Some((at, candidate, ids));
                    break;
                }
            }
            if let Some((at, candidate, ids)) = leading {
                for id in ids {
                    let evidence = LccMatchEvidence { method: LccMatchMethod::RecoveredComponent, code: candidate.to_owned() };
                    let entries = assignments.entry(id).or_default();
                    if !entries.contains(&evidence) {
                        entries.push(evidence);
                    }
                }
                result.unresolved_fragments.push(fragment[at..].trim().to_owned());
            } else {
                result.unresolved_fragments.push(fragment.to_owned());
            }
            cursor += 1;
        }
    }
    result.assignments = assignments.into_iter().map(|(concept_id, evidence)| LccMatchAssignment { concept_id, evidence }).collect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resolve(code: &str) -> Vec<i64> {
        match code {
            "QA76" | "QA76 .A1" => vec![1],
            "QC1" => vec![2],
            "HD28-70" => vec![3],
            _ => vec![],
        }
    }
    fn known(code: &str) -> bool {
        matches!(code, "QA" | "QC" | "HD")
    }
    #[test]
    fn recovery_keeps_complete_components_and_unresolved_evidence() {
        let r = report("HD28-70HD28-70QA76.", resolve, known);
        assert_eq!(r.assignments.len(), 1);
        assert_eq!(r.assignments[0].concept_id, 3);
        assert_eq!(r.assignments[0].evidence[0].code, "HD28-70");
        assert_eq!(r.unresolved_fragments, ["QA76."]);
        assert_eq!(r.original, "HD28-70HD28-70QA76.");
    }
    #[test]
    fn explicit_lists_and_trailing_text_preserve_good_calls() {
        let r = report("QA76 .A1 UNKNOWN; QC1; LAW", resolve, known);
        assert_eq!(r.assignments.iter().map(|a| a.concept_id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(r.unresolved_fragments, ["UNKNOWN", "LAW"]);
        assert!(r.assignments.iter().all(|a| a.evidence[0].method == LccMatchMethod::RecoveredComponent));
    }
    #[test]
    fn unknown_systems_and_embedded_prose_are_not_lcc() {
        for code in ["WB18.2", "IN PROCESS QA76", "CPB", "LAW", "WEB QA76"] {
            let r = report(code, resolve, known);
            assert!(r.assignments.is_empty());
            assert_eq!(r.unresolved_fragments, [code]);
        }
        assert_eq!(report("QA76", resolve, known).assignments[0].evidence[0].method, LccMatchMethod::Exact);
    }
}
