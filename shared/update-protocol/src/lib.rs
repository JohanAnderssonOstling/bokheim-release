//! Signed update manifests and artifact verification shared by publishers and clients.

use ring::signature::{UnparsedPublicKey, ED25519};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

mod artifact;
pub use artifact::{copy_verified, copy_verified_with_progress, stage_verified, verify_file};

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

/// The public-domain catalog snapshot. Clients fetch it quietly, without the
/// approval and restart an application or taxonomy update takes.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRelease {
    pub release_id: u64,
    pub format_version: u32,
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
    pub catalog: Option<CatalogRelease>,
}

#[derive(Debug)]
pub struct VerifiedManifest(Manifest);

impl VerifiedManifest {
    pub fn manifest(&self) -> &Manifest {
        &self.0
    }
}

/// `minimum_sequence` is the host's last accepted publication number. Persist
/// it after successful verification; reverify cached envelopes on every use.
pub fn verify(bytes: &[u8], channel: &str, trusted_keys: &BTreeMap<String, [u8; 32]>, now_unix: u64, minimum_sequence: u64) -> Result<VerifiedManifest, String> {
    if channel.is_empty() || trusted_keys.is_empty() {
        return Err("update verification requires a channel and trusted signing key".into());
    }
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("update manifest exceeds size limit".into());
    }
    let envelope: SignedManifest = serde_json::from_slice(bytes).map_err(|e| format!("invalid signed update envelope: {e}"))?;
    let key = trusted_keys.get(&envelope.key_id).ok_or("unknown update signing key")?;
    let signature = decode_hex::<64>(&envelope.signature)?;
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(envelope.payload.as_bytes());
    UnparsedPublicKey::new(&ED25519, key).verify(&message, &signature).map_err(|_| "invalid update signature")?;
    let manifest: Manifest = serde_json::from_str(&envelope.payload).map_err(|e| format!("invalid update manifest: {e}"))?;
    if manifest.protocol_version != 1 || manifest.channel != channel {
        return Err("unsupported update protocol or channel".into());
    }
    if manifest.sequence == 0 || manifest.sequence < minimum_sequence {
        return Err("obsolete update manifest sequence".into());
    }
    if manifest.issued_at_unix > now_unix || manifest.expires_at_unix <= now_unix || manifest.expires_at_unix <= manifest.issued_at_unix {
        return Err("update manifest is not currently valid".into());
    }
    if let Some(app) = &manifest.application {
        if !app.version.pre.is_empty() && channel == "stable" {
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
    if let Some(catalog) = &manifest.catalog {
        if catalog.release_id == 0 || catalog.format_version == 0 {
            return Err("invalid catalog release metadata".into());
        }
        catalog.artifact.validate()?;
    }
    Ok(VerifiedManifest(manifest))
}

pub fn validate_https_url(value: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|e| format!("invalid update URL: {e}"))?;
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
