//! Release-only integration fixture: real Windows EXEs, helper handoff and rollback.
//! Never distributed. Run on Windows or with the Windows Cargo runner under Wine.
#[cfg(not(windows))]
fn main() { eprintln!("Run this fixture for x86_64-pc-windows-msvc."); }

#[cfg(windows)]
fn main() {
    let result = if std::env::var_os("BOKHEIM_UPDATE_FIXTURE_DATA").is_some() { fixture::client() } else { fixture::controller() };
    if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
}

#[cfg(windows)]
mod fixture {
    use fs2::FileExt;
    use linux_update_host::{now, prepare_candidate, windows::{WindowsHost, WindowsStartup}, windows_process};
    use ring::{digest::{digest, SHA256}, signature::{Ed25519KeyPair, KeyPair}};
    use std::{fs::{self, File, OpenOptions}, io::{Cursor, Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, process::Command, time::{Duration, Instant}};
    use update_client::{ApplicationRelease, Artifact, DiscoveryConfig, Manifest, SignedManifest, SIGNATURE_CONTEXT, coordinator::{ArtifactKind, Phase}};

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
    fn key() -> Ed25519KeyPair { Ed25519KeyPair::from_seed_unchecked(&[97; 32]).unwrap() }
    fn config() -> DiscoveryConfig {
        DiscoveryConfig { endpoint: "https://example.org/v1/stable".into(), channel: "stable".into(),
            trusted_keys: [("fixture".into(), key().public_key().as_ref().try_into().unwrap())].into() }
    }
    fn version(executable: &Path) -> Result<String> {
        let mut file = File::open(executable)?;
        file.seek(SeekFrom::End(-40))?;
        let mut bytes = Vec::new(); file.read_to_end(&mut bytes)?;
        let tail = String::from_utf8_lossy(&bytes);
        Ok(tail.rsplit_once("BOKHEIM_FIXTURE_VERSION=").ok_or("missing fixture version")?.1.trim().into())
    }
    fn lock(data: &Path) -> Result<File> {
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(data.join("desktop-instance.lock"))?;
        FileExt::try_lock_exclusive(&file)?;
        Ok(file)
    }
    pub fn client() -> Result<()> {
        assert!(!cfg!(debug_assertions), "fixture must be optimized");
        let data = PathBuf::from(std::env::var_os("BOKHEIM_UPDATE_FIXTURE_DATA").unwrap());
        let executable = std::env::current_exe()?;
        let version = version(&executable)?;
        let args: Vec<_> = std::env::args_os().collect();
        if let Some(pair) = args.windows(2).find(|v| v[0] == "--bokheim-prepare-update") {
            prepare_candidate(Path::new(&pair[1]), &version)?;
            return Ok(());
        }
        if windows_process::helper_entry(&data, &version, || Ok(config()))? { return Ok(()); }
        let Some(entry) = windows_process::enter(&data)? else { return Ok(()); };
        let _desktop = lock(&data)?;
        let trial = entry.acquired()?;
        let mut host = WindowsHost::open(&data, executable.parent().unwrap(), &version, config())?;
        if matches!(host.startup(trial.as_deref())?, WindowsStartup::Helper) {
            windows_process::spawn_helper(&data, executable.parent().unwrap())?;
            return Ok(());
        }
        if host.is_trial() && fs::read_to_string(data.join("scenario"))? == "crash" {
            rusqlite::Connection::open(data.join("app-state-v1.sqlite3"))?.execute_batch("UPDATE setting SET v='trial'")?;
            std::process::exit(43);
        }
        host.healthy()?;
        let value: String = rusqlite::Connection::open(data.join("app-state-v1.sqlite3"))?
            .query_row("SELECT v FROM setting", [], |r| r.get(0))?;
        update_client::activation::write_json(&data.join("observed.json"), &(version, value))?;
        let deadline = Instant::now() + Duration::from_secs(60);
        while !data.join("exit").exists() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(25)); }
        Ok(())
    }

    pub fn controller() -> Result<()> {
        assert!(!cfg!(debug_assertions), "fixture must be optimized");
        for scenario in ["healthy", "crash"] { scenario_trial(scenario)?; }
        Ok(())
    }
    fn scenario_trial(scenario: &str) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let installation = temp.path().join("app"); fs::create_dir(&installation)?;
        let data = temp.path().join("data"); fs::create_dir(&data)?;
        fs::write(data.join("scenario"), scenario)?;
        let executable = installation.join("Bokheim.exe");
        let original = fs::read(std::env::current_exe()?)?;
        let mut old = original.clone(); old.extend_from_slice(b"\nBOKHEIM_FIXTURE_VERSION=1.0.0\n");
        fs::write(&executable, &old)?;
        let mut next = original; next.extend_from_slice(b"\nBOKHEIM_FIXTURE_VERSION=2.0.0\n");
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("Bokheim.exe", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated))?;
        zip.write_all(&next)?;
        let bytes = zip.finish()?.into_inner();
        rusqlite::Connection::open(data.join("app-state-v1.sqlite3"))?.execute_batch("CREATE TABLE setting(v); INSERT INTO setting VALUES('old')")?;
        let artifact = Artifact { url: "https://example.org/releases/2/app.zip".into(), bytes: bytes.len() as u64, sha256: hex(digest(&SHA256, &bytes).as_ref()) };
        let manifest = Manifest {
            protocol_version: 1, channel: "stable".into(), sequence: 1, issued_at_unix: now(), expires_at_unix: now() + 3600, taxonomy: None,
            application: Some(ApplicationRelease { version: "2.0.0".parse()?, name: "Fixture".into(),
                schema_version: library_database::LIBRARY_SCHEMA_VERSION as u32, taxonomy_format_versions: vec![1],
                artifacts: [("windows-x86_64-zip".into(), artifact)].into() }),
        };
        let payload = serde_json::to_string(&manifest)?;
        let mut message = SIGNATURE_CONTEXT.to_vec(); message.extend_from_slice(payload.as_bytes());
        let envelope = serde_json::to_vec(&SignedManifest { key_id: "fixture".into(), payload, signature: hex(key().sign(&message).as_ref()) })?;
        let mut host = WindowsHost::open(&data, &installation, "1.0.0", config())?;
        host.coordinator.discovered(envelope, now())?; host.approve()?;
        let id = host.coordinator.record().approved.as_ref().unwrap().id.clone();
        let stage = data.join("updates/staging").join(&id); fs::create_dir_all(&stage)?;
        fs::write(stage.join("application.bin"), bytes)?;
        host.coordinator.artifact_prepared(ArtifactKind::Application)?;
        drop(host);
        let mut launcher = Command::new(&executable).env("BOKHEIM_UPDATE_FIXTURE_DATA", &data).spawn()?;
        let deadline = Instant::now() + Duration::from_secs(60);
        while !data.join("observed.json").exists() {
            if Instant::now() >= deadline { fs::write(data.join("exit"), [])?; return Err(format!("{scenario}: no application became healthy").into()); }
            std::thread::sleep(Duration::from_millis(50));
        }
        let observed: (String, String) = update_client::activation::read_json(&data.join("observed.json"))?;
        fs::write(data.join("exit"), [])?;
        launcher.wait()?;
        let expected = if scenario == "healthy" { "2.0.0" } else { "1.0.0" };
        assert_eq!(observed, (expected.into(), "old".into()));
        assert_eq!(version(&executable)?, expected);
        let host = WindowsHost::open(&data, &installation, expected, config())?;
        if scenario == "healthy" { assert!(host.coordinator.record().approved.is_none()); }
        else { assert_eq!(host.coordinator.record().approved.as_ref().unwrap().phase, Phase::Quarantined); }
        drop(host);
        let quiet_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let gate = OpenOptions::new().read(true).write(true).open(data.join("windows-update-launch.lock"))?;
            let desktop = OpenOptions::new().read(true).write(true).open(data.join("desktop-instance.lock"))?;
            if FileExt::try_lock_exclusive(&gate).is_ok() && FileExt::try_lock_exclusive(&desktop).is_ok() { break; }
            if Instant::now() >= quiet_deadline { return Err("helper or desktop did not release ownership".into()); }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Both the trial/stable process and copied helper must exit before
        // removing Windows executable files and SQLite handles.
        let cleanup_deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match fs::remove_dir_all(temp.path()) {
                Ok(()) => break,
                Err(e) if Instant::now() >= cleanup_deadline => return Err(e.into()),
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        println!("{scenario}: real executable handoff, migration and recovery passed");
        Ok(())
    }
}
