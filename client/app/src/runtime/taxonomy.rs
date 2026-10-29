//! The browser starts with a prepared database from its immutable app bundle.
//! Published taxonomy updates are installed in the background for the next start.
use crate::executor::spawn_detached;
use client_platform_web::web_storage;
use js_sys::{global, Uint8Array};
use std::path::Path;
use subject_projection::installed::{self, StoredMetadata};
use update_client::DiscoveryConfig;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Response, WorkerGlobalScope};

const STORED: &str = "taxonomy.sqlite3";
/// Formats this build reads, as native `DeviceVersion::taxonomy_formats`.
const FORMATS: [u32; 1] = [1];

/// Activates the stored taxonomy and looks for a newer release in the
/// background, taking effect on the next start like native updates.
pub(crate) async fn install() -> Result<(), String> {
    let bundled = StoredMetadata::bundled();
    let bundled_path = format!("taxonomy-{}.sqlite3", bundled.source.replace(':', "-"));
    let mut selected = web_storage::reference_database(STORED)?;
    if !selected.as_deref().map(web_storage::database_exists).transpose()?.unwrap_or(false) {
        selected = web_storage::database_exists(STORED)?.then(|| STORED.to_owned());
    }
    let stored = if let Some(path) = selected {
        if path == bundled_path {
            Some((path, StoredMetadata::bundled()))
        } else if path.starts_with("taxonomy-bundled-") {
            // A different build's packaged reference is replaced without
            // opening it; its filename already identifies the old artifact.
            None
        } else {
            match installed::load_metadata(Path::new(&path)) {
                Ok(stored) => Some((path, stored)),
                Err(error) => {
                    log::warn!("Replacing unreadable taxonomy cache: {error}");
                    None
                }
            }
        }
    } else {
        None
    };
    let (path, stored) = match stored.filter(|(_, stored)| FORMATS.contains(&stored.format) && stored.release >= installed::BUNDLED_RELEASE_ID && (!stored.source.starts_with("bundled:") || stored.source == bundled.source)) {
        Some(stored) => stored,
        None => {
            let path = bundled_path;
            if !web_storage::database_exists(&path)? {
                let image = download_bundle().await?;
                crate::platform::reserve_database_capacity(1).await?;
                web_storage::import_reference_database(&path, &image)?;
            }
            (path, bundled)
        }
    };
    web_storage::publish_reference_database(STORED, &path)?;
    web_storage::prune_reference_databases(STORED, "taxonomy-", &path)?;
    if !stored.source.starts_with("bundled:") {
        client_platform_web::cpu_host::set_taxonomy(serde_json::from_str(&stored.source).map_err(|e| e.to_string())?);
    }
    installed::activate(Path::new(&path))?;
    spawn_detached(async move {
        if let Err(error) = download_newer(stored.release).await {
            log::warn!("Taxonomy update will retry on next start: {error}");
        }
    });
    Ok(())
}

async fn download_bundle() -> Result<Vec<u8>, String> {
    let scope: WorkerGlobalScope = global().dyn_into().map_err(|_| "Taxonomy requires a worker")?;
    // This is an application asset, like WASM, rather than an update artifact.
    // Fetch decodes its HTTP Brotli/gzip encoding; no verification pass is needed.
    let response: Response = JsFuture::from(scope.fetch_with_str("./taxonomy.sqlite3")).await.map_err(|e| format!("Taxonomy download failed: {e:?}"))?.dyn_into().map_err(|_| "Invalid taxonomy response")?;
    if !response.ok() {
        return Err(format!("Taxonomy download returned HTTP {}", response.status()));
    }
    let buffer = JsFuture::from(response.array_buffer().map_err(|e| format!("Taxonomy response failed: {e:?}"))?).await.map_err(|e| format!("Taxonomy response failed: {e:?}"))?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// CPU workers use the same packaged reference on their first inspection.
pub async fn install_bundled_matching() -> Result<(), String> {
    installed::install_snapshot(&download_bundle().await?)
}

/// Downloads, prepares and stores the published taxonomy when it is newer
/// than `installed` and this build can read it.
async fn download_newer(installed: u64) -> Result<(), String> {
    let config = DiscoveryConfig::bundled()?;
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let manifest = update_client::fetch::discover(&config, now, 0).await?;
    // The deployed bundle is always the current application, so only the
    // format and schema decide compatibility.
    let schema = library_database::LIBRARY_SCHEMA_VERSION as u32;
    let Some(release) = manifest.manifest().taxonomy.as_ref().filter(|release| release.release_id > installed && FORMATS.contains(&release.format_version) && release.minimum_schema_version <= schema) else {
        return Ok(());
    };
    let path = format!("taxonomy-{}-{}.sqlite3", release.release_id, release.artifact.sha256);
    if !web_storage::database_exists(&path)? {
        let snapshot = update_client::fetch::download(&release.artifact).await?;
        let source = serde_json::to_string(&release.artifact).map_err(|e| e.to_string())?;
        let image = installed::prepare_image(&snapshot, (release.release_id, release.format_version), &source)?;
        crate::platform::reserve_database_capacity(1).await?;
        web_storage::import_reference_database(&path, &image)?;
    }
    web_storage::publish_reference_database(STORED, &path)
}
