use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn fixture() -> (Ed25519KeyPair, DiscoveryConfig, Manifest) {
    // Fixed fixture key, never a release trust anchor.
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let config = DiscoveryConfig { endpoint: DEFAULT_ENDPOINT.into(), channel: "stable".into(), trusted_keys: [("test".into(), key.public_key().as_ref().try_into().unwrap())].into() };
    let artifact = Artifact { url: "https://downloads.example.org/arbitrary-name.bin".into(), bytes: 3, sha256: "00".repeat(32) };
    let manifest = Manifest {
        protocol_version: 1,
        channel: "stable".into(),
        sequence: 10,
        issued_at_unix: 100,
        expires_at_unix: 200,
        application: Some(ApplicationRelease {
            version: "2.0.0".parse().unwrap(),
            name: "Example release".into(),
            schema_version: 76,
            taxonomy_format_versions: vec![5],
            artifacts: [("android-aarch64-apk".into(), artifact.clone())].into(),
        }),
        taxonomy: Some(TaxonomyRelease { release_id: 9, format_version: 5, minimum_application_version: "2.0.0".parse().unwrap(), minimum_schema_version: 76, artifact }),
    };
    (key, config, manifest)
}
fn signed(key: &Ed25519KeyPair, manifest: &Manifest) -> Vec<u8> {
    let payload = serde_json::to_string(manifest).unwrap();
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend(payload.as_bytes());
    let signature = key.sign(&message).as_ref().iter().map(|b| format!("{b:02x}")).collect();
    serde_json::to_vec(&SignedManifest { key_id: "test".into(), payload, signature }).unwrap()
}

#[test]
fn arbitrary_provider_and_filename_work_with_combined_compatibility() {
    let (key, config, mut manifest) = fixture();
    for host in ["downloads.example.org", "mirror.example.net", "github.com"] {
        manifest.application.as_mut().unwrap().artifacts.values_mut().next().unwrap().url = format!("https://{host}/any/package-name");
        let verified = verify(&signed(&key, &manifest), &config, 150, 10).unwrap();
        let version = "1.0.0".parse().unwrap();
        let installed = Installed { application_version: &version, schema_version: 75, taxonomy_format_versions: &[4], taxonomy_release_id: 8, target: "android-aarch64-apk" };
        let offer = verified.offer(&installed);
        assert!(offer.application.is_some());
        assert!(offer.taxonomy.is_some());
        assert!(offer.application.unwrap().1.url.contains(host));
        let unsupported = Installed { target: "linux-aarch64-appimage", ..installed };
        let offer = verified.offer(&unsupported);
        assert!(offer.application.is_none());
        assert!(offer.taxonomy.is_none());
    }
}

#[test]
fn taxonomy_only_and_no_new_release_are_supported() {
    let (key, config, mut manifest) = fixture();
    manifest.application = None;
    let verified = verify(&signed(&key, &manifest), &config, 150, 0).unwrap();
    let version = "2.0.0".parse().unwrap();
    let installed = Installed { application_version: &version, schema_version: 76, taxonomy_format_versions: &[5], taxonomy_release_id: 8, target: "android-aarch64-apk" };
    assert!(verified.offer(&installed).taxonomy.is_some());
    assert!(verified.offer(&Installed { taxonomy_release_id: 9, ..installed }).taxonomy.is_none());
    manifest.taxonomy = None; // A signed withdrawal can remove an offer.
    assert!(verify(&signed(&key, &manifest), &config, 150, 0).unwrap().offer(&installed).taxonomy.is_none());
}

#[test]
fn tampering_unknown_keys_unsigned_and_oversized_metadata_fail_closed() {
    let (key, mut config, manifest) = fixture();
    let raw = signed(&key, &manifest);
    let mut envelope: SignedManifest = serde_json::from_slice(&raw).unwrap();
    envelope.payload = envelope.payload.replace("2.0.0", "3.0.0");
    assert!(verify(&serde_json::to_vec(&envelope).unwrap(), &config, 150, 0).is_err());
    assert!(verify(&serde_json::to_vec(&manifest).unwrap(), &config, 150, 0).is_err());
    assert!(verify(&vec![b' '; MAX_MANIFEST_BYTES + 1], &config, 150, 0).is_err());
    config.trusted_keys.clear();
    assert!(verify(&raw, &config, 150, 0).is_err());
    config.trusted_keys.insert("other".into(), [1; 32]);
    assert!(verify(&raw, &config, 150, 0).is_err());
}

#[test]
fn rejects_expired_future_replayed_and_wrong_protocol_metadata() {
    let (key, config, mut manifest) = fixture();
    let raw = signed(&key, &manifest);
    for (now, floor) in [(99, 0), (200, 0), (150, 11)] {
        assert!(verify(&raw, &config, now, floor).is_err());
    }
    manifest.protocol_version = 2;
    assert!(verify(&signed(&key, &manifest), &config, 150, 0).is_err());
    manifest.protocol_version = 1;
    manifest.channel = "beta".into();
    assert!(verify(&signed(&key, &manifest), &config, 150, 0).is_err());
}

#[test]
fn rejects_unsafe_urls_hashes_and_prereleases() {
    let (key, config, mut manifest) = fixture();
    for url in ["http://host/file", "file:///etc/passwd", "https://user:pass@host/file", "https://host/file#fragment"] {
        manifest.taxonomy.as_mut().unwrap().artifact.url = url.into();
        assert!(verify(&signed(&key, &manifest), &config, 150, 0).is_err());
    }
    manifest.taxonomy = None;
    manifest.application.as_mut().unwrap().version = "2.0.0-beta.1".parse().unwrap();
    assert!(verify(&signed(&key, &manifest), &config, 150, 0).is_err());
    manifest.application.as_mut().unwrap().version = "2.0.0".parse().unwrap();
    manifest.application.as_mut().unwrap().artifacts.values_mut().next().unwrap().sha256 = "bad".into();
    assert!(verify(&signed(&key, &manifest), &config, 150, 0).is_err());
}
