use super::*;
use ring::{
    digest::{digest, SHA256},
    signature::{Ed25519KeyPair, KeyPair},
};
use std::os::unix::fs::PermissionsExt;
use update_client::{ApplicationRelease, Artifact, Manifest, SignedManifest, TaxonomyRelease, SIGNATURE_CONTEXT};
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn artifact(url: &str, bytes: &[u8]) -> Artifact {
    Artifact { url: url.into(), bytes: bytes.len() as u64, sha256: hex(digest(&SHA256, bytes).as_ref()) }
}
fn approve(host: &mut Host, key: &Ed25519KeyPair, manifest: &Manifest, app: &[u8], taxonomy: &[u8]) {
    let payload = serde_json::to_string(manifest).unwrap();
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(payload.as_bytes());
    let bytes = serde_json::to_vec(&SignedManifest { key_id: "fixture".into(), signature: hex(key.sign(&message).as_ref()), payload }).unwrap();
    host.coordinator.discovered(bytes, now()).unwrap();
    host.approve().unwrap();
    // The UI must be able to report preparation before any download completes.
    assert_eq!(host.view(), UpdateView::Preparing);
    assert!(host.coordinator.record().approved.as_ref().unwrap().completed.is_empty());
    let staging = host.root.join("staging").join(&host.coordinator.record().approved.as_ref().unwrap().id);
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("application.bin"), app).unwrap();
    fs::write(staging.join("taxonomy.sqlite3"), taxonomy).unwrap();
    host.coordinator.artifact_prepared(ArtifactKind::Application).unwrap();
    host.coordinator.artifact_prepared(ArtifactKind::Taxonomy).unwrap();
}

#[test]
fn candidate_process() {
    if let Some(path) = std::env::var_os("BOKHEIM_FIXTURE_JOB") {
        prepare_candidate(Path::new(&path), "2.0.0").unwrap();
    }
}

#[test]
fn combined_update_recovers_interruption_then_commits_candidate_migrations() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(data.join("libraries/one")).unwrap();
    let library = data.join("libraries/one/library.db");
    library_database::Database::prepare_update(&library).unwrap();
    let connection = library_database::open_fixture_connection(&library).unwrap();
    connection.execute("UPDATE sync_metadata SET change_origin='remote' WHERE singleton=1", []).unwrap();
    let (manual_id, manual_path): (i64, String) = connection.query_row("SELECT concept_id,path FROM curated.unified_concept_paths ORDER BY route_id LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let mut raw_books = Vec::new();
    for (id, code) in [(1, "QA76"), (2, "QZ999")] {
        let mut book = book_model::BookMetadata::default();
        book.subjects = vec![book_model::BookSubject::new(None, code, "marc", Some("lcc".into()), Some(code.into())).unwrap()];
        let raw = sync_common::wire::encode(&book).unwrap();
        connection.execute("INSERT INTO book(row_id,content_hash,book_metadata) VALUES(?1,?2,?3)", rusqlite::params![id, format!("{id:064x}"), raw]).unwrap();
        raw_books.push(raw);
    }
    let mut manual = book_model::BookMetadata::default();
    manual.subjects = vec![book_model::BookSubject::new(None, &manual_path, "manual:subject", Some("unified".into()), Some(manual_path.clone())).unwrap()];
    let raw = sync_common::wire::encode(&manual).unwrap();
    connection.execute("INSERT INTO book(row_id,content_hash,book_metadata) VALUES(3,?1,?2)", rusqlite::params!["c".repeat(64), raw]).unwrap();
    raw_books.push(raw);
    connection.execute("DELETE FROM local_taxonomy_projection", []).unwrap();
    drop(connection);
    library_database::Database::prepare_update(&library).unwrap();
    let assignments = |path: &Path| {
        let db = rusqlite::Connection::open(path).unwrap();
        let rows =
            db.prepare("SELECT book_row_id,concept_id FROM book_unified_concept ORDER BY book_row_id,concept_id").unwrap().query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        rows
    };
    let prior = assignments(&library);
    let executable = temp.path().join("Bokheim.AppImage");
    fs::write(&executable, b"previous application").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[99; 32]).unwrap();
    let config = DiscoveryConfig { endpoint: "https://updates.example.org/v1/stable".into(), channel: "stable".into(), trusted_keys: [("fixture".into(), key.public_key().as_ref().try_into().unwrap())].into() };
    // The fixture package runs the real candidate entry point in a separate process,
    // keeping the process-wide taxonomy isolated just as an AppImage launch does.
    let test_exe = std::env::current_exe().unwrap();
    let escaped = test_exe.to_str().unwrap().replace('\'', "'\\''");
    let app = format!("#!/bin/sh\nBOKHEIM_FIXTURE_JOB=\"$2\" exec '{escaped}' --exact tests::candidate_process --nocapture\n").into_bytes();
    let source = temp.path().join("taxonomy.sqlite3");
    fs::write(&source, subject_projection::DEFAULT_UNIFIED_TAXONOMY_SQLITE).unwrap();
    let current = device("1.0.0", None).unwrap();
    let release = current.taxonomy_release + 1;
    let db = rusqlite::Connection::open(&source).unwrap();
    db.execute_batch("DELETE FROM lcc_selector; DELETE FROM lcc_range; INSERT INTO concept(concept_id,preferred_label) VALUES(999999999,'New subject'); INSERT INTO lcc_selector(concept_id,selector) VALUES(999999999,'QZ999');").unwrap();
    db.execute("UPDATE concept SET preferred_label='Renamed subject' WHERE concept_id=?1", [manual_id]).unwrap();
    db.execute("UPDATE taxonomy_meta SET value=?1 WHERE key='release_id'", [release.to_string()]).unwrap();
    drop(db);
    let taxonomy = fs::read(&source).unwrap();
    let mut manifest = Manifest {
        protocol_version: 1,
        channel: "stable".into(),
        sequence: 1,
        issued_at_unix: now() - 1,
        expires_at_unix: now() + 3600,
        application: Some(ApplicationRelease {
            version: "2.0.0".parse().unwrap(),
            name: "Fixture".into(),
            schema_version: current.schema,
            taxonomy_format_versions: current.taxonomy_formats,
            artifacts: [(current.target, artifact("https://updates.example.org/releases/2/app", &app))].into(),
        }),
        taxonomy: Some(TaxonomyRelease {
            release_id: release,
            format_version: 1,
            minimum_application_version: "2.0.0".parse().unwrap(),
            minimum_schema_version: current.schema,
            artifact: artifact("https://taxonomy.example.org/releases/2/snapshot", &taxonomy),
        }),
    };
    let open = |version| Host::open(&data, version, config.clone(), Some(executable.clone()), test_exe.clone()).unwrap();
    let mut first = open("1.0.0");
    approve(&mut first, &key, &manifest, &app, &taxonomy);
    assert!(matches!(first.startup(None).unwrap(), Startup::Run));
    drop(first);
    let mut second = open("1.0.0");
    assert!(matches!(second.startup(None).unwrap(), Startup::Relaunch { trial: Some(_), .. }));
    drop(second);
    assert_eq!(fs::read(&executable).unwrap(), app);
    // Power loss before startup acknowledgement: next ordinary launch restores all files.
    let mut recovered = open("1.0.0");
    assert!(matches!(recovered.startup(None).unwrap(), Startup::Run));
    assert_eq!(fs::read(&executable).unwrap(), b"previous application");
    assert_eq!(assignments(&library), prior);
    assert_eq!(recovered.coordinator.record().approved.as_ref().unwrap().phase, Phase::Quarantined);
    // A corrected artifact is a fresh offer requiring fresh explicit approval.
    manifest.sequence = 2;
    let corrected = [app.as_slice(), b"# corrected release\n"].concat();
    manifest.application.as_mut().unwrap().artifacts.values_mut().next().unwrap().clone_from(&artifact("https://updates.example.org/releases/3/app", &corrected));
    approve(&mut recovered, &key, &manifest, &corrected, &taxonomy);
    drop(recovered);
    let mut next = open("1.0.0");
    let Startup::Relaunch { trial: Some(id), .. } = next.startup(None).unwrap() else { panic!("candidate did not activate") };
    drop(next);
    let mut trial = open("2.0.0");
    assert!(matches!(trial.startup(Some(&id)).unwrap(), Startup::Run));
    trial.healthy().unwrap();
    assert!(trial.coordinator.record().approved.is_none());
    assert_eq!(active(&trial.root).unwrap().unwrap().release, release);
    let db = rusqlite::Connection::open(&library).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM book_unified_concept WHERE book_row_id=1", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT concept_id FROM book_unified_concept WHERE book_row_id=2", [], |r| r.get::<_, i64>(0)).unwrap(), 999999999);
    for (index, raw) in raw_books.iter().enumerate() {
        assert_eq!(db.query_row("SELECT book_metadata FROM book WHERE row_id=?1", [(index + 1) as i64], |r| r.get::<_, Vec<u8>>(0)).unwrap(), *raw);
    }
    assert_eq!(db.query_row("SELECT concept_id FROM book_unified_concept WHERE book_row_id=3", [], |r| r.get::<_, i64>(0)).unwrap(), manual_id);
    assert_eq!(db.query_row("SELECT count(*) FROM sync_outbox", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(Journal::load(&trial.root).unwrap().unwrap().state, State::Committed);
    // A second offer can be approved before the first committed receipt is cleared.
    manifest.sequence = 3;
    manifest.application.as_mut().unwrap().version = "3.0.0".parse().unwrap();
    let payload = serde_json::to_string(&manifest).unwrap();
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(payload.as_bytes());
    let envelope = serde_json::to_vec(&SignedManifest { key_id: "fixture".into(), signature: hex(key.sign(&message).as_ref()), payload }).unwrap();
    trial.coordinator.discovered(envelope, now()).unwrap();
    trial.approve().unwrap();
    let pending = trial.coordinator.record().approved.as_ref().unwrap().id.clone();
    let stage = trial.root.join("staging").join(&pending);
    fs::create_dir_all(&stage).unwrap();
    fs::write(stage.join("partial"), b"resume me").unwrap();
    drop(trial);
    let mut reopened = open("2.0.0");
    assert!(matches!(reopened.startup(None).unwrap(), Startup::Run));
    assert_eq!(reopened.coordinator.record().approved.as_ref().unwrap().id, pending);
    assert!(stage.join("partial").exists());
}

#[test]
fn registry_backup_does_not_require_a_library_directory() {
    let temp = tempfile::tempdir().unwrap();
    let registry = temp.path().join("app-state-v1.sqlite3");
    rusqlite::Connection::open(&registry).unwrap().execute_batch("CREATE TABLE setting(v)").unwrap();
    let config = DiscoveryConfig { endpoint: "https://updates.example.org/v1/stable".into(), channel: "stable".into(), trusted_keys: [("test".into(), [1; 32])].into() };
    let host = Host::open(temp.path(), "1.0.0", config, None, std::env::current_exe().unwrap()).unwrap();
    assert_eq!(host.databases().unwrap(), vec![registry]);
}
