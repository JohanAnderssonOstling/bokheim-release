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
fn local_history_fallback_keeps_the_spatial_group_without_a_sole_other_leaf() {
    let taxonomy = curated();
    assert_eq!(taxonomy.matcher.matching_concept_ids("lcc", "DS99.A"), [54734]);
    assert_eq!(taxonomy.concepts[&54734].preferred_label(), "Local");
    assert!(ancestors(54734).contains(&5540)); // By Region is preserved.
    assert_eq!(labels("lcc", "D374"), ["Eastern Question"]);
}
