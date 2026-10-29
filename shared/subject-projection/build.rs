use flate2::{write::GzEncoder, Compression};
use rusqlite::Connection;
use std::env;
use std::io::Write;
use std::path::PathBuf;
use subject_taxonomy::{runtime_index as runtime, taxonomy_reader};

const CLIENT_TAXONOMY_DEPTH: u8 = 2;

fn main() {
    for path in ["build.rs", "src", "data"] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-changed=data/unified-taxonomy-v2.sqlite3");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_AUTHORITATIVE_TAXONOMY");
    println!("cargo:rerun-if-env-changed=BOKHEIM_BUILD_TAXONOMY_SQLITE");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let database_path = env::var_os("BOKHEIM_BUILD_TAXONOMY_SQLITE").map(PathBuf::from).unwrap_or_else(|| manifest_dir.join("data/unified-taxonomy-v2.sqlite3"));
    println!("cargo:rerun-if-changed={}", database_path.display());
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
    let bisac_csv = std::fs::read_to_string(manifest_dir.join("data/bisac-2021.csv")).expect("read BISAC reference");
    let bisac_revision = blake3::hash(format!("bisac-reference-v1\n{bisac_csv}").as_bytes()).to_hex().to_string();
    let bisac_path = output_dir.join("bisac-reference.sqlite3");
    if bisac_path.exists() {
        std::fs::remove_file(&bisac_path).expect("replace generated BISAC reference");
    }
    subject_taxonomy::bisac_reference::compile(&bisac_path, &bisac_csv, &bisac_revision).expect("compile BISAC reference");
    // Compile an allocation-free source for fallback lookups too. Both forms
    // come from the same rows, including duplicate-code ordering and literal
    // path components; a broken runtime SQLite cache cannot lose assignments.
    let bisac = Connection::open(&bisac_path).expect("open compiled BISAC reference");
    let mut query = bisac.prepare("SELECT path,code,normalized_path,tail_component,component_count FROM bisac_node ORDER BY id").expect("read compiled BISAC nodes");
    let nodes = query
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?)))
        .expect("query compiled BISAC nodes")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect compiled BISAC nodes");
    let nodes = nodes
        .into_iter()
        .map(|(path, code, normalized_path, tail_component, component_count)| {
            format!("BundledNode {{ path: {path:?}, code: {code:?}, normalized_path: {normalized_path:?}, tail_component: {tail_component:?}, component_count: {component_count} }}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    std::fs::write(output_dir.join("bisac-reference-nodes.rs"), format!("&[\n{nodes}\n]\n")).expect("write bundled BISAC nodes");
    drop(query);
    drop(bisac);
    let bisac_digest = blake3::hash(&std::fs::read(&bisac_path).expect("read compiled BISAC reference")).to_hex().to_string();
    // Finish the cache metadata before packaging. Native and web can install
    // this exact image without SQLite backups or a post-install write.
    let bisac = Connection::open(&bisac_path).expect("open BISAC reference metadata");
    bisac.execute("UPDATE reference_meta SET revision=?1", [&bisac_digest]).expect("record BISAC artifact identity");
    drop(bisac);
    let bisac_image = std::fs::read(&bisac_path).expect("read prepared BISAC reference");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&bisac_image).expect("compress BISAC reference");
    let compressed = encoder.finish().expect("finish BISAC reference compression");
    let compressed_digest = blake3::hash(&compressed).to_hex().to_string();
    std::fs::write(output_dir.join("bisac-reference.sqlite3.gz"), compressed).expect("write compressed BISAC reference");
    std::fs::write(output_dir.join("bisac-reference-gzip-digest.txt"), compressed_digest).expect("write compressed BISAC digest");
    std::fs::write(output_dir.join("bisac-reference-size.rs"), format!("{}usize", bisac_image.len())).expect("write BISAC reference size");
    std::fs::write(output_dir.join("bisac-reference-revision.txt"), bisac_revision).expect("write BISAC revision");
    std::fs::write(output_dir.join("bisac-reference-digest.txt"), bisac_digest).expect("write BISAC digest");
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
    // Compact the final database before packaging it as a standalone resource.
    client_connection.execute_batch("VACUUM;").expect("compact bundled taxonomy database");
    let concepts = taxonomy_reader::read_unified_taxonomy_connection(&client_connection, "build-time taxonomy").expect("read bundled taxonomy");
    let matcher = runtime::build_runtime(concepts).expect("compile bundled taxonomy matcher");
    let encoded = bincode::serde::encode_to_vec(&matcher, bincode::config::standard()).expect("encode taxonomy matcher");
    std::fs::write(output_dir.join("unified-taxonomy-matcher.bin"), encoded).expect("write taxonomy matcher");
    std::fs::write(output_dir.join("unified-taxonomy-revision-id.txt"), &matcher.revision_id).expect("write taxonomy revision");
    std::fs::write(output_dir.join("taxonomy-release.rs"), format!("{published_release_id}u64")).expect("write taxonomy release");
    std::fs::write(output_dir.join("taxonomy-format.rs"), format!("{format_version}u32")).expect("write taxonomy format");
    let image = std::fs::read(&client_database_path).expect("read prepared taxonomy");
    let digest = blake3::hash(&image).to_hex().to_string();
    std::fs::write(output_dir.join("taxonomy.digest"), &digest).expect("write taxonomy digest");
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("wasm32") {
        // OUT_DIR is <profile>/build/<crate>/out. Cargo-run and all native
        // packagers use the same prepared database beside the executable.
        let profile = output_dir.ancestors().nth(3).expect("Cargo profile directory");
        for (name, bytes) in [("taxonomy.sqlite3", image.as_slice()), ("taxonomy.digest", digest.as_bytes())] {
            let destination = profile.join(name);
            if std::fs::read(&destination).ok().as_deref() != Some(bytes) {
                let staging = profile.join(format!(".{name}.{}", std::process::id()));
                std::fs::write(&staging, bytes).expect("stage bundled taxonomy resource");
                std::fs::rename(staging, destination).expect("publish bundled taxonomy resource");
            }
        }
    }
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
