#![cfg(not(target_arch = "wasm32"))]

mod support;

use rusqlite::Connection;
use subject_projection::{installed, loaded_unified_taxonomy_revision_id, unified_taxonomy_revision_id, UnifiedTaxonomy};

#[test]
fn selected_file_stays_pinned_when_a_new_reference_is_prepared() {
    let (image, definitions) = support::selected_snapshot();
    let expected = UnifiedTaxonomy::from_concepts(definitions.clone()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("selected taxonomy #1?.sqlite3");
    installed::store_image(&image, &selected).unwrap();
    installed::prepare(&selected).unwrap();
    installed::activate(&selected).unwrap();
    assert!(loaded_unified_taxonomy_revision_id().is_none());

    let reader = Connection::open_in_memory().unwrap();
    reader.execute("ATTACH ?1 AS curated", [installed::read_only_uri(&selected).unwrap()]).unwrap();
    let replacement = directory.path().join("next.sqlite3");
    std::fs::copy(&selected, &replacement).unwrap();
    Connection::open(&replacement).unwrap().execute("UPDATE concept SET preferred_label='Next reference' WHERE preferred_label='Installed snapshot test label'", []).unwrap();
    installed::prepare(&replacement).unwrap();
    assert!(installed::activate(&replacement).is_err());
    assert!(loaded_unified_taxonomy_revision_id().is_none());
    let count: i64 = reader.query_row("SELECT count(*) FROM curated.concept WHERE preferred_label='Installed snapshot test label'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 1);
    assert!(reader.execute("DELETE FROM curated.concept", []).is_err());

    assert_eq!(unified_taxonomy_revision_id(), expected.revision_id());
    support::assert_selected_matches(&expected, &definitions);
}
