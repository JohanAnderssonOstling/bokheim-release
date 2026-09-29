use anyhow::{anyhow, bail, ensure, Context, Result};
use fs2::FileExt;
use ring::digest::{Context as Digest, SHA256};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use update_client::{Artifact, DiscoveryConfig, Manifest, SignedManifest};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    endpoint: String,
    channel: String,
    trusted_keys: BTreeMap<String, String>,
    /// HTTPS origins mapped to separate public directories, e.g. updates/taxonomy.
    origins: BTreeMap<String, String>,
    /// Explicit HTTPS directory prefixes for externally hosted packages.
    #[serde(default)]
    external_artifact_prefixes: Vec<String>,
}

fn component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

impl Config {
    fn discovery(&self) -> Result<DiscoveryConfig> {
        ensure!(component(&self.channel), "invalid channel");
        let trusted_keys = self.trusted_keys.iter().map(|(id, hex)| Ok((id.clone(), update_client::decode_hex(hex).map_err(|e| anyhow!(e))?))).collect::<Result<_>>()?;
        let config = DiscoveryConfig { endpoint: self.endpoint.clone(), channel: self.channel.clone(), trusted_keys };
        config.validate().map_err(|e| anyhow!(e))?;
        let mut names = std::collections::BTreeSet::new();
        for (origin, name) in &self.origins {
            let url = url::Url::parse(origin)?;
            ensure!(url.scheme() == "https" && url.origin().ascii_serialization() == *origin && component(name) && names.insert(name), "invalid or duplicate origin mapping");
        }
        for prefix in &self.external_artifact_prefixes {
            let url = url::Url::parse(prefix)?;
            ensure!(url.scheme() == "https" && url.host_str().is_some()
                && url.username().is_empty() && url.password().is_none()
                && url.query().is_none() && url.fragment().is_none()
                && url.as_str() == prefix && url.path().ends_with('/')
                && url.path() != "/" && !url.path().contains('%')
                && !url.path().contains("//")
                && !self.origins.contains_key(&url.origin().ascii_serialization()),
                "external artifact prefix must be a canonical HTTPS directory on an external origin");
        }
        self.path(&self.endpoint, false)?;
        Ok(config)
    }

    fn external(&self, raw: &str) -> Result<bool> {
        let url = url::Url::parse(raw)?;
        if self.origins.contains_key(&url.origin().ascii_serialization()) {
            return Ok(false);
        }
        ensure!(url.scheme() == "https" && url.as_str() == raw
            && url.username().is_empty() && url.password().is_none()
            && url.query().is_none() && url.fragment().is_none()
            && !url.path().contains('%') && !url.path().contains("//")
            && url.path().split('/').skip(1).all(component)
            && self.external_artifact_prefixes.iter().any(|prefix| raw.starts_with(prefix)),
            "artifact URL is outside configured external directories");
        Ok(true)
    }

    fn path(&self, raw: &str, artifact: bool) -> Result<PathBuf> {
        let url = url::Url::parse(raw)?;
        ensure!(url.query().is_none() && url.fragment().is_none() && url.username().is_empty() && url.password().is_none(), "unexpected URL components");
        let origin = url.origin().ascii_serialization();
        ensure!(!url.path().starts_with("//"), "URL path contains an empty segment");
        let namespace = self.origins.get(&origin).context("URL origin not hosted by this publisher")?;
        // Reject normalization, percent decoding, empty segments and traversal.
        ensure!(raw == format!("{}{}", origin, url.path()), "URL must be canonical");
        let parts: Vec<_> = url.path().trim_start_matches('/').split('/').collect();
        ensure!(parts.iter().all(|s| component(s)), "unsafe URL path");
        if artifact {
            ensure!(parts.len() >= 3 && parts[0] == "releases", "artifacts require /releases/<version>/<file>");
        } else {
            ensure!(parts == ["v1", &self.channel], "feed must be /v1/<channel>");
        }
        Ok(Path::new(namespace).join(parts.join("/")))
    }
}

// The publishing account exclusively owns root and bundle. Reject symlinks so
// configuration mistakes cannot redirect writes or publish files outside them.
fn directories(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        directories(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(meta.is_dir() && !meta.file_type().is_symlink(), "not a real directory: {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)?,
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn regular(path: &Path) -> Result<File> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(meta.is_file() && !meta.file_type().is_symlink(), "not a regular file: {}", path.display());
    File::open(path).map_err(Into::into)
}

fn bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    regular(path)?.take(update_client::MAX_MANIFEST_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= update_client::MAX_MANIFEST_BYTES, "manifest exceeds size limit");
    Ok(bytes)
}

fn copy_verified(mut input: File, mut output: impl Write, artifact: &Artifact) -> Result<()> {
    ensure!(input.metadata()?.len() == artifact.bytes, "artifact size mismatch: {}", artifact.url);
    let mut digest = Digest::new(&SHA256);
    let mut copied = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        copied = copied.checked_add(n as u64).context("artifact length overflow")?;
        ensure!(copied <= artifact.bytes, "artifact grew while copying");
        digest.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
    }
    let expected = update_client::decode_hex::<32>(&artifact.sha256).map_err(|e| anyhow!(e))?;
    ensure!(copied == artifact.bytes && digest.finish().as_ref() == expected, "artifact checksum mismatch: {}", artifact.url);
    Ok(())
}

fn make_public(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
    }
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all().map_err(Into::into)
}

fn publish(config: &Config, bundle: &Path, root: &Path, now: u64) -> Result<()> {
    // Anonymous requests prove that clients can fetch the public package. Never
    // use CI's GitHub credentials: draft/private assets must not be advertised.
    let client = update_client::blocking::client("bokheim-update-publisher/1").map_err(|e| anyhow!(e))?;
    publish_with_verifier(config, bundle, root, now, |artifact| {
        update_client::blocking::download(&client, artifact, &mut std::io::sink()).map_err(|e| anyhow!(e))
    })
}

fn publish_with_verifier(config: &Config, bundle: &Path, root: &Path, now: u64, mut verify_external: impl FnMut(&Artifact) -> Result<()>) -> Result<()> {
    let discovery = config.discovery()?;
    directories(root)?;
    let lock_path = root.join("publish.lock");
    if lock_path.try_exists()? {
        regular(&lock_path)?;
    }
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path)?;
    lock.try_lock_exclusive().context("another publication is running")?;
    let bytes = bounded(&bundle.join("manifest.signed.json"))?;
    let verified = update_client::verify(&bytes, &discovery, now, 0).map_err(|e| anyhow!(e))?;
    let manifest = verified.manifest();
    let public = root.join("public");
    let feed = public.join(config.path(&config.endpoint, false)?);
    directories(feed.parent().unwrap())?;
    if feed.try_exists()? {
        let previous = bounded(&feed)?;
        let envelope: SignedManifest = serde_json::from_slice(&previous)?;
        let old: Manifest = serde_json::from_str(&envelope.payload)?;
        // Expiration prevents discovery; it must not erase the replay floor.
        let old = update_client::verify(&previous, &discovery, old.issued_at_unix, 0).map_err(|e| anyhow!(e))?;
        ensure!(manifest.sequence >= old.manifest().sequence, "publication sequence would go backwards");
        ensure!(manifest.sequence != old.manifest().sequence || bytes == previous, "sequence already used for different metadata");
    }
    let artifacts = manifest.application.iter().flat_map(|app| app.artifacts.values()).chain(manifest.taxonomy.iter().map(|t| &t.artifact));
    for artifact in artifacts {
        if config.external(&artifact.url)? {
            verify_external(artifact).context("external artifact is unavailable or does not match the signed manifest")?;
            continue;
        }
        let relative = config.path(&artifact.url, true)?;
        let destination = public.join(&relative);
        directories(destination.parent().unwrap())?;
        if destination.try_exists()? {
            copy_verified(regular(&destination)?, std::io::sink(), artifact).context("immutable artifact collision or corruption")?;
            for ancestor in destination.ancestors().skip(1) {
                if ancestor.starts_with(root) {
                    sync_directory(ancestor)?;
                }
            }
            continue;
        }
        let source = bundle.join("artifacts").join(relative);
        // Require existing, real ancestors without creating anything in bundle.
        for ancestor in source.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            let meta = fs::symlink_metadata(ancestor)?;
            ensure!(meta.is_dir() && !meta.file_type().is_symlink(), "unsafe bundle directory");
        }
        let mut stage = tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
        copy_verified(regular(&source)?, &mut stage, artifact)?;
        make_public(stage.as_file())?;
        stage.persist_noclobber(&destination)?;
        for ancestor in destination.ancestors().skip(1) {
            if ancestor.starts_with(root) {
                sync_directory(ancestor)?;
            }
        }
    }
    // Keep exact signed history outside the public root. No private signing key
    // is ever needed by this process. A crash before promotion leaves old feed.
    let history = root.join("history").join(&config.channel);
    directories(&history)?;
    let archived = history.join(format!("{}.json", manifest.sequence));
    if archived.try_exists()? {
        ensure!(bounded(&archived)? == bytes, "sequence conflicts with publication history");
    } else {
        let mut stage = tempfile::NamedTempFile::new_in(&history)?;
        stage.write_all(&bytes)?;
        stage.as_file().sync_all()?;
        stage.persist_noclobber(archived)?;
        sync_directory(&history)?;
    }
    // Flush created directory entries before exposing the new feed.
    for dir in [public.join(config.path(&config.endpoint, false)?).parent().unwrap().to_path_buf(), history] {
        for ancestor in dir.ancestors() {
            if ancestor.starts_with(root) {
                sync_directory(ancestor)?;
            }
        }
    }
    let mut stage = tempfile::NamedTempFile::new_in(feed.parent().unwrap())?;
    stage.write_all(&bytes)?;
    make_public(stage.as_file())?;
    stage.persist(&feed)?;
    sync_directory(feed.parent().unwrap())?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "--check-config" {
        let config: Config = serde_json::from_reader(File::open(&args[1])?)?;
        config.discovery()?;
        println!("Publisher configuration is valid");
        return Ok(());
    }
    if args.len() != 3 {
        bail!("usage: update-publisher CONFIG.json BUNDLE_DIR PUBLISH_ROOT | --check-config CONFIG.json");
    }
    let config: Config = serde_json::from_reader(File::open(&args[0])?)?;
    publish(&config, Path::new(&args[1]), Path::new(&args[2]), SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    println!("Published verified update feed at {}", config.endpoint);
    Ok(())
}

#[cfg(test)]
mod tests;
