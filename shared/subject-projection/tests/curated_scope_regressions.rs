use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::OnceLock,
};
use subject_projection::{read_unified_taxonomy_sqlite, UnifiedConceptDefinition, UnifiedTaxonomy};

struct Curated {
    matcher: UnifiedTaxonomy,
    concepts: BTreeMap<i64, UnifiedConceptDefinition>,
}

fn curated() -> &'static Curated {
    static CURATED: OnceLock<Curated> = OnceLock::new();
    CURATED.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/unified-taxonomy-v2.sqlite3");
        let definitions = read_unified_taxonomy_sqlite(&path).unwrap();
        Curated { matcher: UnifiedTaxonomy::from_concepts(definitions.clone()).unwrap(), concepts: definitions.into_iter().map(|c| (c.concept_id(), c)).collect() }
    })
}

fn labels(system: &str, code: &str) -> Vec<String> {
    let taxonomy = curated();
    taxonomy.matcher.matching_concept_ids(system, code).into_iter().map(|id| taxonomy.concepts[&id].preferred_label().to_owned()).collect()
}

fn ancestors(id: i64) -> BTreeSet<i64> {
    let taxonomy = curated();
    let mut pending = taxonomy.concepts[&id].parent_ids().to_vec();
    let mut result = BTreeSet::new();
    while let Some(parent) = pending.pop() {
        if result.insert(parent) {
            pending.extend_from_slice(taxonomy.concepts[&parent].parent_ids());
        }
    }
    result
}

#[test]
fn neurosis_cutters_do_not_all_become_eating_disorders() {
    for code in ["RC552.A43", "RC552.A44 .S65 2020", "RC552.F423", "RC552.S62"] {
        assert_eq!(labels("lcc", code), ["Anxieties & Phobias"], "{code}");
    }
    for code in ["RC552.A5", "RC552.B84 .S65 2020", "RC552.C65", "RC552.E18", "RC552.N55"] {
        assert_eq!(labels("lcc", code), ["Eating Disorders"], "{code}");
    }
    for code in ["RC552.N5", "RC552.T5", "RC552.W74"] {
        assert_eq!(labels("lcc", code), ["Psychopathology"], "{code}");
    }
    assert_eq!(labels("lcc", "RC552.P67"), ["Post-Traumatic Stress Disorder"]);
}

#[test]
fn aesthetics_history_retains_its_scope_separately_from_general_philosophy() {
    for (code, expected) in [("BH151", "Modern Aesthetics"), ("BH131", "Medieval Aesthetics"), ("BH83", "Origins of Aesthetics"), ("BH108", "Ancient Greek Aesthetics")] {
        assert_eq!(labels("lcc", code), [expected], "{code}");
    }
    assert_eq!(labels("ddc", "189"), ["Medieval Philosophy"]);
    assert_eq!(labels("ddc", "190"), ["Modern Philosophy"]);
}

#[test]
fn equivalent_source_headings_share_one_concept() {
    let taxonomy = curated();
    for (system, code, lcc) in [("bisac", "SCI070000", "QL"), ("bisac", "SCI020000", "QH540"), ("bisac", "NAT010000", "QH540"), ("bisac", "TEC005000", "TH"), ("bisac", "LIT004150", "PQ69"), ("bisac", "PSY022050", "RC514")] {
        let expected = taxonomy.matcher.matching_concept_ids("lcc", lcc);
        assert_eq!(expected.len(), 1, "{lcc}");
        assert_eq!(taxonomy.matcher.matching_concept_ids(system, code), expected, "{code} / {lcc}");
    }
}

#[test]
fn regional_periods_keep_their_country_context() {
    let taxonomy = curated();
    let french = taxonomy.matcher.matching_concept_ids("lcc", "B2431")[0];
    let german = taxonomy.matcher.matching_concept_ids("lcc", "B3397")[0];
    assert_ne!(french, german);
    assert!(ancestors(french).contains(&11152)); // French Modern Philosophy.
    assert!(ancestors(german).contains(&10891)); // German/Austrian Modern Philosophy.
    let dutch = taxonomy.matcher.matching_concept_ids("lcc", "B4051")[0];
    let belgian = taxonomy.matcher.matching_concept_ids("lcc", "B4157")[0];
    assert_ne!(dutch, belgian);
    assert!(ancestors(dutch).contains(&10920));
    assert!(!ancestors(belgian).contains(&10920));
}

#[test]
fn religious_philosophies_are_not_forced_into_ancient_chronology() {
    assert_eq!(labels("lcc", "B162.6"), ["Shinto Philosophy"]);
    for id in [10995, 11093, 10861, 10901, 11088, 11151] {
        let parents = ancestors(id);
        assert!(parents.contains(&4538)); // Philosophy of Religion.
        assert!(!parents.contains(&4508)); // Ancient & Classical Philosophy.
        assert!(!parents.contains(&4510)); // Modern Philosophy.
    }
}

#[test]
fn colonial_routes_do_not_include_independent_country_history() {
    for (independent_period, empire) in [(11268, 50813), (11231, 50814)] {
        assert!(!ancestors(independent_period).contains(&empire));
    }
    assert!(ancestors(11306).contains(&50813)); // British Guiana, 1803–1966.
    assert!(ancestors(11239).contains(&50814)); // Suriname, 1604–1814.
    assert!(ancestors(10725).contains(&3513)); // 1979 conflict under Vietnamese history.
    assert!(!ancestors(10725).contains(&3024)); // Not Vietnamese cooking.
}

#[test]
fn ethical_topics_do_not_all_become_good_and_evil() {
    for code in ["BJ1340", "BJ1451", "BJ1481"] {
        assert_eq!(labels("lcc", code), ["Ethics"], "{code}");
    }
    for code in ["BJ1400", "BJ1401", "BJ1408.5"] {
        assert_eq!(labels("lcc", code), ["Good & Evil"], "{code}");
    }
    assert_eq!(labels("bisac", "PHI008000"), ["Good & Evil"]);
}

#[test]
fn cognitive_topics_keep_their_own_scopes() {
    for code in ["BF355", "BF357", "BF468", "BF482", "BF491", "BF450"] {
        assert_eq!(labels("lcc", code), ["Cognition"], "{code}");
    }
    for (code, expected) in [
        ("BF455", "Psycholinguistics"),
        ("BF463.M4", "Psycholinguistics"),
        ("BF495", "Synesthesia"),
        ("BF499", "Synesthesia"),
        ("BF408", "Creativity"),
        ("BF426", "Creativity"),
        ("BF431", "Intelligence"),
        ("BF441", "Thought & Reasoning"),
        ("BF444", "Reasoning & Judgment"),
        ("BF337.C63", "Habit, Adjustment & Nature–Nurture"),
        ("BF346.M8", "Habit, Adjustment & Nature–Nurture"),
        ("BF353", "Environmental Psychology"),
    ] {
        assert_eq!(labels("lcc", code), [expected], "{code}");
    }
    assert_eq!(labels("bisac", "LAN009040"), ["Psycholinguistics"]);
    assert!(!ancestors(53300).contains(&12875));
    assert!(ancestors(3784).contains(&5389));
    assert!(ancestors(3784).contains(&3778));
}

#[test]
fn nursing_and_public_mental_health_are_not_limited_to_acute_care_or_prevention() {
    assert_eq!(labels("lcc", "RT89"), ["Management & Leadership"]);
    for code in ["RT97", "RT98", "RT120.H65", "RT120.R87"] {
        assert_eq!(labels("lcc", code), ["Home & Community Nursing"], "{code}");
    }
    assert_eq!(labels("lcc", "RT120.F34"), ["Nursing"]);
    for code in ["RA790.55", "RA790.75"] {
        assert_eq!(labels("lcc", code), ["Public Mental Health"], "{code}");
    }
    for id in [2748, 2751, 2752, 2759, 2761] {
        assert_eq!(curated().concepts[&id].parent_ids(), &[2747]);
    }
}

#[test]
fn history_source_duplicates_share_subjects_and_browsing_routes() {
    let taxonomy = curated();
    for (bisac, lcc) in [("HIS027350", "D151"), ("HIS043000", "D804.177"), ("HIS035000", "D16.2"), ("HIS015060", "DA550")] {
        let from_lcc = taxonomy.matcher.matching_concept_ids("lcc", lcc);
        assert_eq!(from_lcc.len(), 1, "{lcc}");
        assert_eq!(taxonomy.matcher.matching_concept_ids("bisac", bisac), from_lcc, "{bisac}");
    }
    assert!(ancestors(3687).contains(&3608)); // Medieval history.
    assert!(ancestors(3687).contains(&5814)); // Military eras and early wars.
    assert!(ancestors(3533).contains(&54692)); // Keep England's By Period group.
    assert_eq!(labels("lcc", "DT545"), ["Côte d’Ivoire"]);
    assert_eq!(labels("lcc", "DT433.5"), ["Kenyan"]);
    assert_eq!(labels("lcc", "DT211"), ["Libyan"]);
}

#[test]
fn colonial_routes_do_not_imply_the_wrong_empire() {
    assert!(ancestors(50816).contains(&50814)); // New Netherland: Dutch America.
    assert!(!ancestors(50816).contains(&50813)); // Not British America.
    for (code, id) in [("F372", 3447), ("F314", 3439)] {
        // Local time slices now classify directly to their places. A broad
        // place must not inherit the removed period's colonial browsing route.
        assert_eq!(curated().matcher.matching_concept_ids("lcc", code), [id]);
        assert!(!ancestors(id).contains(&3409));
        assert!(!ancestors(id).contains(&50813));
        assert!(!ancestors(id).contains(&50815));
    }
    for id in [10639, 10394, 10527, 10388] {
        assert!(ancestors(id).contains(&3371)); // Tunisian periods belong to Tunisia.
        let parent = curated().concepts[&id].parent_ids()[0];
        assert_eq!(curated().concepts[&parent].preferred_label(), "By Period");
    }
    assert_eq!(curated().concepts[&11531].preferred_label(), "19th Century–Present");
}

#[test]
fn local_history_fallback_keeps_the_spatial_group_without_a_sole_other_leaf() {
    let taxonomy = curated();
    assert_eq!(taxonomy.matcher.matching_concept_ids("lcc", "DS99.A"), [54734]);
    assert_eq!(taxonomy.concepts[&54734].preferred_label(), "Local");
    assert!(ancestors(54734).contains(&5540)); // By Region is preserved.
    assert_eq!(labels("lcc", "D374"), ["Eastern Question"]);
}

#[test]
fn local_history_periods_classify_to_the_place() {
    for (code, place) in [
        ("F73.44", 50928),
        ("F73.52", 50928), // Boston.
        ("F128.44", 50921),
        ("F128.55", 50921), // New York City.
        ("F158.5", 50929),  // Philadelphia.
        ("F69", 3450),      // Massachusetts.
        ("F391.2", 3472),   // Texas.
        ("F200", 50911),    // Washington, D.C.
        ("DD881", 11448),   // Berlin.
    ] {
        assert_eq!(curated().matcher.matching_concept_ids("lcc", code), [place], "{code}");
    }
    assert_eq!(curated().matcher.matching_concept_ids("lcc", "F122.1"), [50816]);
}
