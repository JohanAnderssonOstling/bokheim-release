use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};
use update_client::{ApplicationRelease, SIGNATURE_CONTEXT};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Fixture {
    temp: tempfile::TempDir,
    config: Config,
    manifest: Manifest,
    key: Ed25519KeyPair,
}
impl Fixture {
    fn new() -> Self {
        let key = Ed25519KeyPair::from_seed_unchecked(&[42; 32]).unwrap();
        let config = Config {
            endpoint: "https://updates.example.org/v1/stable".into(),
            channel: "stable".into(),
            trusted_keys: [("test".into(), hex(key.public_key().as_ref()))].into(),
            origins: [("https://updates.example.org".into(), "updates".into())].into(),
            external_artifact_prefixes: Vec::new(),
        };
        let artifact = Artifact { url: "https://updates.example.org/releases/2.0/app.bin".into(), bytes: 3, sha256: hex(ring::digest::digest(&SHA256, b"app").as_ref()) };
        let manifest = Manifest {
            protocol_version: 1,
            channel: "stable".into(),
            sequence: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
            application: Some(ApplicationRelease { version: "2.0.0".parse().unwrap(), name: "Release".into(), schema_version: 1, taxonomy_format_versions: vec![1], artifacts: [("linux".into(), artifact)].into() }),
            taxonomy: None,
        };
        let fixture = Self { temp: tempfile::tempdir().unwrap(), config, manifest, key };
        fs::create_dir_all(fixture.bundle().join("artifacts/updates/releases/2.0")).unwrap();
        fs::write(fixture.bundle().join("artifacts/updates/releases/2.0/app.bin"), b"app").unwrap();
        fixture
    }
    fn bundle(&self) -> PathBuf {
        self.temp.path().join("bundle")
    }
    fn root(&self) -> PathBuf {
        self.temp.path().join("host")
    }
    fn feed(&self) -> PathBuf {
        self.root().join("public/updates/v1/stable")
    }
    fn envelope(&self) -> Vec<u8> {
        let payload = serde_json::to_string(&self.manifest).unwrap();
        let mut message = SIGNATURE_CONTEXT.to_vec();
        message.extend_from_slice(payload.as_bytes());
        serde_json::to_vec(&SignedManifest { key_id: "test".into(), signature: hex(self.key.sign(&message).as_ref()), payload }).unwrap()
    }
    fn run(&self, now: u64) -> Result<()> {
        fs::write(self.bundle().join("manifest.signed.json"), self.envelope())?;
        publish(&self.config, &self.bundle(), &self.root(), now)
    }
}

#[test]
fn publish_is_idempotent_and_reuses_immutable_artifacts() {
    let mut f = Fixture::new();
    f.run(150).unwrap();
    assert_eq!(fs::read(f.feed()).unwrap(), f.envelope());
    fs::remove_dir_all(f.bundle().join("artifacts")).unwrap();
    f.run(150).unwrap();
    f.manifest.sequence = 2;
    f.run(150).unwrap();
    assert_eq!(fs::read(f.feed()).unwrap(), f.envelope());
    assert!(f.root().join("history/stable/1.json").exists());
}

#[test]
fn taxonomy_only_and_withdrawal_preserve_downloads() {
    let mut f = Fixture::new();
    let mut artifact = f.manifest.application.take().unwrap().artifacts["linux"].clone();
    artifact.url = "https://taxonomy.example.org/releases/42/snapshot.sqlite3".into();
    f.config.origins.insert("https://taxonomy.example.org".into(), "taxonomy".into());
    f.manifest.taxonomy = Some(update_client::TaxonomyRelease { release_id: 42, format_version: 1, minimum_application_version: "1.0.0".parse().unwrap(), minimum_schema_version: 1, artifact });
    let directory = f.bundle().join("artifacts/taxonomy/releases/42");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("snapshot.sqlite3"), b"app").unwrap();
    f.run(150).unwrap();
    f.manifest.sequence = 2;
    f.manifest.taxonomy = None;
    f.run(150).unwrap();
    assert_eq!(fs::read(f.feed()).unwrap(), f.envelope());
    assert!(f.root().join("public/taxonomy/releases/42/snapshot.sqlite3").exists());
}

#[test]
fn concurrent_publication_is_rejected() {
    let f = Fixture::new();
    fs::create_dir_all(f.root()).unwrap();
    let lock = File::create(f.root().join("publish.lock")).unwrap();
    lock.try_lock_exclusive().unwrap();
    assert!(f.run(150).is_err());
    assert!(!f.feed().exists());
    drop(lock);
    f.run(150).unwrap();
}

#[test]
fn missing_or_corrupt_artifact_never_replaces_feed() {
    let mut f = Fixture::new();
    f.run(150).unwrap();
    let old = fs::read(f.feed()).unwrap();
    f.manifest.sequence = 2;
    let app = f.manifest.application.as_mut().unwrap();
    let mut extra = app.artifacts["linux"].clone();
    extra.url = "https://updates.example.org/releases/2.1/app.bin".into();
    app.artifacts.insert("android".into(), extra);
    assert!(f.run(150).is_err());
    fs::create_dir_all(f.bundle().join("artifacts/updates/releases/2.1")).unwrap();
    fs::write(f.bundle().join("artifacts/updates/releases/2.1/app.bin"), b"bad").unwrap();
    assert!(f.run(150).is_err());
    assert_eq!(fs::read(f.feed()).unwrap(), old);
    fs::write(f.bundle().join("artifacts/updates/releases/2.1/app.bin"), b"app").unwrap();
    f.run(150).unwrap();
}

#[test]
fn expired_feed_still_prevents_replay_and_sequence_reuse() {
    let mut f = Fixture::new();
    f.manifest.sequence = 9;
    f.run(150).unwrap();
    f.manifest.issued_at_unix = 250;
    f.manifest.expires_at_unix = 350;
    assert!(f.run(300).is_err());
    f.manifest.sequence = 8;
    assert!(f.run(300).is_err());
    f.manifest.sequence = 10;
    f.run(300).unwrap();
}

#[test]
fn immutable_collision_and_bad_signature_are_rejected() {
    let mut f = Fixture::new();
    f.run(150).unwrap();
    let old = fs::read(f.feed()).unwrap();
    f.manifest.sequence = 2;
    f.manifest.application.as_mut().unwrap().artifacts.get_mut("linux").unwrap().sha256 = "00".repeat(32);
    assert!(f.run(150).is_err());
    assert_eq!(fs::read(f.feed()).unwrap(), old);
    f.key = Ed25519KeyPair::from_seed_unchecked(&[43; 32]).unwrap();
    assert!(f.run(150).is_err());
    assert_eq!(fs::read(f.feed()).unwrap(), old);
}

#[test]
fn unsafe_paths_and_unknown_origins_are_rejected() {
    let f = Fixture::new();
    for path in [
        "https://elsewhere.org/releases/1/app",
        "https://updates.example.org/releases/../v1/stable",
        "https://updates.example.org/releases/1/%2e%2e/app",
        "https://updates.example.org/releases/1/app?x",
        "https://updates.example.org/v1/stable",
    ] {
        assert!(f.config.path(path, true).is_err(), "{path}");
    }
}

#[test]
fn external_assets_are_checked_before_feed_promotion_and_on_retry() {
    let mut f = Fixture::new();
    f.run(150).unwrap();
    let previous = fs::read(f.feed()).unwrap();
    f.config.external_artifact_prefixes.push("https://github.com/bokheim/releases/releases/download/".into());
    f.manifest.sequence = 2;
    f.manifest.application.as_mut().unwrap().artifacts.get_mut("linux").unwrap().url =
        "https://github.com/bokheim/releases/releases/download/v2.0.0/Bokheim.AppImage".into();
    fs::write(f.bundle().join("manifest.signed.json"), f.envelope()).unwrap();
    let failed = publish_with_verifier(&f.config, &f.bundle(), &f.root(), 150, |_| bail!("not publicly available"));
    assert!(failed.is_err());
    assert_eq!(fs::read(f.feed()).unwrap(), previous);
    assert!(!f.root().join("history/stable/2.json").exists());
    let mut checked = 0;
    for _ in 0..2 {
        publish_with_verifier(&f.config, &f.bundle(), &f.root(), 150, |artifact| {
            // Exercise the same authenticated size/hash checks as local files.
            checked += 1;
            copy_verified(regular(&f.bundle().join("artifacts/updates/releases/2.0/app.bin"))?, std::io::sink(), artifact)
        }).unwrap();
    }
    assert_eq!(checked, 2);
    assert_eq!(fs::read(f.feed()).unwrap(), f.envelope());
}

#[test]
fn external_allowlist_has_directory_boundaries_and_canonical_urls() {
    let mut f = Fixture::new();
    f.config.external_artifact_prefixes = vec!["https://github.com/owner/repo/releases/download/".into()];
    f.config.discovery().unwrap();
    assert!(f.config.external("https://github.com/owner/repo/releases/download/v1/app.bin").unwrap());
    assert!(!f.config.external("https://updates.example.org/releases/v1/app.bin").unwrap());
    for url in [
        "https://github.com/owner/repo-other/releases/download/v1/app.bin",
        "https://github.com/owner/repo/releases/download/../private/app.bin",
        "https://github.com/owner/repo/releases/download/v1/%2e%2e/app.bin",
        "https://github.com/owner/repo/releases/download/v1/app.bin?token=secret",
        "https://github.com/owner/repo/releases/download/v1/app.bin#fragment",
        "https://github.com/owner/repo/releases/download//app.bin",
        "https://github.com@elsewhere.org/owner/repo/releases/download/v1/app.bin",
        "http://github.com/owner/repo/releases/download/v1/app.bin",
    ] {
        assert!(f.config.external(url).is_err(), "{url}");
    }
    for prefix in ["https://github.com/", "https://github.com/owner/repo", "http://github.com/owner/repo/", "https://github.com/owner/../repo/", "https://updates.example.org/releases/"] {
        f.config.external_artifact_prefixes = vec![prefix.into()];
        assert!(f.config.discovery().is_err(), "{prefix}");
    }
}

#[cfg(unix)]
#[test]
fn symlinked_public_directory_is_rejected() {
    let f = Fixture::new();
    fs::create_dir_all(f.root()).unwrap();
    std::os::unix::fs::symlink(f.bundle(), f.root().join("public")).unwrap();
    assert!(f.run(150).is_err());
    assert!(!f.bundle().join("updates").exists());
}
