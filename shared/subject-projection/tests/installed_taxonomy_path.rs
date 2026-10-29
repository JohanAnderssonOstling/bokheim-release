#![cfg(not(target_arch = "wasm32"))]

mod support;

use subject_projection::{installed, loaded_unified_taxonomy_revision_id, unified_taxonomy_revision_id, UnifiedTaxonomy};

#[test]
fn selected_file_installs_the_expected_matcher() {
    let (image, definitions) = support::selected_snapshot();
    let expected = UnifiedTaxonomy::from_concepts(definitions.clone()).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    // An empty file cannot answer metadata queries. Activation must only pin
    // its path; consumers open the database after the host has installed it.
    installed::activate(file.path()).unwrap();
    assert!(loaded_unified_taxonomy_revision_id().is_none());
    installed::store_image(&image, file.path()).unwrap();
    installed::prepare(file.path()).unwrap();
    installed::activate(file.path()).unwrap();
    assert!(loaded_unified_taxonomy_revision_id().is_none(), "selecting a file must not initialize the matcher");

    assert_eq!(unified_taxonomy_revision_id(), expected.revision_id());
    support::assert_selected_matches(&expected, &definitions);
}
