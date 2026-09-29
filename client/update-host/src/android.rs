//! Android owns APK replacement. This host owns the approved data transaction.
//! The migration callback must run prepare_candidate in a fresh private process;
//! it must return only after that process has exited and all SQLite handles close.
use super::*;

#[derive(Clone, Serialize)]
pub struct InstallRequest {
    pub path: PathBuf,
    pub version: String,
    pub bytes: u64,
    pub sha256: String,
}

pub struct AndroidHost {
    inner: Host,
    target: String,
}

impl AndroidHost {
    pub fn open(data: &Path, version: &str, config: DiscoveryConfig, target: &str) -> Result<Self, String> {
        if !target.starts_with("android-") || !target.ends_with("-apk") {
            return Err("invalid Android package target".into());
        }
        let mut inner = Host::open(data, version, config, None, PathBuf::new())?;
        let mut installed = device(version, active(&inner.root)?.as_ref())?;
        installed.target = target.into();
        inner.coordinator.set_installed(installed);
        Ok(Self { inner, target: target.into() })
    }

    fn installed(&self) -> Result<DeviceVersion, String> {
        let mut installed = device(&self.inner.version, active(&self.inner.root)?.as_ref())?;
        installed.target = self.target.clone();
        Ok(installed)
    }

    pub fn view(&self) -> UpdateView {
        self.inner.coordinator.view(now())
    }

    pub fn approve(&mut self) -> Result<(), String> {
        self.inner.approve()
    }

    /// Package identity/version validation failed before the installer opened.
    /// Live code and data are untouched; await a corrected signed publication.
    pub fn reject_package(&mut self) -> Result<(), String> {
        let _lock = UpdateLock::acquire(&self.inner.root)?;
        self.inner.coordinator.reload()?;
        if !self.inner.coordinator.record().approved.as_ref().is_some_and(|p| p.phase == Phase::Ready) {
            return Err("no ready package to reject".into());
        }
        self.inner.coordinator.quarantine_after_recovery("APK identity or version does not match approved release".into())
    }

    /// An explicit Install click obtains a reverified pinned APK. Returning from
    /// Android's installer never calls activated; only a later startup can do so.
    pub fn install_request(&mut self) -> Result<InstallRequest, String> {
        let lock = UpdateLock::acquire(&self.inner.root)?;
        self.inner.coordinator.reload()?;
        let plan = self.inner.coordinator.record().approved.as_ref().ok_or("no approved update")?.clone();
        if plan.phase != Phase::Ready {
            return Err("APK is not ready to install".into());
        }
        let paths = self.inner.coordinator.staged_paths(&self.inner.root, &lock)?;
        let path = paths.into_iter().find(|(k, _)| *k == ArtifactKind::Application).ok_or("no APK in approved update")?.1;
        let artifact = &plan.artifacts.iter().find(|a| a.kind == ArtifactKind::Application).ok_or("no APK metadata")?.artifact;
        let manifest = self.inner.coordinator.approved_manifest()?;
        let version = manifest.application.ok_or("missing APK version")?.version.to_string();
        Ok(InstallRequest { path, version, bytes: artifact.bytes, sha256: artifact.sha256.clone() })
    }

    pub fn tick(&mut self) -> Result<UpdateView, String> {
        let lock = UpdateLock::acquire(&self.inner.root)?;
        self.inner.coordinator.reload()?;
        self.acknowledge_committed()?;
        let client = update_client::blocking::client("Bokheim-Android-update/1")?;
        self.inner.coordinator.check_for_updates(&self.inner.root, &lock, &client, now())?;
        if self.inner.coordinator.preparation_due(now()) {
            let workspace = self.inner.workspace_bytes()?;
            if let Err(error) = self.inner.coordinator.prepare_next(&self.inner.root, &lock, &client, now(), workspace, 0) {
                self.inner.coordinator.preparation_failed(now(), WaitReason::Retry, error)?;
            }
        }
        Ok(self.view())
    }

    fn acknowledge_committed(&mut self) -> Result<(), String> {
        if let Some(journal) = Journal::load(&self.inner.root)? {
            if journal.state == State::Committed && self.inner.coordinator.record().approved.as_ref().is_some_and(|p| p.id == journal.plan_id) {
                self.inner.coordinator.activated(self.installed()?)?;
            }
        }
        Ok(())
    }

    /// Called before the registry/backend opens. Returns whether startup is a
    /// trial: its background tasks must remain gated until healthy is called.
    pub fn startup(&mut self, prepare: impl FnOnce(&Path) -> Result<(), String>) -> Result<bool, String> {
        let lock = UpdateLock::acquire(&self.inner.root)?;
        self.inner.coordinator.reload()?;
        if let Some(journal) = Journal::load(&self.inner.root)? {
            if journal.state == State::Committed {
                self.acknowledge_committed()?;
                self.inner.cleanup_committed()?;
            } else {
                journal.recover(&self.inner.root)?;
                self.inner.coordinator.quarantine_after_recovery("Android data startup was interrupted; restored previous data".into())?;
            }
            Journal::clear(&self.inner.root)?;
            self.inner.coordinator.set_installed(self.installed()?);
        }
        if self.inner.coordinator.activation_due() {
            let manifest = self.inner.coordinator.approved_manifest()?;
            let needs_apk = self.inner.coordinator.record().approved.as_ref().unwrap().artifacts.iter().any(|a| a.kind == ArtifactKind::Application);
            let installed = self.installed()?;
            let apk_present = !needs_apk || manifest.application.as_ref().is_some_and(|app| installed.application >= app.version);
            if apk_present {
                if let Err(error) = self.promote(&lock, prepare) {
                    if let Some(journal) = Journal::load(&self.inner.root)? {
                        journal.recover(&self.inner.root)?;
                        self.inner.coordinator.quarantine_after_recovery(error)?;
                        Journal::clear(&self.inner.root)?;
                    } else if self.inner.coordinator.record().approved.as_ref().is_some_and(|p| p.phase == Phase::Activating) {
                        // Candidate failed before any live replacement.
                        self.inner.coordinator.quarantine_after_recovery(error)?;
                    } else if self.inner.coordinator.record().approved.as_ref().is_some_and(|p| p.phase == Phase::Ready) {
                        self.inner.coordinator.activation_recovered_for_retry(now(), WaitReason::Retry, error)?;
                    }
                    self.inner.coordinator.set_installed(self.installed()?);
                } else {
                    self.inner.trial = true;
                }
            }
        }
        if let Some(taxonomy) = active(&self.inner.root)? {
            subject_projection::installed::activate(&taxonomy.path)?;
        }
        Ok(self.inner.trial)
    }

    fn promote(&mut self, lock: &UpdateLock, prepare: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
        let paths = self.inner.coordinator.staged_paths(&self.inner.root, lock)?;
        let deficit = self.inner.workspace_bytes()?.saturating_sub(fs2::available_space(&self.inner.root).map_err(|e| e.to_string())?);
        if deficit > 0 {
            self.inner.coordinator.activation_recovered_for_retry(now(), WaitReason::Space { additional_bytes: deficit }, "insufficient migration workspace".into())?;
            return Err("waiting for migration workspace".into());
        }
        let manifest = self.inner.coordinator.begin_activation()?;
        let plan = self.inner.coordinator.record().approved.as_ref().unwrap().clone();
        let work = self.inner.root.join("generations").join(&plan.id);
        fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut replacements = Vec::new();
        let mut databases = Vec::new();
        for (index, path) in self.inner.databases()?.into_iter().enumerate() {
            let previous = work.join(format!("{index}.prior.sqlite3"));
            let prepared = work.join(format!("{index}.next.sqlite3"));
            activation::copy_database(&path, &previous)?;
            activation::copy_database(&previous, &prepared)?;
            if path.file_name().is_some_and(|name| name == "library.db") {
                databases.push(prepared.clone());
            }
            replacements.push(Replacement { destination: path, previous: Some(previous), prepared, sqlite: true });
        }
        let taxonomy = paths.iter().find(|(kind, _)| *kind == ArtifactKind::Taxonomy).map(|(_, source)| {
            let destination = work.join("taxonomy.sqlite3");
            activation::copy_file(source, &destination)?;
            Ok::<_, String>(destination)
        }).transpose()?;
        let result = work.join("prepared.json");
        if result.try_exists().map_err(|e| e.to_string())? {
            fs::remove_file(&result).map_err(|e| e.to_string())?;
        }
        let job = PrepareJob {
            databases, taxonomy, previous_taxonomy: active(&self.inner.root)?,
            expected_taxonomy: manifest.taxonomy.as_ref().map(|t| (t.release_id, t.format_version)), result: result.clone(),
        };
        let job_path = work.join("prepare-job.json");
        activation::write_json(&job_path, &job)?;
        prepare(&job_path)?;
        let candidate: Prepared = activation::read_json(&result)?;
        if candidate.device.application != self.inner.version.parse().map_err(|e| format!("{e}"))?
            || candidate.device.schema != library_database::LIBRARY_SCHEMA_VERSION as u32 {
            return Err("migration helper differs from installed APK".into());
        }
        if plan.artifacts.iter().any(|a| a.kind == ArtifactKind::Taxonomy)
            && candidate.taxonomy.as_ref().map(|t| t.release) != manifest.taxonomy.as_ref().map(|t| t.release_id) {
            return Err("migration helper did not prepare approved taxonomy".into());
        }
        if let Some(app) = manifest.application.as_ref().filter(|_| plan.artifacts.iter().any(|a| a.kind == ArtifactKind::Application)) {
            if candidate.device.schema < app.schema_version {
                return Err("installed APK does not provide required schema".into());
            }
        }
        let pointer = self.inner.root.join("active-taxonomy.json");
        let prepared = work.join("active-taxonomy.json");
        activation::write_json(&prepared, &candidate.taxonomy)?;
        let previous = if pointer.try_exists().map_err(|e| e.to_string())? {
            let previous = work.join("previous-taxonomy.json");
            activation::copy_file(&pointer, &previous)?;
            Some(previous)
        } else { None };
        replacements.push(Replacement { destination: pointer, previous, prepared, sqlite: false });
        activation::sync_dir(&work)?;
        activation::sync_dir(work.parent().unwrap())?;
        activation::sync_dir(&self.inner.root)?;
        Journal { plan_id: plan.id, previous_version: plan.installed_at_approval.application.to_string(), next_version: self.inner.version.clone(), appimage: None, replacements, state: State::Applying }.promote(&self.inner.root)
    }

    pub fn healthy(&mut self) -> Result<(), String> {
        if !self.inner.trial { return Ok(()); }
        let _lock = UpdateLock::acquire(&self.inner.root)?;
        let mut journal = Journal::load(&self.inner.root)?.ok_or("missing Android data activation journal")?;
        let installed = self.installed()?;
        journal.commit(&self.inner.root)?;
        if let Err(error) = self.inner.coordinator.activated(installed) {
            log::warn!("Android update acknowledgement will retry: {error}");
        }
        self.inner.trial = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{digest::{digest, SHA256}, signature::{Ed25519KeyPair, KeyPair}};
    use update_client::{ApplicationRelease, Artifact, Manifest, SignedManifest, SIGNATURE_CONTEXT};

    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

    struct Fixture {
        dir: tempfile::TempDir,
        config: DiscoveryConfig,
        key: Ed25519KeyPair,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            rusqlite::Connection::open(dir.path().join("app-state-v1.sqlite3")).unwrap()
                .execute_batch("CREATE TABLE setting(v); INSERT INTO setting VALUES('prior')").unwrap();
            let key = Ed25519KeyPair::from_seed_unchecked(&[17;32]).unwrap();
            let config = DiscoveryConfig { endpoint: "https://example.org/v1/stable".into(), channel: "stable".into(),
                trusted_keys: [("test".into(), key.public_key().as_ref().try_into().unwrap())].into() };
            Self { dir, config, key }
        }
        fn host(&self, version: &str) -> AndroidHost {
            AndroidHost::open(self.dir.path(), version, self.config.clone(), "android-aarch64-apk").unwrap()
        }
        fn approved(&self) -> AndroidHost {
            let mut host = self.host("1.0.0");
            let manifest = Manifest { protocol_version: 1, channel: "stable".into(), sequence: 1,
                issued_at_unix: now(), expires_at_unix: now()+3600, taxonomy: None,
                application: Some(ApplicationRelease { version: "2.0.0".parse().unwrap(), name: "Test".into(),
                    schema_version: library_database::LIBRARY_SCHEMA_VERSION as u32, taxonomy_format_versions: vec![1],
                    artifacts: [("android-aarch64-apk".into(), Artifact { url: "https://example.org/releases/2/app.apk".into(), bytes: 3, sha256: hex(digest(&SHA256, b"apk").as_ref()) })].into() }) };
            let payload = serde_json::to_string(&manifest).unwrap();
            let mut message = SIGNATURE_CONTEXT.to_vec(); message.extend_from_slice(payload.as_bytes());
            let envelope = serde_json::to_vec(&SignedManifest { key_id: "test".into(), payload, signature: hex(self.key.sign(&message).as_ref()) }).unwrap();
            host.inner.coordinator.discovered(envelope, now()).unwrap();
            assert_eq!(host.view(), UpdateView::Available);
            assert!(host.install_request().is_err());
            host.approve().unwrap();
            let stage = host.inner.root.join("staging").join(&host.inner.coordinator.record().approved.as_ref().unwrap().id);
            fs::create_dir_all(&stage).unwrap(); fs::write(stage.join("application.bin"), b"apk").unwrap();
            host.inner.coordinator.artifact_prepared(ArtifactKind::Application).unwrap();
            host
        }
        fn value(&self) -> String {
            rusqlite::Connection::open(self.dir.path().join("app-state-v1.sqlite3")).unwrap()
                .query_row("SELECT v FROM setting", [], |row| row.get(0)).unwrap()
        }
    }

    #[test]
    fn consent_and_installed_apk_gate_data_activation() {
        let f = Fixture::new();
        let mut host = f.approved();
        assert_eq!(host.install_request().unwrap().version, "2.0.0");
        assert!(!host.startup(|_| panic!("same-session activation")).unwrap());
        drop(host);
        let mut host = f.host("1.0.0");
        assert!(!host.startup(|_| panic!("APK not installed")).unwrap());
        assert_eq!(host.view(), UpdateView::Ready { action: update_client::coordinator::UpdateAction::Install });
        drop(host);
        let mut host = f.host("2.0.0");
        assert!(host.startup(|path| prepare_candidate(path, "2.0.0")).unwrap());
        host.healthy().unwrap();
        assert!(host.inner.coordinator.record().approved.is_none());
        assert_eq!(f.value(), "prior");
        drop(host);
        assert!(!f.host("2.0.0").startup(|_| panic!("already committed")).unwrap());
    }

    #[test]
    fn interrupted_trial_restores_database_before_next_startup() {
        let f = Fixture::new(); drop(f.approved());
        let mut host = f.host("2.0.0");
        assert!(host.startup(|path| prepare_candidate(path, "2.0.0")).unwrap());
        rusqlite::Connection::open(f.dir.path().join("app-state-v1.sqlite3")).unwrap()
            .execute_batch("UPDATE setting SET v='trial change'").unwrap();
        drop(host);
        let mut host = f.host("2.0.0");
        assert!(!host.startup(|_| panic!("quarantined trial")).unwrap());
        assert_eq!(f.value(), "prior");
        assert_eq!(host.inner.coordinator.record().approved.as_ref().unwrap().phase, Phase::Quarantined);
    }

    #[test]
    fn corrupt_staged_apk_retries_without_touching_live_data() {
        let f = Fixture::new(); let mut host = f.approved();
        let path = host.install_request().unwrap().path;
        fs::write(path, b"bad").unwrap();
        assert!(host.install_request().is_err()); drop(host);
        let mut host = f.host("2.0.0");
        assert!(!host.startup(|_| panic!("unverified APK")).unwrap());
        assert_eq!(host.inner.coordinator.record().approved.as_ref().unwrap().phase, Phase::Preparing);
        assert_eq!(f.value(), "prior");
    }

    #[test]
    fn invalid_apk_is_quarantined_without_repeated_install_prompts() {
        let f = Fixture::new(); let mut host = f.approved();
        host.reject_package().unwrap();
        assert!(host.install_request().is_err());
        assert_eq!(host.view(), UpdateView::Hidden);
        assert_eq!(f.value(), "prior");
    }
}
