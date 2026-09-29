//! Read-only, opt-in audit of replacing the matcher with a containment forest.
//! Run with TAXONOMY_AUDIT_DIR=/absolute/output cargo test --release -p
//! subject-projection --lib runtime::range_audit::audit -- --ignored --nocapture.
use super::*;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct Bound {
    letters: String,
    number: f64,
    cutters: Vec<(String, String)>,
    upper: bool,
}

impl Bound {
    fn cmp(&self, other: &Self) -> Ordering {
        let order = self.letters.cmp(&other.letters).then_with(|| self.number.total_cmp(&other.number));
        if !order.is_eq() {
            return order;
        }
        for ((a, ad), (b, bd)) in self.cutters.iter().zip(&other.cutters) {
            let order = a.cmp(b);
            if !order.is_eq() {
                return order;
            }
            let order = match (ad.is_empty() && self.upper, bd.is_empty() && other.upper) {
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (true, true) => Ordering::Equal,
                (false, false) => ad.cmp(bd),
            };
            if !order.is_eq() {
                return order;
            }
        }
        match self.cutters.len().cmp(&other.cutters.len()) {
            Ordering::Less => {
                if self.upper {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            Ordering::Greater => {
                if other.upper {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            Ordering::Equal => self.upper.cmp(&other.upper),
        }
    }
    fn code(&self, point: bool) -> Option<String> {
        if !self.number.is_finite() {
            return if point { None } else { Some(self.letters.clone()) };
        }
        let mut code = format!("{}{}", self.letters, self.number);
        for (letters, digits) in &self.cutters {
            code.push('.');
            code.push_str(letters);
            code.push_str(digits);
            if point && digits.is_empty() {
                code.push('1');
            }
        }
        Some(code)
    }
}

struct Range {
    index: usize,
    concept: i64,
    low: Bound,
    high: Bound,
}
impl Range {
    fn contains(&self, other: &Self) -> bool {
        !self.low.cmp(&other.low).is_gt() && !self.high.cmp(&other.high).is_lt()
    }
    fn point(&self, point: &Bound) -> bool {
        !self.low.cmp(point).is_gt() && !point.cmp(&self.high).is_gt()
    }
    fn text(&self) -> String {
        format!("{}..{}", self.low.code(false).unwrap(), self.high.code(false).unwrap())
    }
}
fn bounds(r: &LccSelector, index: usize) -> Range {
    Range {
        index,
        concept: r.concept_id,
        low: Bound { letters: r.start_letters.clone(), number: r.start_number, cutters: r.start_cutters.clone(), upper: false },
        high: Bound { letters: r.end_letters.clone(), number: r.end_number, cutters: r.end_cutters.clone(), upper: true },
    }
}
fn write_json(out: &std::path::Path, name: &str, value: &Value) {
    std::fs::write(out.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

#[test]
#[ignore = "writes an explicit audit report; does not change the taxonomy"]
fn audit() {
    let out = PathBuf::from(std::env::var_os("TAXONOMY_AUDIT_DIR").expect("set TAXONOMY_AUDIT_DIR to the report directory"));
    std::fs::create_dir_all(&out).unwrap();
    // The compiled image freezes this audit even if source taxonomy edits continue.
    let image = std::env::var_os("TAXONOMY_AUDIT_INPUT").map(|path| std::fs::read(path).unwrap()).unwrap_or_else(|| DEFAULT_UNIFIED_TAXONOMY_SQLITE.to_vec());
    std::fs::write(out.join("taxonomy.sqlite3"), image).unwrap();
    let connection = rusqlite::Connection::open_with_flags(out.join("taxonomy.sqlite3"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let definitions = crate::taxonomy_reader::read_unified_taxonomy_connection(&connection, "audit snapshot").unwrap();
    let labels = definitions.iter().map(|c| (c.concept_id(), c.preferred_label().to_owned())).collect::<BTreeMap<_, _>>();
    let runtime = build_runtime(definitions, false).unwrap();
    let index = &runtime.selectors["lcc"];
    let mut ranges = index.lcc_ranges.iter().enumerate().map(|(i, r)| bounds(r, i)).collect::<Vec<_>>();
    ranges.sort_by(|a, b| a.low.cmp(&b.low).then_with(|| b.high.cmp(&a.high)));
    let describe = |r: &Range| json!({"range_index":r.index,"concept_id":r.concept,"label":labels[&r.concept],"range":r.text()});
    let relation = |a: i64, b: i64| {
        if a == b {
            "same_concept"
        } else if runtime.ancestors[&b].contains(&a) {
            "a_ancestor_of_b"
        } else if runtime.ancestors[&a].contains(&b) {
            "b_ancestor_of_a"
        } else {
            "unrelated_concepts"
        }
    };
    let mut containers = vec![Vec::<usize>::new(); ranges.len()];
    let mut crossing = Vec::new();
    let mut equals = Vec::new();
    let mut nested = BTreeMap::<&str, usize>::new();
    let mut crossing_ranges = BTreeSet::new();
    let mut samples = BTreeSet::<String>::new();
    let mut same_numeric_boundary = 0;
    let mut geometric_checks = 0;
    for (i, a) in ranges.iter().enumerate() {
        assert!(!a.low.cmp(&a.high).is_gt(), "reversed range {}", a.text());
        assert!(a.low.cutters.iter().rev().skip(1).all(|(_, d)| !d.is_empty()));
        assert!(a.high.cutters.iter().rev().skip(1).all(|(_, d)| !d.is_empty()));
        for bound in [&a.low, &a.high] {
            if let Some(code) = bound.code(true) {
                samples.insert(code);
            }
        }
        if a.low.letters == a.high.letters && a.high.number.is_finite() && a.low.number < a.high.number {
            samples.insert(format!("{}{}", a.low.letters, (a.low.number + a.high.number) / 2.0));
        }
        for (j, b) in ranges.iter().enumerate().skip(i + 1) {
            if b.low.cmp(&a.high).is_gt() {
                break;
            }
            let ab = a.contains(b);
            let ba = b.contains(a);
            // Verify the geometric model against the production Cutter containment comparator.
            assert_eq!(ab, lcc_range_contains(&index.lcc_ranges[a.index], &index.lcc_ranges[b.index]));
            assert_eq!(ba, lcc_range_contains(&index.lcc_ranges[b.index], &index.lcc_ranges[a.index]));
            geometric_checks += 1;
            if ab && ba {
                equals.push(json!({"a":describe(a),"b":describe(b)}));
            } else if ab {
                containers[j].push(i);
                *nested.entry(relation(a.concept, b.concept)).or_default() += 1;
            } else if ba {
                containers[i].push(j);
                *nested.entry(relation(b.concept, a.concept)).or_default() += 1;
            } else {
                crossing_ranges.extend([i, j]);
                let boundary = a.high.letters == b.low.letters && a.high.number == b.low.number;
                same_numeric_boundary += usize::from(boundary);
                let witness = b.low.code(true).filter(|code| {
                    let call = parse_lcc_call(code).unwrap();
                    let p = Bound { letters: call.letters, number: call.number, cutters: cutter_components(call.remainder), upper: false };
                    a.point(&p) && b.point(&p)
                });
                crossing.push(json!({"a":describe(a),"b":describe(b),"relationship":relation(a.concept,b.concept),"shared_numeric_boundary_only":boundary,"witness":witness}));
            }
        }
    }
    let mut parents = Vec::new();
    let mut parent_counts = BTreeMap::<usize, usize>::new();
    let mut parent_relations = BTreeMap::<&str, usize>::new();
    for (child, candidates) in containers.iter().enumerate() {
        let closest = candidates.iter().copied().filter(|outer| !candidates.iter().any(|inner| outer != inner && ranges[*outer].contains(&ranges[*inner]) && !ranges[*inner].contains(&ranges[*outer]))).collect::<Vec<_>>();
        *parent_counts.entry(closest.len()).or_default() += 1;
        for parent in &closest {
            *parent_relations.entry(relation(ranges[*parent].concept, ranges[child].concept)).or_default() += 1;
        }
        if closest.len() > 1 {
            parents.push(json!({"child":describe(&ranges[child]),"parents":closest.iter().map(|i|describe(&ranges[*i])).collect::<Vec<_>>() }));
        }
    }
    write_json(&out, "crossing-ranges.json", &json!(crossing));
    write_json(&out, "equal-ranges.json", &json!(equals));
    write_json(&out, "ambiguous-parents.json", &json!(parents));
    eprintln!("audited {} ranges; {} crossing pairs; {} ranges with multiple closest containers", ranges.len(), crossing.len(), parents.len());

    // Probe endpoints and numeric interiors with the actual matcher; range-only
    // selection means the inclusion-minimal covering ranges, with no subject ranking.
    samples.extend(index.exact.keys().filter(|code| parse_lcc_call(code).is_some()).cloned());
    let mut mismatches = Vec::new();
    let mut categories = BTreeMap::<&str, usize>::new();
    let mut valid_points = 0;
    let mut exact_first_same = 0;
    let mut exact_first_differences = 0;
    let mut exact_first_strict_differences = 0;
    let mut exact_first_examples = Vec::new();
    for code in &samples {
        let Some(call) = parse_lcc_call(code) else { continue };
        if canonical_lcc_notation(code).is_none() {
            continue;
        }
        let point = Bound { letters: call.letters.clone(), number: call.number, cutters: cutter_components(call.remainder), upper: false };
        let hits = ranges.iter().enumerate().filter_map(|(i, r)| r.point(&point).then_some(i)).collect::<Vec<_>>();
        let minimal = hits.iter().copied().filter(|a| !hits.iter().any(|b| a != b && ranges[*a].contains(&ranges[*b]) && !ranges[*b].contains(&ranges[*a]))).collect::<Vec<_>>();
        let proposed = minimal.iter().map(|i| ranges[*i].concept).collect::<BTreeSet<_>>();
        let actual = matching_candidates(&runtime, "lcc", code);
        valid_points += 1;
        let exact = lcc_match_keys(code).unwrap_or_default().iter().filter_map(|key| index.exact.get(key)).flatten().copied().collect::<BTreeSet<_>>();
        let exact_ranked = lcc_match_keys(code).unwrap_or_default().iter().enumerate().flat_map(|(rank, key)| index.exact.get(key).into_iter().flatten().map(move |id| (*id, rank))).collect::<Vec<_>>();
        let shadowed = exact_ranked.iter().flat_map(|(id, _)| runtime.ancestors[id].iter().copied()).collect::<BTreeSet<_>>();
        let exact_ranked = exact_ranked.into_iter().filter(|(id, _)| !shadowed.contains(id)).collect::<Vec<_>>();
        let best = exact_ranked.iter().map(|(_, rank)| *rank).max();
        let exact_first = if exact_ranked.is_empty() { proposed.clone() } else { exact_ranked.iter().filter(|(_, rank)| Some(*rank) == best).map(|(id, _)| *id).collect() };
        let actual_resolved = actual.iter().copied().next().filter(|_| actual.len() == 1);
        let proposed_resolved = exact_first.iter().copied().next().filter(|_| exact_first.len() == 1);
        exact_first_strict_differences += usize::from(actual_resolved != proposed_resolved);
        if exact_first == actual {
            exact_first_same += 1;
        } else {
            exact_first_differences += 1;
            if !exact.is_empty() {
                exact_first_examples.push(json!({"code":code,"exact_first_candidates":exact_first,"current_candidates":actual,"range_only_candidates":proposed}));
            }
        }
        let category = if proposed == actual {
            "same_candidates"
        } else if !exact.is_empty() {
            "different_with_exact_candidates"
        } else if proposed.len() > 1 {
            "different_with_crossing_or_equal_minima"
        } else if proposed.is_empty() {
            "different_without_covering_range"
        } else {
            "different_with_unique_minimal_concept"
        };
        *categories.entry(category).or_default() += 1;
        if proposed != actual {
            mismatches.push(json!({"code":code,"category":category,"range_only_candidates":proposed,"current_candidates":actual,"exact_candidates":exact,
            "minimal_ranges":minimal.iter().map(|i|describe(&ranges[*i])).collect::<Vec<_>>() }));
        }
    }
    write_json(&out, "point-comparisons.json", &json!(mismatches));
    write_json(&out, "exact-first-differences.json", &json!(exact_first_examples));
    let mut interval_examples = Vec::new();
    let mut interval_same = 0;
    let mut interval_different = 0;
    let mut interval_skipped = 0;
    for input in &ranges {
        let code = format!("{}-{}", input.low.code(false).unwrap(), input.high.code(false).unwrap());
        if canonical_lcc_notation(&code).is_none() {
            interval_skipped += 1;
            continue;
        }
        let hits = ranges.iter().enumerate().filter_map(|(i, r)| r.contains(input).then_some(i)).collect::<Vec<_>>();
        let minimal = hits.iter().copied().filter(|a| !hits.iter().any(|b| a != b && ranges[*a].contains(&ranges[*b]) && !ranges[*b].contains(&ranges[*a]))).collect::<Vec<_>>();
        let proposed = minimal.iter().map(|i| ranges[*i].concept).collect::<BTreeSet<_>>();
        let actual = matching_candidates(&runtime, "lcc", &code);
        if proposed == actual {
            interval_same += 1;
        } else {
            interval_different += 1;
            interval_examples.push(json!({"code":code,"range_only_candidates":proposed,"current_candidates":actual,"source":describe(input)}));
        }
    }
    write_json(&out, "interval-comparisons.json", &json!(interval_examples));

    let mut selector_stats = BTreeMap::new();
    let mut conflicts = Vec::new();
    for (system, selectors) in runtime.selectors.iter() {
        let mut shared = 0;
        let mut unrelated = 0;
        let mut prefix_exact_overlaps = 0;
        for (key, ids) in &selectors.exact {
            if ids.len() > 1 {
                shared += 1;
                let shadowed = ids.iter().flat_map(|id| runtime.ancestors[id].iter().copied()).collect::<BTreeSet<_>>();
                let survivors = ids.difference(&shadowed).copied().collect::<Vec<_>>();
                if survivors.len() > 1 {
                    unrelated += 1;
                    conflicts
                        .push(json!({"system":system,"key":key,"concepts":ids,"unrelated_survivors":survivors,"current_candidates":matching_candidates(&runtime,system,key),"current_resolved":strict_matching_concepts(&runtime,system,key)}));
                }
            }
            prefix_exact_overlaps += selectors.prefixes.iter().filter(|(p, _)| key.starts_with(p)).count();
        }
        let prefix_pairs = selectors.prefixes.iter().enumerate().map(|(i, (a, _))| selectors.prefixes[i + 1..].iter().filter(|(b, _)| a.starts_with(b) || b.starts_with(a)).count()).sum::<usize>();
        selector_stats.insert(system.clone(),json!({"exact_keys_after_runtime_normalization":selectors.exact.len(),"exact_keys_with_multiple_owners":shared,"exact_keys_with_unrelated_survivors":unrelated,"prefix_rules":selectors.prefixes.len(),"textual_prefix_exact_pairs":prefix_exact_overlaps,"textual_prefix_prefix_pairs":prefix_pairs}));
    }
    write_json(&out, "selector-conflicts.json", &json!(conflicts));
    let summary = json!({"revision":runtime.revision_id,"concepts":labels.len(),"ranges":ranges.len(),"overlapping_pairs_checked":geometric_checks,
        "crossing_pairs":crossing.len(),"crossing_ranges":crossing_ranges.len(),"crossing_pairs_at_shared_numeric_boundary":same_numeric_boundary,
        "equal_pairs":equals.len(),"nested_pair_relationships":nested,"closest_container_counts":parent_counts,"closest_container_relationships":parent_relations,
        "valid_point_probes":valid_points,"point_categories":categories,"exact_first_point_comparison":{"same":exact_first_same,"different":exact_first_differences,"differences_with_exact_rules":exact_first_examples.len(),"strict_resolution_differences":exact_first_strict_differences},
        "whole_range_input_comparison":{"same":interval_same,"different":interval_different,"skipped_unparsed":interval_skipped},"selectors":selector_stats,
        "limitations":["Point probes are generated endpoints, numeric midpoints and exact keys; they are not an exhaustive input proof or a real-book corpus.","Point comparison uses current match_candidates (before ambiguity rejection and damaged-input recovery).","Prefix pair counts describe text overlap; LCC prefix acceptance also uses parsed match keys.","Crossing ranges prevent a containment forest even if some conflicts share an owner or are currently resolved by ranking."]});
    write_json(&out, "summary.json", &summary);
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}
