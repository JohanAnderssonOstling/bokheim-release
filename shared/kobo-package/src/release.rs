use semver::Version;
use serde::{Deserialize, Serialize};
use std::io::Read;
use update_protocol::{Artifact, MAX_MANIFEST_BYTES};

pub const RELEASE_API: &str = "https://api.github.com/repos/JohanAnderssonOstling/bokheim-release/releases/latest";
const ASSET_NAME: &str = "Bokheim-Kobo-armv7.tar.gz";
const LEGACY_ASSET_NAME: &str = "Bokheim-Kobo-Libra2-armv7.tar.gz";
const DOWNLOAD_PREFIX: &str = "https://github.com/JohanAnderssonOstling/bokheim-release/releases/download/";
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// Keep the durable ready.json format compatible with earlier Kobo launchers.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Release {
    pub version: Version,
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

impl Release {
    pub fn artifact(&self) -> Artifact {
        Artifact { url: self.url.clone(), bytes: self.bytes, sha256: self.sha256.clone() }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.artifact().validate()?;
        if !self.url.starts_with(DOWNLOAD_PREFIX) {
            return Err("The Kobo update has an unexpected download address".into());
        }
        if self.bytes > MAX_DOWNLOAD_BYTES {
            return Err("The Kobo package is unexpectedly large".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

/// HTTP clients stay in their platform adapters; parsing and limits are shared.
pub fn newer_release(input: impl Read, installed: &Version) -> Result<Option<Release>, String> {
    parse_release(input, installed, None)
}

pub fn newer_release_for_model(input: impl Read, installed: &Version, model_id: u16) -> Result<Option<Release>, String> {
    if kobo_profile::Profile::from_id(model_id).is_none() {
        return Err("This Kobo has no supported device profile".into());
    }
    parse_release(input, installed, Some(model_id))
}

fn parse_release(input: impl Read, installed: &Version, model_id: Option<u16>) -> Result<Option<Release>, String> {
    let bytes = update_files::read_bounded(input, MAX_MANIFEST_BYTES).map_err(|error| format!("Could not read the release information: {error}"))?;
    let release: GithubRelease = serde_json::from_slice(&bytes).map_err(|error| format!("Could not read the release information: {error}"))?;
    let version = Version::parse(release.tag_name.trim_start_matches('v')).map_err(|error| error.to_string())?;
    if version <= *installed {
        return Ok(None);
    }
    let mut assets = release.assets;
    let index = assets.iter().position(|asset| asset.name == ASSET_NAME)
        .or_else(|| (model_id == Some(388)).then(|| assets.iter().position(|asset| asset.name == LEGACY_ASSET_NAME)).flatten())
        .ok_or("The latest release has no compatible multi-device Kobo package yet. Try again later.")?;
    let asset = assets.swap_remove(index);
    let sha256 = asset.digest.and_then(|digest| digest.strip_prefix("sha256:").map(str::to_owned)).ok_or("The Kobo package has no verification checksum")?;
    let release = Release { version, url: asset.browser_download_url, bytes: asset.size, sha256 };
    release.validate()?;
    Ok(Some(release))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(names: &[&str]) -> Vec<u8> {
        let assets: Vec<_> = names.iter().map(|name| serde_json::json!({"name": name, "browser_download_url": format!("{DOWNLOAD_PREFIX}v2.0.0/{name}"), "size": 1024, "digest": format!("sha256:{}", "ab".repeat(32))})).collect();
        serde_json::to_vec(&serde_json::json!({"tag_name": "v2.0.0", "assets": assets})).unwrap()
    }
    #[test]
    fn usb_only_downloads_universal_and_libra_updates_can_migrate_from_legacy() {
        let installed = Version::new(1, 0, 0);
        let legacy = release(&[LEGACY_ASSET_NAME]);
        assert!(newer_release(legacy.as_slice(), &installed).is_err());
        assert!(newer_release_for_model(legacy.as_slice(), &installed, 376).is_err());
        assert!(newer_release_for_model(legacy.as_slice(), &installed, 388).unwrap().is_some());
        let both = release(&[LEGACY_ASSET_NAME, ASSET_NAME]);
        for profile in kobo_profile::PROFILES {
            let release = newer_release_for_model(both.as_slice(), &installed, profile.id).unwrap().unwrap();
            assert!(release.url.ends_with(ASSET_NAME));
        }
        assert!(newer_release(both.as_slice(), &Version::new(2, 0, 0)).unwrap().is_none());
    }
}
