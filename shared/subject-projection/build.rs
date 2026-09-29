#[path = "src/selector_format.rs"]
mod selector_format;
// Compile the same matcher builder used for downloaded overlays. The build
// script never loads the artifact it is producing.
#[allow(dead_code)]
#[path = "src/lcc.rs"]
mod lcc;
#[allow(dead_code)]
#[path = "src/runtime_index.rs"]
mod runtime;
fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
#[path = "src/range_storage.rs"]
mod range_storage;
#[path = "src/taxonomy_reader.rs"]
mod taxonomy_reader;

use rusqlite::Connection;
use std::env;
use std::path::PathBuf;

const CLIENT_TAXONOMY_DEPTH: u8 = 2;

fn main() {
    for path in ["build.rs", "src", "data"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-changed=data/unified-taxonomy-v2.sqlite3");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_AUTHORITATIVE_TAXONOMY");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let database_path = manifest_dir.join("data/unified-taxonomy-v2.sqlite3");
    let connection = Connection::open(&database_path).unwrap_or_else(|error| panic!("failed to open {}: {error}", database_path.display()));
    let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0)).expect("check taxonomy database integrity");
    assert_eq!(integrity, "ok", "taxonomy database failed its integrity check");
    let foreign_key_violations: i64 = connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0)).expect("check taxonomy database foreign keys");
    assert_eq!(foreign_key_violations, 0, "taxonomy database contains invalid relationships");
    let format_version: String = connection.query_row("SELECT value FROM taxonomy_meta WHERE key='format_version'", [], |row| row.get(0)).expect("taxonomy database has a format version");
    assert_eq!(format_version, "1", "unsupported taxonomy database format");
    let published_release_id: u64 =
        connection.query_row("SELECT value FROM taxonomy_meta WHERE key='release_id'", [], |row| row.get::<_, String>(0)).expect("taxonomy database has a release identifier").parse().expect("taxonomy release identifier is an integer");
    assert!(published_release_id > 0, "taxonomy release identifier must be positive");

    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let client_database_path = output_dir.join("unified-taxonomy-client-v2.sqlite3");
    if env::var_os("CARGO_FEATURE_AUTHORITATIVE_TAXONOMY").is_some() {
        std::fs::copy(&database_path, &client_database_path).unwrap_or_else(|error| panic!("failed to copy authoritative taxonomy to {}: {error}", client_database_path.display()));
    } else {
        build_client_database(&database_path, &client_database_path);
    }
    let client_connection = Connection::open(&client_database_path).unwrap_or_else(|error| panic!("failed to open {}: {error}", client_database_path.display()));
    // Route IDs identify occurrences, so a concept can retain several parents.
    client_connection.execute_batch(include_str!("src/route_storage.sql")).expect("compile curated navigation routes");
    // Route and lookup index construction can leave partially filled pages.
    // Compact the final image before it is embedded in client binaries.
    client_connection.execute_batch("VACUUM;").expect("compact bundled taxonomy database");
    let concepts = taxonomy_reader::read_unified_taxonomy_connection(&client_connection, "build-time taxonomy").expect("read bundled taxonomy");
    let matcher = runtime::build_runtime(concepts, false).expect("compile bundled taxonomy matcher");
    let encoded = bincode::serde::encode_to_vec(&matcher, bincode::config::standard()).expect("encode taxonomy matcher");
    std::fs::write(output_dir.join("unified-taxonomy-matcher.bin"), encoded).expect("write taxonomy matcher");
    std::fs::write(output_dir.join("unified-taxonomy-revision-id.txt"), &matcher.revision_id).expect("write taxonomy revision");
}

fn build_client_database(authority_path: &PathBuf, client_path: &PathBuf) {
    if client_path.exists() {
        std::fs::remove_file(client_path).unwrap_or_else(|error| panic!("failed to replace {}: {error}", client_path.display()));
    }
    let client = Connection::open(client_path).unwrap_or_else(|error| panic!("failed to create {}: {error}", client_path.display()));
    client
        .execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE taxonomy_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL) STRICT;
             CREATE TABLE concept(concept_id INTEGER PRIMARY KEY,preferred_label TEXT NOT NULL CHECK(length(trim(preferred_label))>0),normalized_label TEXT NOT NULL) STRICT;
             CREATE TABLE concept_parent(
                 concept_id INTEGER NOT NULL REFERENCES concept(concept_id) ON DELETE CASCADE,
                 parent_concept_id INTEGER NOT NULL REFERENCES concept(concept_id),
                 ordinal INTEGER NOT NULL CHECK(ordinal>=0),
                 PRIMARY KEY(concept_id,parent_concept_id),UNIQUE(concept_id,ordinal),CHECK(concept_id<>parent_concept_id)
             ) STRICT, WITHOUT ROWID;
             CREATE TABLE lcc_selector(
                 concept_id INTEGER NOT NULL REFERENCES concept(concept_id) ON DELETE CASCADE,
                 selector TEXT NOT NULL CHECK(length(trim(selector))>0),
                 PRIMARY KEY(concept_id,selector)
             ) STRICT, WITHOUT ROWID;
             CREATE TABLE bisac_selector(
                 concept_id INTEGER NOT NULL REFERENCES concept(concept_id) ON DELETE CASCADE,
                 selector TEXT NOT NULL CHECK(length(trim(selector))>0),
                 PRIMARY KEY(concept_id,selector)
             ) STRICT, WITHOUT ROWID;
             CREATE TABLE ddc_selector(
                 selector TEXT NOT NULL CHECK(length(selector)=3 AND selector GLOB '[0-9][0-9][0-9]'),
                 concept_id INTEGER NOT NULL REFERENCES concept(concept_id) ON DELETE CASCADE,
                 PRIMARY KEY(selector)
             ) STRICT, WITHOUT ROWID;
             CREATE INDEX concept_parent_parent_idx ON concept_parent(parent_concept_id,ordinal,concept_id);
             CREATE INDEX lcc_selector_lookup_idx ON lcc_selector(selector,concept_id);
             CREATE INDEX bisac_selector_lookup_idx ON bisac_selector(selector,concept_id);
             CREATE INDEX ddc_selector_concept_idx ON ddc_selector(concept_id);
             CREATE INDEX concept_label_lookup_idx ON concept(normalized_label,concept_id);",
        )
        .expect("create client taxonomy schema");
    client.execute_batch(include_str!("src/range_storage.sql")).expect("create client range schema");
    client.execute("ATTACH DATABASE ?1 AS authority", [authority_path.to_string_lossy().as_ref()]).expect("attach authoritative taxonomy");
    client
        .execute_batch(&format!(
            "CREATE TEMP TABLE client_concept(concept_id INTEGER PRIMARY KEY);
             INSERT INTO client_concept
             WITH RECURSIVE selected(concept_id,depth) AS (
                 SELECT c.concept_id,0 FROM authority.concept c
                 WHERE NOT EXISTS(SELECT 1 FROM authority.concept_parent p WHERE p.concept_id=c.concept_id)
                 UNION
                 SELECT edge.concept_id,selected.depth+1
                 FROM selected JOIN authority.concept_parent edge ON edge.parent_concept_id=selected.concept_id
                 WHERE selected.depth<{CLIENT_TAXONOMY_DEPTH}
             ), ancestors(concept_id) AS (
                 SELECT concept_id FROM selected
                 UNION
                 SELECT edge.parent_concept_id
                 FROM authority.concept_parent edge JOIN ancestors ON edge.concept_id=ancestors.concept_id
             )
             SELECT DISTINCT concept_id FROM ancestors;
             INSERT INTO taxonomy_meta SELECT key,value FROM authority.taxonomy_meta;
             INSERT INTO taxonomy_meta(key,value) VALUES('client_seed_depth','{CLIENT_TAXONOMY_DEPTH}');
             INSERT INTO concept SELECT concept_id,preferred_label,normalized_label FROM authority.concept WHERE concept_id IN client_concept ORDER BY rowid;
             INSERT INTO concept_parent SELECT concept_id,parent_concept_id,ordinal FROM authority.concept_parent WHERE concept_id IN client_concept ORDER BY concept_id,ordinal;
             INSERT INTO lcc_selector SELECT concept_id,selector FROM authority.lcc_selector WHERE concept_id IN client_concept ORDER BY concept_id,selector;
             INSERT INTO bisac_selector SELECT concept_id,selector FROM authority.bisac_selector WHERE concept_id IN client_concept ORDER BY concept_id,selector;
             INSERT INTO ddc_selector SELECT selector,concept_id FROM authority.ddc_selector WHERE concept_id IN client_concept ORDER BY selector;
             INSERT INTO lcc_range SELECT concept_id,start_letters,start_number,start_cutters,end_letters,end_number,end_cutters FROM authority.lcc_range WHERE concept_id IN client_concept ORDER BY concept_id,start_letters,start_number,start_cutters,end_letters,end_number,end_cutters;"
        ))
        .expect("populate client taxonomy seed");
    client.execute_batch("DETACH DATABASE authority; VACUUM; PRAGMA optimize;").expect("finalize client taxonomy seed");
    let concept_count: i64 = client.query_row("SELECT count(*) FROM concept", [], |row| row.get(0)).expect("count client concepts");
    assert!((1..=2_000).contains(&concept_count), "client taxonomy seed unexpectedly contains {concept_count} concepts");
}
