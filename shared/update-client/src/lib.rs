//! Provider-independent release discovery. Hosts supply transport and durable
//! state; only authenticated, compatible metadata can produce update offers.
use ring::signature::{UnparsedPublicKey, ED25519};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(all(feature = "blocking", not(target_arch = "wasm32")))]
pub mod blocking;

pub mod coordinator;


#[cfg(all(feature = "native-state", not(target_arch = "wasm32")))]
pub mod native_state;
#[cfg(all(feature = "native-state", any(unix, windows)))]
pub mod activation;
#[cfg(all(feature = "native-state", any(unix, windows)))]
pub mod native_files;
#[cfg(all(feature = "native-state", any(unix, windows)))]
pub mod windows_package;
#[cfg(all(feature = "native-state", any(unix, windows)))]
pub mod supervisor;

pub const DEFAULT_ENDPOINT: &str = "https://updates.bokheim.se/v1/stable";
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const SIGNATURE_CONTEXT: &[u8] = b"bokheim-update-manifest-v1\0";

/// Signature covers SIGNATURE_CONTEXT followed by the exact UTF-8 payload.
/// Transport JSON escaping does not affect the signed payload bytes.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    pub key_id: String,
    pub payload: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub url: String,
    pub bytes: u64,
    pub sha256: String,
}

impl Artifact {
    pub fn validate(&self) -> Result<(), String> {
        validate_https_url(&self.url)?;
        if self.bytes == 0 {
            return Err("update artifact size must be positive".into());
        }
        decode_hex::<32>(&self.sha256)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationRelease {
    pub version: Version,
    pub name: String,
    /// Required resulting library schema, not an instruction to run remote SQL.
    pub schema_version: u32,
    pub taxonomy_format_versions: Vec<u32>,
    /// Stable platform/package IDs, independent of download filenames or host.
    pub artifacts: BTreeMap<String, Artifact>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaxonomyRelease {
    pub release_id: u64,
    pub format_version: u32,
    pub minimum_application_version: Version,
    pub minimum_schema_version: u32,
    pub artifact: Artifact,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub protocol_version: u32,
    pub channel: String,
    /// Monotonically increasing publication number, including withdrawals.
    pub sequence: u64,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub application: Option<ApplicationRelease>,
    pub taxonomy: Option<TaxonomyRelease>,
}

/// Trust comes from the host configuration, never from the downloaded envelope.
#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub endpoint: String,
    pub channel: String,
    pub trusted_keys: BTreeMap<String, [u8; 32]>,
}

impl DiscoveryConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_https_url(&self.endpoint)?;
        if self.channel.is_empty() || self.trusted_keys.is_empty() {
            return Err("update discovery requires a channel and trusted signing key".into());
        }
        Ok(())
    }

    /// Production builds inject a public key; no test or generated key is trusted
    /// by default. Only the endpoint may be overridden at runtime.
    pub fn bundled() -> Result<Self, String> {
        let key = option_env!("BOKHEIM_UPDATE_PUBLIC_KEY_HEX").ok_or("no update signing public key was bundled")?;
        let key_id = option_env!("BOKHEIM_UPDATE_KEY_ID").unwrap_or("release-1");
        let endpoint = option_env!("BOKHEIM_UPDATE_ENDPOINT").unwrap_or(DEFAULT_ENDPOINT).to_owned();
        let config = Self { endpoint, channel: "stable".into(), trusted_keys: [(key_id.into(), decode_hex(key)?)].into() };
        config.validate()?;
        Ok(config)
    }
}

#[derive(Debug)]
pub struct VerifiedManifest(Manifest);

pub struct Installed<'a> {
    pub application_version: &'a Version,
    pub schema_version: u32,
    pub taxonomy_format_versions: &'a [u32],
    pub taxonomy_release_id: u64,
    pub target: &'a str,
}

/// A snapshot of the offer. Persist the authenticated source envelope alongside
/// an approved plan so subsequent publications cannot silently change it.
#[derive(Debug)]
pub struct UpdateOffer<'a> {
    pub application: Option<(&'a ApplicationRelease, &'a Artifact)>,
    pub taxonomy: Option<&'a TaxonomyRelease>,
}

impl VerifiedManifest {
    pub fn manifest(&self) -> &Manifest {
        &self.0
    }

    pub fn application_for(&self, target: &str, installed: &Version) -> Option<(&ApplicationRelease, &Artifact)> {
        let application = self.0.application.as_ref()?;
        if application.version <= *installed {
            return None;
        }
        Some((application, application.artifacts.get(target)?))
    }

    pub fn offer(&self, installed: &Installed<'_>) -> UpdateOffer<'_> {
        let application = self.application_for(installed.target, installed.application_version);
        let (version, schema, formats) =
            application.map_or((installed.application_version, installed.schema_version, installed.taxonomy_format_versions), |(release, _)| (&release.version, release.schema_version, release.taxonomy_format_versions.as_slice()));
        let taxonomy = self
            .0
            .taxonomy
            .as_ref()
            .filter(|release| release.release_id > installed.taxonomy_release_id && version >= &release.minimum_application_version && schema >= release.minimum_schema_version && formats.contains(&release.format_version));
        UpdateOffer { application, taxonomy }
    }
}

/// `minimum_sequence` is the host's last accepted publication number. Persist
/// it after successful verification; reverify cached envelopes on every use.
pub fn verify(bytes: &[u8], config: &DiscoveryConfig, now_unix: u64, minimum_sequence: u64) -> Result<VerifiedManifest, String> {
    config.validate()?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("update manifest exceeds size limit".into());
    }
    let envelope: SignedManifest = serde_json::from_slice(bytes).map_err(|e| format!("invalid signed update envelope: {e}"))?;
    let key = config.trusted_keys.get(&envelope.key_id).ok_or("unknown update signing key")?;
    let signature = decode_hex::<64>(&envelope.signature)?;
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(envelope.payload.as_bytes());
    UnparsedPublicKey::new(&ED25519, key).verify(&message, &signature).map_err(|_| "invalid update signature")?;
    let manifest: Manifest = serde_json::from_str(&envelope.payload).map_err(|e| format!("invalid update manifest: {e}"))?;
    if manifest.protocol_version != 1 || manifest.channel != config.channel {
        return Err("unsupported update protocol or channel".into());
    }
    if manifest.sequence == 0 || manifest.sequence < minimum_sequence {
        return Err("obsolete update manifest sequence".into());
    }
    if manifest.issued_at_unix > now_unix || manifest.expires_at_unix <= now_unix || manifest.expires_at_unix <= manifest.issued_at_unix {
        return Err("update manifest is not currently valid".into());
    }
    if let Some(app) = &manifest.application {
        if !app.version.pre.is_empty() && config.channel == "stable" {
            return Err("prerelease application in stable update channel".into());
        }
        if app.name.trim().is_empty() || app.schema_version == 0 || app.artifacts.is_empty() || app.taxonomy_format_versions.is_empty() {
            return Err("incomplete application release".into());
        }
        for (target, artifact) in &app.artifacts {
            if target.is_empty() {
                return Err("empty update target".into());
            }
            artifact.validate()?;
        }
    }
    if let Some(taxonomy) = &manifest.taxonomy {
        if taxonomy.release_id == 0 || taxonomy.format_version == 0 || taxonomy.minimum_schema_version == 0 {
            return Err("invalid taxonomy compatibility metadata".into());
        }
        taxonomy.artifact.validate()?;
    }
    Ok(VerifiedManifest(manifest))
}

pub fn validate_https_url(value: &str) -> Result<(), String> {
    let url = url::Url::parse(value).map_err(|e| format!("invalid update URL: {e}"))?;
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("update URL must use HTTPS without credentials or a fragment".into());
    }
    Ok(())
}

pub fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N], String> {
    if value.len() != N * 2 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("expected {} hexadecimal bytes", N));
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
