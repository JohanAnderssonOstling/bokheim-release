use std::{path::Path, sync::OnceLock};
use subject_projection::{read_unified_taxonomy_sqlite, UnifiedTaxonomy};

fn taxonomy() -> &'static UnifiedTaxonomy {
    static TAXONOMY: OnceLock<UnifiedTaxonomy> = OnceLock::new();
    TAXONOMY.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/unified-taxonomy-v2.sqlite3");
        UnifiedTaxonomy::from_concepts(read_unified_taxonomy_sqlite(&path).unwrap()).unwrap()
    })
}

fn matches(code: &str, expected: i64) {
    assert_eq!(taxonomy().matching_concept_ids("lcc", code), [expected], "{code}");
}

#[test]
fn historical_topics_do_not_capture_neighboring_subjects_and_periods() {
    for (code, subject) in [
        ("DA664.C36", 13834), // Castles and historic buildings.
        ("DA665", 53071),
        ("DA667", 53071), // Travel and scenery.
        ("DH513", 51789),
        ("DH516.H8", 51789), // Collective biographies.
        ("DH511", 3577),
        ("DH516.5", 3577),
        ("DH517", 3577), // General Belgian history.
        ("DK503.54", 11216),
        ("DK504.54", 11217),
        ("DK505.54", 11221),
        ("DK503.68", 51842),
        ("DK504.68", 51851),
        ("DK505.68", 51860),
        ("DK507.729.A1", 11390),
        ("DK507.7292", 11390),
        ("DK507.7293", 11390),
        ("DK507.7295", 11346),
        ("DK507.73", 11346),
        ("DK507.74", 11346),
        ("DQ114", 10352),
        ("DQ118.R65", 10352),
        ("DQ121", 10097),
        ("DQ123.L4", 10097), // Later early-modern periods.
        ("DQ124", 3602),
        ("DQ171", 10180), // Nineteenth century.
        ("DU145", 54736), // Canberra is not a general society topic.
    ] {
        matches(code, subject);
    }
}

#[test]
fn abduction_and_kidnapping_do_not_absorb_intervening_offenses() {
    for code in ["HV6571", "HV6574.A1", "HV6595", "HV6604.A1"] {
        matches(code, 52572);
    }
    for code in ["HV6575", "HV6575.3.A1", "HV6584", "HV6587.A1", "HV6593.A1"] {
        matches(code, 52571);
    }
    matches("HV6594", 52573); // Stalking.
                              // Printed dash intervals are below the coverage threshold: rejected, not expanded.
    assert!(taxonomy().matching_concept_ids("lcc", "HV6571-HV6604").is_empty());
}

#[test]
fn malformed_legal_bounds_do_not_capture_other_jurisdictions() {
    for (code, subject) in [
        ("KB3817", 12990),
        ("KB3818", 12990),
        ("KBM1040", 13246),
        ("KBM1147", 13246),
        ("KIA313", 51566),
        ("KIA320.A1", 51566),
        ("KIA2001", 51566),
        ("KIA3317", 51566), // Alaska stays Arctic indigenous law.
    ] {
        matches(code, subject);
    }
    // Invalid oversized endpoints may get a broad fallback, never the spurious specific topic.
    for (code, excluded) in [("KB38318", 12990), ("KBM11147", 5619)] {
        assert!(!taxonomy().matching_concept_ids("lcc", code).contains(&excluded), "{code}");
    }
    assert!(!taxonomy().matching_concept_ids("lcc", "KJC2260-KJC2666").contains(&5553));
}

#[test]
fn substantive_law_is_not_misclassified_as_specialized_procedure() {
    for (code, subject) in [
        ("K1501", 11741),
        ("K1539", 11741),
        ("K1551", 11741),
        ("K1571", 13314),
        ("K1577.5", 13314),
        ("KJC2655", 13314),
        ("KJC4413", 5317), // General public law is outside court procedure.
        ("KJC4420", 5643),
        ("KJC4431", 5643),
        ("KJC4447", 5643),
        ("KJC4436", 13295), // Comparative constitutional reform.
    ] {
        matches(code, subject);
    }
}

#[test]
fn literary_periods_and_landmarks_keep_their_source_scopes() {
    for (code, subject) in [
        ("PQ1709.Y6", 7378), // Last sixteenth-century author group.
        ("PQ1710", 7911),
        ("PQ1711.A25", 7911),
        ("PQ1713.A8", 7911),
        ("PR109", 6614),
        ("PR110.A1", 6614),
        ("PR110.Z1", 6614),
        ("PS80", 13681),
        ("PS81", 13681), // Twenty-first-century criticism.
        ("PS85", 13852),
        ("PS88", 13852),
        ("PS96", 13852), // General literary history.
        ("PS185", 13638),
        ("PS229", 13638), // History by period remains specific.
    ] {
        matches(code, subject);
    }
}

#[test]
fn educational_topics_respect_their_schedule_boundaries() {
    for (code, subject) in [
        ("LB1050", 6738),
        ("LB1050.75.L49", 6738), // Reading and reading tests.
        ("LB1050.9", 2199),
        ("LB1051", 2199), // Educational psychology.
        ("LB1601", 7117),
        ("LB1602", 9846),   // Methods of study and school memoirs.
        ("LB1603", 2327),   // Secondary education starts here.
        ("R837.S55", 9909), // Medical teaching techniques.
        ("R837.8", 7986),
        ("R838", 7986), // Premedical education.
    ] {
        matches(code, subject);
    }
}
