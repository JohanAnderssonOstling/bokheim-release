#![cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;
use subject_projection::{bisac_code_for_path, match_bisac_subjects, match_bisac_subjects_with_minimum_adjacent, BookSubject};
use subject_taxonomy::compact_whitespace;

fn subject(name: &str, authority: Option<&str>, code: Option<&str>) -> BookSubject {
    BookSubject::new(None, name, "test:subject", authority.map(str::to_owned), code.map(str::to_owned)).unwrap()
}

#[test]
fn every_bundled_code_keeps_its_reference_path() {
    let mut expected = BTreeMap::<String, String>::new();
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(include_str!("../data/bisac-2021.csv").as_bytes());
    for record in reader.records() {
        let record = record.unwrap();
        let code = record[0].trim();
        let path = record[1].split('/').map(compact_whitespace).filter(|component| !component.is_empty()).collect::<Vec<_>>().join(" / ");
        if code.is_empty() || path.is_empty() {
            continue;
        }
        expected
            .entry(code.to_owned())
            .and_modify(|previous| {
                if path > *previous {
                    *previous = path.clone();
                }
            })
            .or_insert(path);
    }
    for (code, path) in expected {
        let assignments = match_bisac_subjects(&[subject(&code.to_lowercase(), Some("bisac"), Some(&code.to_lowercase()))]);
        assert_eq!(assignments.len(), 1, "{code}");
        assert_eq!(assignments[0].node_path(), path, "{code}");
        assert_eq!(assignments[0].evidence_positions(), &[0]);
        assert_eq!(bisac_code_for_path(&path).as_deref(), Some(code.as_str()));
    }
}

#[test]
fn heading_components_keep_their_evidence_and_specificity_rules() {
    let subjects = [subject("Science", None, None), subject("Physics", None, None), subject("Quantum Theory", None, None)];
    let assignments = match_bisac_subjects(&subjects);
    let specific = assignments.iter().find(|assignment| assignment.node_path() == "Science / Physics / Quantum Theory").unwrap();
    assert_eq!(specific.evidence_positions(), &[0, 1, 2]);
    assert!(!assignments.iter().any(|assignment| assignment.node_path() == "Science / Physics / General"));
    assert!(match_bisac_subjects_with_minimum_adjacent(&subjects, 4).is_empty());
    assert_eq!(bisac_code_for_path("  science > physics ").as_deref(), Some("SCI055000"));
}

#[test]
fn explicit_codes_bypass_heading_depth_and_manual_codes_survive_lcc_precedence() {
    let explicit = subject("SCI057000", Some("bisac"), Some("SCI057000"));
    assert_eq!(match_bisac_subjects_with_minimum_adjacent(&[explicit.clone()], 10)[0].node_path(), "Science / Physics / Quantum Theory");
    let mut subjects = vec![subject("QA76.76", Some("lcc"), Some("QA76.76")), subject("Science / Physics / Relativity", None, None), explicit];
    assert!(match_bisac_subjects(&subjects).is_empty());
    subjects.push(BookSubject::new(None, "SCI057000", "manual:test", Some("bisac".into()), Some("SCI057000".into())).unwrap());
    let assignments = match_bisac_subjects(&subjects);
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].node_path(), "Science / Physics / Quantum Theory");
    assert_eq!(assignments[0].evidence_positions(), &[3]);
}

#[test]
fn unrecognized_codes_are_rejected() {
    assert!(match_bisac_subjects(&[subject("SCI999999", Some("bisac"), Some("SCI999999"))]).is_empty());
}
