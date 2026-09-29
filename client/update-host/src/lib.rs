#![cfg(any(target_os = "linux", target_os = "android", target_os = "windows"))]
//! Native Linux lifecycle, independent of the GUI. All activation runs before workers.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
#[cfg(any(target_os = "linux", target_os = "android"))]
use std::process::Command;
use update_client::{
    activation::{self, Journal, Replacement, State},
    coordinator::{ArtifactKind, Coordinator, DeviceVersion, Phase, UpdateView, WaitReason},
    native_state::{SqliteStore, UpdateLock},
    DiscoveryConfig,
};

#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod android;
pub mod windows;
#[cfg(windows)]
pub mod windows_process;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveTaxonomy {
    pub path: PathBuf,
    pub release: u64,
}
#[derive(Serialize, Deserialize)]
struct PrepareJob {
    databases: Vec<PathBuf>,
    taxonomy: Option<PathBuf>,
    previous_taxonomy: Option<ActiveTaxonomy>,
    expected_taxonomy: Option<(u64, u32)>,
    result: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Prepared {
    device: DeviceVersion,
    taxonomy: Option<ActiveTaxonomy>,
}

pub fn device(version: &str, taxonomy: Option<&ActiveTaxonomy>) -> Result<DeviceVersion, String> {
    let mut db = rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?;
    db.deserialize_bytes("main", subject_projection::DEFAULT_UNIFIED_TAXONOMY_SQLITE).map_err(|e| e.to_string())?;
    let release: String = db.query_row("SELECT value FROM taxonomy_meta WHERE key='release_id'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    Ok(DeviceVersion {
        application: version.parse().map_err(|e| format!("{e}"))?,
        schema: library_database::LIBRARY_SCHEMA_VERSION as u32,
        taxonomy_formats: vec![1],
        taxonomy_release: taxonomy.map(|t| t.release).unwrap_or(release.parse().map_err(|e| format!("{e}"))?),
        target: if cfg!(target_os = "android") { format!("android-{}-apk", std::env::consts::ARCH) }
            else if cfg!(target_os = "windows") { format!("windows-{}-zip", std::env::consts::ARCH) }
            else { format!("linux-{}-appimage", std::env::consts::ARCH) },
    })
}

/// Candidate subprocess entry point. It only opens the isolated files in its job.
pub fn prepare_candidate(job_path: &Path, version: &str) -> Result<(), String> {
    let job: PrepareJob = activation::read_json(job_path)?;
    let taxonomy = if let Some(path) = job.taxonomy {
        let actual = subject_projection::installed::metadata(&path)?;
        if Some(actual) != job.expected_taxonomy {
            return Err("taxonomy version differs from signed manifest".into());
        }
        subject_projection::installed::prepare(&path)?;
        Some(ActiveTaxonomy { path, release: actual.0 })
    } else {
        job.previous_taxonomy
    };
    if let Some(t) = &taxonomy {
        subject_projection::installed::activate(&t.path)?;
    }
    for path in job.databases {
        library_database::Database::prepare_update(&path).map_err(|e| e.to_string())?;
    }
    activation::write_json(&job.result, &Prepared { device: device(version, taxonomy.as_ref())?, taxonomy })
}

pub enum Startup {
    Run,
    Relaunch { executable: PathBuf, trial: Option<String> },
}
#[cfg(any(target_os = "linux", target_os = "android"))]
pub struct Host {
    pub root: PathBuf,
    data: PathBuf,
    version: String,
    pub executable: PathBuf,
    appimage: Option<PathBuf>,
    pub coordinator: Coordinator<SqliteStore>,
    trial: bool,
}
#[cfg(any(target_os = "linux", target_os = "android"))]
impl Host {
    pub fn open(data: &Path, version: &str, config: DiscoveryConfig, appimage: Option<PathBuf>, executable: PathBuf) -> Result<Self, String> {
        let root = data.join("updates");
        let _lock = UpdateLock::acquire(&root)?;
        let taxonomy = active(&root)?;
        let mut installed = device(version, taxonomy.as_ref())?;
        if appimage.is_none() {
            installed.target = "linux-taxonomy-only".into();
        }
        let session = format!("{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
        let coordinator = Coordinator::open(SqliteStore::open(&root)?, config, installed, session)?;
        Ok(Self { root, data: data.into(), version: version.into(), executable, appimage, coordinator, trial: false })
    }
    pub fn startup(&mut self, trial: Option<&str>) -> Result<Startup, String> {
        let lock = UpdateLock::acquire(&self.root)?;
        self.coordinator.reload()?;
        if let Some(journal) = Journal::load(&self.root)? {
            if journal.state == State::Committed {
                if self.actual_device()?.application < journal.next_version.parse().map_err(|e| format!("{e}"))? {
                    return Ok(Startup::Relaunch { executable: journal.appimage.ok_or("committed application path missing")?, trial: None });
                }
                if let Err(error) = self.finish_committed(&journal.plan_id) {
                    log::warn!("committed update bookkeeping will retry: {error}");
                    self.select_taxonomy()?;
                    return Ok(Startup::Run);
                }
            } else if journal.state == State::Trial && trial == Some(journal.plan_id.as_str()) && journal.next_version == self.version {
                self.coordinator.approved_manifest()?;
                self.trial = true;
                self.select_taxonomy()?;
                return Ok(Startup::Run);
            } else {
                journal.recover(&self.root)?;
                if self.coordinator.record().approved.is_some() {
                    self.coordinator.quarantine_after_recovery("interrupted or failed startup trial; prior version restored".into())?;
                }
                Journal::clear(&self.root)?;
                if journal.previous_version != self.version {
                    return Ok(Startup::Relaunch { executable: journal.appimage.ok_or("restored application path missing")?, trial: None });
                }
            }
        }
        if self.coordinator.activation_due() {
            match self.prepare_and_promote(&lock) {
                Ok(id) => return Ok(Startup::Relaunch { executable: self.appimage.clone().unwrap_or_else(|| self.executable.clone()), trial: Some(id) }),
                Err(error) => {
                    if let Some(journal) = Journal::load(&self.root)? {
                        journal.recover(&self.root)?;
                        Journal::clear(&self.root)?;
                    }
                    if self.coordinator.record().approved.as_ref().is_some_and(|p| p.phase == Phase::Activating) {
                        self.coordinator.quarantine_after_recovery(error)?;
                    }
                }
            }
        }
        self.coordinator.set_installed(self.actual_device()?);
        self.select_taxonomy()?;
        Ok(Startup::Run)
    }
    fn actual_device(&self) -> Result<DeviceVersion, String> {
        let mut actual = device(&self.version, active(&self.root)?.as_ref())?;
        if self.appimage.is_none() {
            actual.target = "linux-taxonomy-only".into();
        }
        Ok(actual)
    }
    fn select_taxonomy(&self) -> Result<(), String> {
        if let Some(t) = active(&self.root)? {
            subject_projection::installed::activate(&t.path)?;
        }
        Ok(())
    }
    fn finish_committed(&mut self, plan_id: &str) -> Result<(), String> {
        if self.coordinator.record().approved.as_ref().is_some_and(|p| p.id == plan_id) {
            self.coordinator.activated(self.actual_device()?)?;
        }
        Journal::clear(&self.root)?;
        self.cleanup_committed()
    }
    fn cleanup_committed(&self) -> Result<(), String> {
        let active = active(&self.root)?.map(|t| t.path);
        let pending = self.coordinator.record().approved.as_ref().map(|p| p.id.as_str());
        let generations = self.root.join("generations");
        if generations.exists() {
            for entry in fs::read_dir(&generations).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if !entry.file_type().map_err(|e| e.to_string())?.is_dir() || update_client::decode_hex::<32>(&entry.file_name().to_string_lossy()).is_err() {
                    continue;
                }
                if pending == entry.file_name().to_str() {
                    continue;
                }
                let directory = entry.path();
                if active.as_ref().and_then(|p| p.parent()) == Some(directory.as_path()) {
                    for file in fs::read_dir(&directory).map_err(|e| e.to_string())? {
                        let file = file.map_err(|e| e.to_string())?;
                        if Some(file.path()) != active && file.file_type().map_err(|e| e.to_string())?.is_file() {
                            fs::remove_file(file.path()).map_err(|e| e.to_string())?;
                        }
                    }
                } else {
                    fs::remove_dir_all(directory).map_err(|e| e.to_string())?;
                }
            }
        }
        let staging = self.root.join("staging");
        if staging.exists() {
            for entry in fs::read_dir(staging).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.file_type().map_err(|e| e.to_string())?.is_dir() && pending != entry.file_name().to_str() && update_client::decode_hex::<32>(&entry.file_name().to_string_lossy()).is_ok() {
                    fs::remove_dir_all(entry.path()).map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }
    /// Called after the backend and first window have initialized, before the UI event loop.
    pub fn healthy(&mut self) -> Result<(), String> {
        if !self.trial {
            return Ok(());
        }
        let _lock = UpdateLock::acquire(&self.root)?;
        let mut journal = Journal::load(&self.root)?.ok_or("missing startup journal")?;
        let installed = self.actual_device()?;
        journal.commit(&self.root)?;
        // After this commit, bookkeeping failure must not crash a usable app.
        // The journal remains authoritative and polling retries the acknowledgement.
        if let Err(error) = self.coordinator.activated(installed) {
            log::warn!("committed update acknowledgement will retry: {error}");
        }
        self.trial = false;
        // Keep committed receipt until next launch so the supervisor can observe it.
        Ok(())
    }
    pub fn is_trial(&self) -> bool {
        self.trial
    }

    pub fn approve(&mut self) -> Result<(), String> {
        let _lock = UpdateLock::acquire(&self.root)?;
        self.coordinator.reload()?;
        self.coordinator.approve(now())
    }
    pub fn tick(&mut self) -> Result<UpdateView, String> {
        let lock = UpdateLock::acquire(&self.root)?;
        self.coordinator.reload()?;
        if let Some(journal) = Journal::load(&self.root)? {
            if journal.state == State::Committed && self.coordinator.record().approved.as_ref().is_some_and(|p| p.id == journal.plan_id) {
                self.coordinator.activated(self.actual_device()?)?;
            }
        }
        let client = update_client::blocking::client(concat!("Bokheim-update/", env!("CARGO_PKG_VERSION")))?;
        self.coordinator.check_for_updates(&self.root, &lock, &client, now())?;
        if self.coordinator.preparation_due(now()) {
            if let Some(reason) = self.preflight()? {
                self.coordinator.preparation_failed(now(), reason, "update needs storage or installation permission".into())?;
                return Ok(self.coordinator.view(now()));
            }
            let workspace = self.workspace_bytes()?;
            if let Err(error) = self.coordinator.prepare_next(&self.root, &lock, &client, now(), workspace, 0) {
                self.coordinator.preparation_failed(now(), WaitReason::Retry, error)?;
            }
        }
        Ok(self.coordinator.view(now()))
    }
    fn databases(&self) -> Result<Vec<PathBuf>, String> {
        let parent = self.data.join("libraries");
        let mut paths = Vec::new();
        let registry = self.data.join("app-state-v1.sqlite3");
        if registry.try_exists().map_err(|e| e.to_string())? {
            paths.push(registry);
        }
        if !parent.try_exists().map_err(|e| e.to_string())? {
            return Ok(paths);
        }
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let path = entry.path().join("library.db");
            if path.try_exists().map_err(|e| e.to_string())? {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }
    fn preflight(&self) -> Result<Option<WaitReason>, String> {
        use std::os::unix::fs::MetadataExt;
        let mut required = self.workspace_bytes()?;
        if let Some(plan) = &self.coordinator.record().approved {
            for entry in &plan.artifacts {
                if !plan.completed.contains(&entry.kind) {
                    required = required.checked_add(entry.artifact.bytes).ok_or("workspace overflow")?;
                }
            }
            if let Some(artifact) = plan.artifacts.iter().find(|a| a.kind == ArtifactKind::Application) {
                let path = self.appimage.as_ref().ok_or("application is not an AppImage")?;
                let parent = path.parent().ok_or("AppImage has no parent")?;
                let probe = parent.join(format!(".bokheim-update-permission-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()));
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
                    Ok(file) => {
                        drop(file);
                        fs::remove_file(probe).map_err(|e| e.to_string())?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(30) => return Ok(Some(WaitReason::Permission)),
                    Err(e) => return Err(e.to_string()),
                }
                if fs::metadata(parent).map_err(|e| e.to_string())?.dev() == fs::metadata(&self.root).map_err(|e| e.to_string())?.dev() {
                    required = required.checked_add(artifact.artifact.bytes).ok_or("workspace overflow")?;
                } else {
                    let deficit = artifact.artifact.bytes.saturating_sub(fs2::available_space(parent).map_err(|e| e.to_string())?);
                    if deficit > 0 {
                        return Ok(Some(WaitReason::Space { additional_bytes: deficit }));
                    }
                }
            }
        }
        let deficit = required.saturating_sub(fs2::available_space(&self.root).map_err(|e| e.to_string())?);
        Ok((deficit > 0).then_some(WaitReason::Space { additional_bytes: deficit }))
    }
    fn workspace_bytes(&self) -> Result<u64, String> {
        let mut bytes = 0u64;
        for path in self.databases()? {
            let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
            let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0)).map_err(|e| e.to_string())?;
            let size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(|e| e.to_string())?;
            bytes = bytes.checked_add(u64::try_from(pages.checked_mul(size).and_then(|n| n.checked_mul(3)).ok_or("database workspace overflow")?).map_err(|e| e.to_string())?).ok_or("workspace overflow")?;
        }
        if self.coordinator.record().approved.as_ref().is_some_and(|p| p.artifacts.iter().any(|a| a.kind == ArtifactKind::Application)) {
            if let Some(path) = &self.appimage {
                bytes = bytes.checked_add(fs::metadata(path).map_err(|e| e.to_string())?.len()).ok_or("workspace overflow")?;
            }
        }
        // Backup, candidate copy and migration journal; no fixed megabyte threshold.
        if let Some(plan) = &self.coordinator.record().approved {
            for a in &plan.artifacts {
                bytes = bytes.checked_add(a.artifact.bytes).ok_or("workspace overflow")?;
            }
        }
        Ok(bytes)
    }
    fn prepare_and_promote(&mut self, lock: &UpdateLock) -> Result<String, String> {
        let paths = self.coordinator.staged_paths(&self.root, lock)?;
        if let Some(reason) = self.preflight()? {
            self.coordinator.activation_recovered_for_retry(now(), reason, "update needs storage or installation permission".into())?;
            return Err("update waits for storage or permission".into());
        }
        let manifest = self.coordinator.begin_activation()?;
        let id = self.coordinator.record().approved.as_ref().unwrap().id.clone();
        let work = self.root.join("generations").join(&id);
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut entries = Vec::new();
        let mut databases = Vec::new();
        for (index, path) in self.databases()?.into_iter().enumerate() {
            let previous = work.join(format!("{index}.prior.sqlite3"));
            let prepared = work.join(format!("{index}.next.sqlite3"));
            activation::copy_database(&path, &previous)?;
            activation::copy_database(&previous, &prepared)?;
            if path.file_name().is_some_and(|name| name == "library.db") {
                databases.push(prepared.clone());
            }
            entries.push(Replacement { destination: path, previous: Some(previous), prepared, sqlite: true });
        }
        let taxonomy = paths.iter().find(|(k, _)| *k == ArtifactKind::Taxonomy).map(|(_, p)| p);
        let taxonomy = taxonomy
            .map(|source| {
                let path = work.join("taxonomy.sqlite3");
                activation::copy_file(source, &path)?;
                Ok::<_, String>(path)
            })
            .transpose()?;
        let application = paths.iter().find(|(k, _)| *k == ArtifactKind::Application).map(|(_, p)| p);
        let candidate = if let Some(source) = application {
            let path = work.join("candidate.AppImage");
            activation::copy_file(source, &path)?;
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            path
        } else {
            self.executable.clone()
        };
        let result = work.join("prepared.json");
        if result.try_exists().map_err(|e| e.to_string())? {
            fs::remove_file(&result).map_err(|e| e.to_string())?;
        }
        let job = PrepareJob {
            databases,
            taxonomy,
            previous_taxonomy: active(&self.root)?,
            expected_taxonomy: manifest.taxonomy.as_ref().filter(|_| paths.iter().any(|(k, _)| *k == ArtifactKind::Taxonomy)).map(|t| (t.release_id, t.format_version)),
            result: result.clone(),
        };
        let job_path = work.join("prepare.json");
        activation::write_json(&job_path, &job)?;
        let mut child = Command::new(&candidate).arg("--bokheim-prepare-update").arg(&job_path).env_remove("APPIMAGE").env_remove("APPDIR").spawn().map_err(|e| e.to_string())?;
        let status = update_client::supervisor::wait_for_preparation(&mut child, std::time::Duration::from_secs(30 * 60))?;
        if !status.success() {
            return Err(format!("candidate migration failed: {status}"));
        }
        let prepared: Prepared = activation::read_json(&result)?;
        if let Some(app) = manifest.application.as_ref().filter(|_| application.is_some()) {
            if prepared.device.application != app.version || prepared.device.schema != app.schema_version {
                return Err("candidate application/schema differs from signed release".into());
            }
        } else if prepared.device.application.to_string() != self.version {
            return Err("taxonomy preparation ran a different application".into());
        }
        if let Some((release, _)) = job.expected_taxonomy {
            if prepared.device.taxonomy_release != release {
                return Err("candidate taxonomy differs from signed release".into());
            }
        }
        let next_pointer = work.join("active-taxonomy.json");
        activation::write_json(&next_pointer, &prepared.taxonomy)?;
        let pointer = self.root.join("active-taxonomy.json");
        let previous = if pointer.exists() {
            let p = work.join("prior-taxonomy.json");
            activation::copy_file(&pointer, &p)?;
            Some(p)
        } else {
            None
        };
        entries.push(Replacement { destination: pointer, previous, prepared: next_pointer, sqlite: false });
        if application.is_some() {
            let destination = self.appimage.clone().ok_or("application update requires an AppImage installation")?;
            let required = fs::metadata(&candidate).map_err(|e| e.to_string())?.len();
            let free = fs2::available_space(destination.parent().ok_or("AppImage has no directory")?).map_err(|e| e.to_string())?;
            if free < required {
                return Err(format!("AppImage volume needs {} additional bytes", required - free));
            }
            let backup = work.join("previous.AppImage");
            activation::copy_file(&destination, &backup)?;
            entries.push(Replacement { destination, previous: Some(backup), prepared: candidate, sqlite: false });
        }
        activation::sync_dir(&work)?;
        activation::sync_dir(work.parent().unwrap())?;
        activation::sync_dir(&self.root)?;
        let mut journal = Journal { plan_id: id.clone(), previous_version: self.version.clone(), next_version: prepared.device.application.to_string(), appimage: self.appimage.clone(), replacements: entries, state: State::Applying };
        journal.promote(&self.root)?;
        Ok(id)
    }
}
fn active(root: &Path) -> Result<Option<ActiveTaxonomy>, String> {
    let path = root.join("active-taxonomy.json");
    if path.try_exists().map_err(|e| e.to_string())? {
        activation::read_json(&path)
    } else {
        Ok(None)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
