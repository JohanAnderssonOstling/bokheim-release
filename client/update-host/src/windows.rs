//! Windows transaction host. Replacement/recovery runs in a copied helper executable
//! after the application process releases ownership, never in the installed EXE.
use super::*;

pub enum WindowsStartup { Run, Helper }

pub struct WindowsHost {
    pub root: PathBuf,
    pub installation: PathBuf,
    data: PathBuf,
    version: String,
    pub coordinator: Coordinator<SqliteStore>,
    trial: bool,
}

impl WindowsHost {
    pub fn open(data: &Path, installation: &Path, version: &str, config: DiscoveryConfig) -> Result<Self, String> {
        let root = data.join("updates");
        let _lock = UpdateLock::acquire(&root)?;
        let mut installed = device(version, active(&root)?.as_ref())?;
        installed.target = "windows-x86_64-zip".into();
        let session = format!("{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos());
        let coordinator = Coordinator::open(SqliteStore::open(&root)?, config, installed, session)?;
        Ok(Self { root, installation: installation.canonicalize().map_err(|e| e.to_string())?, data: data.into(), version: version.into(), coordinator, trial: false })
    }

    fn installed(&self) -> Result<DeviceVersion, String> {
        let mut actual = device(&self.version, active(&self.root)?.as_ref())?;
        actual.target = "windows-x86_64-zip".into();
        Ok(actual)
    }

    pub fn startup(&mut self, trial: Option<&str>) -> Result<WindowsStartup, String> {
        let _lock = UpdateLock::acquire(&self.root)?;
        self.coordinator.reload()?;
        if let Some(journal) = Journal::load(&self.root)? {
            if journal.state == State::Committed {
                if self.installed()?.application < journal.next_version.parse().map_err(|e| format!("{e}"))? { return Ok(WindowsStartup::Helper); }
                self.acknowledge(&journal)?;
                Journal::clear(&self.root)?;
            } else if journal.state == State::Trial && trial == Some(journal.plan_id.as_str()) && journal.next_version == self.version {
                self.coordinator.approved_manifest()?;
                if !self.coordinator.record().approved.as_ref().is_some_and(|p| p.id == journal.plan_id && p.phase == Phase::Activating) {
                    return Err("Windows trial does not match approved activation".into());
                }
                self.trial = true;
            } else { return Ok(WindowsStartup::Helper); }
        } else if self.coordinator.activation_due() {
            return Ok(WindowsStartup::Helper);
        }
        if !self.trial {
            if let Err(error) = self.cleanup() {
                // Antivirus or another reader can temporarily hold old files.
                // Cleanup is retried next launch and cannot block a healthy app.
                log::warn!("Windows update cleanup will retry: {error}");
            }
        }
        if let Some(taxonomy) = active(&self.root)? { subject_projection::installed::activate(&taxonomy.path)?; }
        Ok(WindowsStartup::Run)
    }

    fn acknowledge(&mut self, journal: &Journal) -> Result<(), String> {
        if self.coordinator.record().approved.as_ref().is_some_and(|p| p.id == journal.plan_id) {
            self.coordinator.activated(self.installed()?)?;
        }
        Ok(())
    }

    pub fn is_trial(&self) -> bool { self.trial }

    pub fn healthy(&mut self) -> Result<(), String> {
        if !self.trial { return Ok(()); }
        let _lock = UpdateLock::acquire(&self.root)?;
        let mut journal = Journal::load(&self.root)?.ok_or("missing Windows startup journal")?;
        let actual = self.installed()?;
        journal.commit(&self.root)?;
        if let Err(error) = self.coordinator.activated(actual) { log::warn!("Windows update acknowledgement will retry: {error}"); }
        self.trial = false;
        Ok(())
    }

    /// A helper that never started has not changed live files. Preserve approval
    /// and resume preparation while allowing this application session to run.
    pub fn helper_unavailable(&mut self, reason: WaitReason, diagnostic: String) -> Result<(), String> {
        let _lock = UpdateLock::acquire(&self.root)?;
        if Journal::load(&self.root)?.is_some() { return Err(diagnostic); }
        self.coordinator.reload()?;
        self.coordinator.activation_recovered_for_retry(now(), reason, diagnostic)
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
            if journal.state == State::Committed { self.acknowledge(&journal)?; }
        }
        let client = update_client::blocking::client("Bokheim-Windows-update/1")?;
        self.coordinator.check_for_updates(&self.root, &lock, &client, now())?;
        if self.coordinator.preparation_due(now()) {
            let workspace = self.database_workspace()?;
            if let Err(error) = self.coordinator.prepare_next(&self.root, &lock, &client, now(), workspace, 0) {
                self.coordinator.preparation_failed(now(), WaitReason::Retry, error)?;
            }
        }
        Ok(self.coordinator.view(now()))
    }

    fn databases(&self) -> Result<Vec<PathBuf>, String> {
        let mut paths = Vec::new();
        let registry = self.data.join("app-state-v1.sqlite3");
        if registry.try_exists().map_err(|e| e.to_string())? { paths.push(registry); }
        let libraries = self.data.join("libraries");
        if libraries.try_exists().map_err(|e| e.to_string())? {
            for entry in fs::read_dir(libraries).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if !entry.file_type().map_err(|e| e.to_string())?.is_dir() { continue; }
                let path = entry.path().join("library.db");
                if path.try_exists().map_err(|e| e.to_string())? { paths.push(path); }
            }
        }
        paths.sort(); Ok(paths)
    }

    fn database_workspace(&self) -> Result<u64, String> {
        let mut total = 0u64;
        for path in self.databases()? {
            let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
            let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0)).map_err(|e| e.to_string())?;
            let size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(|e| e.to_string())?;
            let pages = u64::try_from(pages).map_err(|e| e.to_string())?;
            let size = u64::try_from(size).map_err(|e| e.to_string())?;
            total = total.checked_add(pages.checked_mul(size).and_then(|n| n.checked_mul(3)).ok_or("database workspace overflow")?).ok_or("workspace overflow")?;
        }
        Ok(total)
    }

    fn cleanup(&self) -> Result<(), String> {
        let active = active(&self.root)?.map(|t| t.path);
        let pending = self.coordinator.record().approved.as_ref().map(|p| p.id.as_str());
        for directory in ["generations", "staging"] {
            let parent = self.root.join(directory);
            if !parent.exists() { continue; }
            for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name(); let name = name.to_string_lossy();
                if !entry.file_type().map_err(|e| e.to_string())?.is_dir() || pending == Some(name.as_ref()) || update_client::decode_hex::<32>(&name).is_err() { continue; }
                let path = entry.path();
                if active.as_ref().and_then(|p| p.parent()) == Some(path.as_path()) {
                    for file in fs::read_dir(path).map_err(|e| e.to_string())? {
                        let file = file.map_err(|e| e.to_string())?;
                        if active.as_ref() == Some(&file.path()) { continue; }
                        if file.file_type().map_err(|e| e.to_string())?.is_dir() { fs::remove_dir_all(file.path()).map_err(|e| e.to_string())?; }
                        else { fs::remove_file(file.path()).map_err(|e| e.to_string())?; }
                    }
                } else { fs::remove_dir_all(path).map_err(|e| e.to_string())?; }
            }
        }
        Ok(())
    }

    /// The caller must hold exclusive desktop ownership and execute from a copy
    /// outside installation. Returns the plan ID for the trial process, or None
    /// when the restored/committed application should run normally.
    pub fn prepare_or_recover(&mut self, prepare: impl FnOnce(&Path, &Path) -> Result<(), String>) -> Result<Option<String>, String> {
        let lock = UpdateLock::acquire(&self.root)?;
        self.coordinator.reload()?;
        if let Some(journal) = Journal::load(&self.root)? {
            if journal.state != State::Committed { self.recover(&journal, "Windows update startup was interrupted".into())?; }
            return Ok(None);
        }
        if !self.coordinator.activation_due() { return Ok(None); }
        match self.promote(&lock, prepare) {
            Ok(id) => Ok(Some(id)),
            Err(error) => {
                if let Some(journal) = Journal::load(&self.root)? {
                    self.recover(&journal, error)?;
                } else if self.coordinator.record().approved.as_ref().is_some_and(|p| p.phase == Phase::Activating) {
                    self.coordinator.quarantine_after_recovery(error)?;
                } else if self.coordinator.record().approved.as_ref().is_some_and(|p| p.phase == Phase::Ready) {
                    self.coordinator.activation_recovered_for_retry(now(), WaitReason::Retry, error)?;
                }
                Ok(None)
            }
        }
    }

    /// Run from the external helper while it holds desktop ownership. The
    /// candidate runs against private copies and must exit before promotion.
    pub fn prepare_or_recover_in_process(&mut self) -> Result<Option<String>, String> {
        self.prepare_or_recover(|executable, job| {
            let mut command = std::process::Command::new(executable);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // Headless migration, no console popup.
            }
            let mut child = command.arg("--bokheim-prepare-update").arg(job).spawn().map_err(|e| e.to_string())?;
            let status = update_client::supervisor::wait_for_preparation(&mut child, std::time::Duration::from_secs(30 * 60))?;
            if status.success() { Ok(()) } else { Err(format!("candidate migration failed: {status}")) }
        })
    }

    fn recover(&mut self, journal: &Journal, reason: String) -> Result<(), String> {
        if journal.state == State::Committed { return Ok(()); }
        journal.recover(&self.root)?;
        if self.coordinator.record().approved.as_ref().is_some_and(|p| p.id == journal.plan_id) {
            self.coordinator.quarantine_after_recovery(reason)?;
        }
        Journal::clear(&self.root)
    }

    pub fn recover_stopped_trial(&mut self, reason: String) -> Result<(), String> {
        let _lock = UpdateLock::acquire(&self.root)?;
        self.coordinator.reload()?;
        if let Some(journal) = Journal::load(&self.root)? { self.recover(&journal, reason)?; }
        Ok(())
    }

    fn promote(&mut self, lock: &UpdateLock, prepare: impl FnOnce(&Path, &Path) -> Result<(), String>) -> Result<String, String> {
        let paths = self.coordinator.staged_paths(&self.root, lock)?;
        let app = paths.iter().find(|(kind, _)| *kind == ArtifactKind::Application).map(|(_, path)| path);
        let package = app.map(|path| update_client::windows_package::inspect(path)).transpose()?;
        let mut required = self.database_workspace()?;
        if let Some(package) = &package {
            // Candidate plus temporary replacement copies. Backups below count
            // existing bytes independently; no fixed free-space threshold.
            required = required.checked_add(package.expanded_bytes).ok_or("workspace overflow")?;
            for relative in &package.files {
                let destination = self.installation.join(relative);
                let mut parent = destination.parent().ok_or("missing package directory")?;
                while !parent.exists() { parent = parent.parent().ok_or("invalid installation directory")?; }
                for ancestor in parent.ancestors().take_while(|p| p.starts_with(&self.installation)) {
                    let metadata = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
                    if metadata.file_type().is_symlink() { return Err("installation contains a linked directory".into()); }
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt;
                        if metadata.file_attributes() & 0x400 != 0 { return Err("installation contains a reparse point".into()); }
                    }
                }
                let probe = parent.join(format!(".bokheim-write-check-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()));
                match fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
                    Ok(file) => { drop(file); fs::remove_file(&probe).map_err(|e| e.to_string())?; }
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                        self.coordinator.activation_recovered_for_retry(now(), WaitReason::Permission, "installation directory is not writable".into())?;
                        return Err("waiting for installation permission".into());
                    }
                    Err(error) => return Err(error.to_string()),
                }
                if destination.try_exists().map_err(|e| e.to_string())? {
                    required = required.checked_add(fs::metadata(destination).map_err(|e| e.to_string())?.len()).ok_or("backup workspace overflow")?;
                }
            }
            if same_volume(&self.root, &self.installation)? {
                required = required.checked_add(package.expanded_bytes).ok_or("replacement workspace overflow")?;
            } else {
                let deficit = package.expanded_bytes.saturating_sub(fs2::available_space(&self.installation).map_err(|e| e.to_string())?);
                if deficit > 0 {
                    self.coordinator.activation_recovered_for_retry(now(), WaitReason::Space { additional_bytes: deficit }, "insufficient installation volume space".into())?;
                    return Err("waiting for installation storage".into());
                }
            }
        }
        if let Some((_, taxonomy)) = paths.iter().find(|(kind, _)| *kind == ArtifactKind::Taxonomy) {
            required = required.checked_add(fs::metadata(taxonomy).map_err(|e| e.to_string())?.len()).ok_or("taxonomy workspace overflow")?;
        }
        let deficit = required.saturating_sub(fs2::available_space(&self.root).map_err(|e| e.to_string())?);
        if deficit > 0 {
            self.coordinator.activation_recovered_for_retry(now(), WaitReason::Space { additional_bytes: deficit }, "insufficient Windows update workspace".into())?;
            return Err("waiting for storage".into());
        }
        let manifest = self.coordinator.begin_activation()?;
        let id = self.coordinator.record().approved.as_ref().unwrap().id.clone();
        let work = self.root.join("generations").join(&id);
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let candidate_dir = work.join("application");
        let candidate = if let Some(source) = app {
            // A previous incomplete extraction is isolated, never the active app.
            if candidate_dir.exists() { fs::remove_dir_all(&candidate_dir).map_err(|e| e.to_string())?; }
            update_client::windows_package::unpack(source, &candidate_dir, fs2::available_space(&work).map_err(|e| e.to_string())?)?;
            candidate_dir.join("Bokheim.exe")
        } else { self.installation.join("Bokheim.exe") };
        let mut replacements = Vec::new(); let mut databases = Vec::new();
        for (index, destination) in self.databases()?.into_iter().enumerate() {
            let previous = work.join(format!("{index}.prior.sqlite3")); let prepared = work.join(format!("{index}.next.sqlite3"));
            activation::copy_database(&destination, &previous)?; activation::copy_database(&previous, &prepared)?;
            if destination.file_name().is_some_and(|name| name == "library.db") { databases.push(prepared.clone()); }
            replacements.push(Replacement { destination, previous: Some(previous), prepared, sqlite: true });
        }
        let taxonomy = paths.iter().find(|(kind, _)| *kind == ArtifactKind::Taxonomy).map(|(_, source)| {
            let path = work.join("taxonomy.sqlite3"); activation::copy_file(source, &path)?; Ok::<_, String>(path)
        }).transpose()?;
        let result = work.join("prepared.json");
        if result.exists() { fs::remove_file(&result).map_err(|e| e.to_string())?; }
        let job = PrepareJob { databases, previous_taxonomy: active(&self.root)?,
            expected_taxonomy: manifest.taxonomy.as_ref().filter(|_| taxonomy.is_some()).map(|t| (t.release_id, t.format_version)), taxonomy, result: result.clone() };
        let job_path = work.join("prepare-job.json"); activation::write_json(&job_path, &job)?;
        prepare(&candidate, &job_path)?;
        let prepared: Prepared = activation::read_json(&result)?;
        if let Some(app) = manifest.application.as_ref().filter(|_| app.is_some()) {
            if prepared.device.application != app.version || prepared.device.schema != app.schema_version { return Err("Windows candidate differs from signed release".into()); }
        } else if prepared.device.application.to_string() != self.version { return Err("unexpected taxonomy helper version".into()); }
        if let Some((release, _)) = job.expected_taxonomy {
            if prepared.device.taxonomy_release != release { return Err("Windows candidate taxonomy differs from approved release".into()); }
        }
        let pointer = self.root.join("active-taxonomy.json"); let next = work.join("active-taxonomy.json");
        activation::write_json(&next, &prepared.taxonomy)?;
        let previous = if pointer.exists() { let old = work.join("prior-taxonomy.json"); activation::copy_file(&pointer, &old)?; Some(old) } else { None };
        replacements.push(Replacement { destination: pointer, previous, prepared: next, sqlite: false });
        if let Some(package) = package {
            for (index, relative) in package.files.iter().enumerate() {
                let destination = self.installation.join(relative);
                fs::create_dir_all(destination.parent().unwrap()).map_err(|e| e.to_string())?;
                let previous = if destination.exists() {
                    let backup = work.join(format!("app-{index}.prior")); activation::copy_file(&destination, &backup)?; Some(backup)
                } else { None };
                replacements.push(Replacement { destination, previous, prepared: candidate_dir.join(relative), sqlite: false });
            }
        }
        activation::sync_dir(&work)?; activation::sync_dir(work.parent().unwrap())?;
        Journal { plan_id: id.clone(), previous_version: self.version.clone(), next_version: prepared.device.application.to_string(), appimage: None, replacements, state: State::Applying }.promote(&self.root)?;
        Ok(id)
    }
}

fn same_volume(left: &Path, right: &Path) -> Result<bool, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(fs::metadata(left).map_err(|e| e.to_string())?.dev() == fs::metadata(right).map_err(|e| e.to_string())?.dev())
    }
    #[cfg(windows)]
    {
        // Canonical paths carry drive/UNC prefixes. Mounted-directory reparse
        // points in the installation are rejected before this comparison.
        let prefix = |path: &Path| -> Result<String, String> {
            let path = path.canonicalize().map_err(|e| e.to_string())?;
            Ok(path.components().next().ok_or("missing volume prefix")?.as_os_str().to_string_lossy().to_ascii_lowercase())
        };
        Ok(prefix(left)? == prefix(right)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{digest::{digest, SHA256}, signature::{Ed25519KeyPair, KeyPair}};
    use std::io::{Cursor, Write};
    use update_client::{ApplicationRelease, Artifact, Manifest, SignedManifest, SIGNATURE_CONTEXT};
    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

    struct Fixture { temp: tempfile::TempDir, config: DiscoveryConfig, key: Ed25519KeyPair }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            fs::create_dir_all(temp.path().join("app")).unwrap();
            fs::create_dir(temp.path().join("data")).unwrap();
            fs::write(temp.path().join("app/Bokheim.exe"), b"old app").unwrap();
            rusqlite::Connection::open(temp.path().join("data/app-state-v1.sqlite3")).unwrap().execute_batch("CREATE TABLE setting(v); INSERT INTO setting VALUES('old')").unwrap();
            let key = Ed25519KeyPair::from_seed_unchecked(&[23;32]).unwrap();
            let config = DiscoveryConfig { endpoint: "https://example.org/v1/stable".into(), channel: "stable".into(), trusted_keys: [("test".into(), key.public_key().as_ref().try_into().unwrap())].into() };
            Self { temp, config, key }
        }
        fn host(&self, version: &str) -> WindowsHost {
            WindowsHost::open(&self.temp.path().join("data"), &self.temp.path().join("app"), version, self.config.clone()).unwrap()
        }
        fn approved(&self) -> WindowsHost {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (name, contents) in [("Bokheim.exe", b"new app".as_slice())] {
                zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap(); zip.write_all(contents).unwrap();
            }
            let bytes = zip.finish().unwrap().into_inner();
            let artifact = Artifact { url: "https://example.org/releases/2/app.zip".into(), bytes: bytes.len() as u64, sha256: hex(digest(&SHA256, &bytes).as_ref()) };
            let manifest = Manifest { protocol_version: 1, channel: "stable".into(), sequence: 1, issued_at_unix: now(), expires_at_unix: now()+3600, taxonomy: None,
                application: Some(ApplicationRelease { version: "2.0.0".parse().unwrap(), name: "Release".into(), schema_version: library_database::LIBRARY_SCHEMA_VERSION as u32, taxonomy_format_versions: vec![1], artifacts: [("windows-x86_64-zip".into(), artifact)].into() }) };
            let payload = serde_json::to_string(&manifest).unwrap(); let mut message = SIGNATURE_CONTEXT.to_vec(); message.extend_from_slice(payload.as_bytes());
            let envelope = serde_json::to_vec(&SignedManifest { key_id: "test".into(), payload, signature: hex(self.key.sign(&message).as_ref()) }).unwrap();
            let mut host = self.host("1.0.0"); host.coordinator.discovered(envelope, now()).unwrap(); host.approve().unwrap();
            let stage = host.root.join("staging").join(&host.coordinator.record().approved.as_ref().unwrap().id);
            fs::create_dir_all(&stage).unwrap(); fs::write(stage.join("application.bin"), bytes).unwrap();
            host.coordinator.artifact_prepared(ArtifactKind::Application).unwrap(); host
        }
        fn executable(&self) -> Vec<u8> { fs::read(self.temp.path().join("app/Bokheim.exe")).unwrap() }
    }

    #[test]
    fn executable_and_registry_recover_after_interrupted_trial() {
        let f = Fixture::new(); let mut host = f.approved();
        assert!(host.prepare_or_recover(|_, _| panic!("same-session activation")).unwrap().is_none()); drop(host);
        let mut helper = f.host("1.0.0");
        assert!(matches!(helper.startup(None).unwrap(), WindowsStartup::Helper));
        helper.prepare_or_recover(|_, path| prepare_candidate(path, "2.0.0")).unwrap().unwrap();
        assert_eq!(f.executable(), b"new app");
        rusqlite::Connection::open(f.temp.path().join("data/app-state-v1.sqlite3")).unwrap().execute_batch("UPDATE setting SET v='trial'").unwrap(); drop(helper);
        let mut recovery = f.host("2.0.0"); recovery.prepare_or_recover(|_, _| panic!("must recover first")).unwrap();
        assert_eq!(f.executable(), b"old app");
        let value: String = rusqlite::Connection::open(f.temp.path().join("data/app-state-v1.sqlite3")).unwrap().query_row("SELECT v FROM setting", [], |r| r.get(0)).unwrap();
        assert_eq!(value, "old");
        assert_eq!(recovery.coordinator.record().approved.as_ref().unwrap().phase, Phase::Quarantined);
    }

    #[test]
    fn healthy_trial_commits_then_cleans_old_workspace() {
        let f = Fixture::new(); drop(f.approved()); let mut helper = f.host("1.0.0");
        let id = helper.prepare_or_recover(|_, path| prepare_candidate(path, "2.0.0")).unwrap().unwrap(); drop(helper);
        let mut app = f.host("2.0.0");
        assert!(matches!(app.startup(Some(&id)).unwrap(), WindowsStartup::Run)); assert!(app.is_trial());
        app.healthy().unwrap(); assert!(app.coordinator.record().approved.is_none()); drop(app);
        let mut app = f.host("2.0.0"); assert!(matches!(app.startup(None).unwrap(), WindowsStartup::Run));
        assert_eq!(f.executable(), b"new app"); assert!(!app.root.join("generations").join(id).exists());
    }

    #[cfg(windows)]
    #[test]
    fn locked_obsolete_file_does_not_block_startup_and_cleanup_retries() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        let old = f.temp.path().join("data/updates/staging").join("ab".repeat(32));
        fs::create_dir_all(&old).unwrap();
        let file = old.join("application.bin"); fs::write(&file, b"obsolete").unwrap();
        let reader = fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        let mut app = f.host("1.0.0");
        assert!(matches!(app.startup(None).unwrap(), WindowsStartup::Run));
        assert!(file.exists()); drop(app); drop(reader);
        let mut app = f.host("1.0.0");
        assert!(matches!(app.startup(None).unwrap(), WindowsStartup::Run));
        assert!(!old.exists());
    }
}
