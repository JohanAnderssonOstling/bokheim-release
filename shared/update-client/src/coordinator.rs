//! Durable update lifecycle. Hosts own I/O, scheduling, and activation. Every
//! transition is persisted before it is returned to a caller or shown in the UI.
use crate::{verify, Artifact, DiscoveryConfig, Installed, Manifest};
use ring::digest::{digest, SHA256};
use semver::Version;
use serde::{Deserialize, Serialize};

pub const CHECK_INTERVAL_SECONDS: u64 = 6 * 60 * 60;
const MAX_RETRY_SECONDS: u64 = 60 * 60;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum UpdateAction {
    Update,
    Restart,
    Install,
    GrantPermission,
}

/// Technical errors stay in diagnostics. Only actionable reasons enter the UI.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum UpdateView {
    #[default]
    Hidden,
    Available,
    Preparing,
    WaitingForConnection,
    Ready {
        action: UpdateAction,
    },
    NeedsSpace {
        additional_bytes: u64,
    },
    NeedsPermission,
    Activating,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceVersion {
    pub application: Version,
    pub schema: u32,
    pub taxonomy_formats: Vec<u32>,
    pub taxonomy_release: u64,
    pub target: String,
}

impl DeviceVersion {
    fn installed(&self) -> Installed<'_> {
        Installed { application_version: &self.application, schema_version: self.schema, taxonomy_format_versions: &self.taxonomy_formats, taxonomy_release_id: self.taxonomy_release, target: &self.target }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ArtifactKind {
    Application,
    Taxonomy,
}

impl ArtifactKind {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Application => "application.bin",
            Self::Taxonomy => "taxonomy.sqlite3",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedArtifact {
    pub kind: ArtifactKind,
    pub artifact: Artifact,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Ready,
    Activating,
    Quarantined,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WaitReason {
    Retry,
    Connection,
    Space { additional_bytes: u64 },
    Permission,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovedPlan {
    pub id: String,
    /// Retain the exact authenticated publication, not just extracted URLs.
    pub envelope: Vec<u8>,
    pub approved_at: u64,
    pub installed_at_approval: DeviceVersion,
    pub artifacts: Vec<PlannedArtifact>,
    pub phase: Phase,
    pub completed: Vec<ArtifactKind>,
    pub ready_session: Option<String>,
    pub retry_at: u64,
    pub failures: u32,
    pub waiting: Option<WaitReason>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Record {
    pub revision: u64,
    pub trust_scope: String,
    pub highest_sequence: u64,
    pub envelope_digest: String,
    pub discovered: Option<Vec<u8>>,
    pub next_check_at: u64,
    pub discovery_failures: u32,
    pub last_discovery_error: Option<String>,
    pub approved: Option<ApprovedPlan>,
}

/// CAS prevents separate hosts or stale settings windows from overwriting a
/// newer decision. A conflict must reload; it must not retry a stale write.
pub trait Store {
    fn load(&self) -> Result<Record, String>;
    fn save(&mut self, expected_revision: u64, next: &Record) -> Result<(), String>;
}

pub struct Coordinator<S> {
    store: S,
    config: DiscoveryConfig,
    device: DeviceVersion,
    session: String,
    record: Record,
}

fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn trust_scope(config: &DiscoveryConfig) -> String {
    let mut bytes = config.channel.as_bytes().to_vec();
    for (id, key) in &config.trusted_keys {
        bytes.extend_from_slice(&(id.len() as u64).to_be_bytes());
        bytes.extend_from_slice(id.as_bytes());
        bytes.extend_from_slice(key);
    }
    hash(&bytes)
}

fn retry_delay(failures: u32) -> u64 {
    30_u64.saturating_mul(1_u64 << failures.saturating_sub(1).min(7)).min(MAX_RETRY_SECONDS)
}

impl<S: Store> Coordinator<S> {
    /// Session ID must identify a process launch, not a window or activity resume.
    pub fn open(store: S, config: DiscoveryConfig, device: DeviceVersion, session: String) -> Result<Self, String> {
        config.validate()?;
        if session.is_empty() {
            return Err("update coordinator requires a launch session ID".into());
        }
        let record = store.load()?;
        let mut coordinator = Self { store, config, device, session, record };
        let scope = trust_scope(&coordinator.config);
        if coordinator.record.trust_scope != scope {
            // Adding a rotation key must not erase an approved plan or reset its
            // replay floor while the original signer is still trusted.
            let retained = if coordinator.record.approved.is_some() {
                coordinator.approved_manifest()?;
                true
            } else {
                coordinator.record.discovered.as_ref().is_some_and(|bytes| {
                    let Ok(envelope) = serde_json::from_slice::<crate::SignedManifest>(bytes) else { return false };
                    let Ok(manifest) = serde_json::from_str::<Manifest>(&envelope.payload) else { return false };
                    verify(bytes, &coordinator.config, manifest.issued_at_unix, coordinator.record.highest_sequence).is_ok()
                })
            };
            let mut next = if retained { coordinator.record.clone() } else { Record { revision: coordinator.record.revision, ..Record::default() } };
            next.trust_scope = scope;
            next.next_check_at = 0;
            coordinator.commit(next)?;
        }
        Ok(coordinator)
    }

    pub fn record(&self) -> &Record {
        &self.record
    }
    /// Refresh observed installed versions after the host restores a generation.
    /// This does not change the pinned versions recorded when a plan was approved.
    pub fn set_installed(&mut self, device: DeviceVersion) {
        self.device = device;
    }

    pub fn config(&self) -> &DiscoveryConfig {
        &self.config
    }
    pub fn reload(&mut self) -> Result<(), String> {
        self.record = self.store.load()?;
        Ok(())
    }

    fn commit(&mut self, mut next: Record) -> Result<(), String> {
        let expected = self.record.revision;
        next.revision = expected.checked_add(1).ok_or("update state revision exhausted")?;
        self.store.save(expected, &next)?;
        self.record = next;
        Ok(())
    }

    pub fn discovery_due(&self, now: u64) -> bool {
        now >= self.record.next_check_at
    }

    pub fn discovered(&mut self, envelope: Vec<u8>, now: u64) -> Result<(), String> {
        let verified = verify(&envelope, &self.config, now, self.record.highest_sequence)?;
        let fingerprint = hash(&envelope);
        if verified.manifest().sequence == self.record.highest_sequence && fingerprint != self.record.envelope_digest {
            return Err("update publication changed without increasing its sequence".into());
        }
        let mut next = self.record.clone();
        if let Some(failed) = next.approved.as_ref().filter(|p| p.phase == Phase::Quarantined) {
            let offer = verified.offer(&self.device.installed());
            let candidates = offer.application.map(|(_, artifact)| (ArtifactKind::Application, artifact)).into_iter().chain(offer.taxonomy.map(|taxonomy| (ArtifactKind::Taxonomy, &taxonomy.artifact))).collect::<Vec<_>>();
            // A re-signed publication of the same broken bytes is not a fix.
            // A corrected plan requires fresh explicit authorization.
            let corrected = !candidates.is_empty() && (candidates.len() != failed.artifacts.len() || candidates.iter().any(|(kind, artifact)| !failed.artifacts.iter().any(|old| old.kind == *kind && old.artifact.sha256 == artifact.sha256)));
            if corrected {
                next.approved = None;
            }
        }
        next.highest_sequence = verified.manifest().sequence;
        next.envelope_digest = fingerprint;
        next.discovered = Some(envelope);
        next.next_check_at = now.saturating_add(CHECK_INTERVAL_SECONDS);
        next.discovery_failures = 0;
        next.last_discovery_error = None;
        // Approved plans stay pinned even when a new release is published.
        self.commit(next)
    }

    pub fn discovery_failed(&mut self, now: u64, diagnostic: String) -> Result<(), String> {
        let mut next = self.record.clone();
        next.discovery_failures = next.discovery_failures.saturating_add(1);
        next.next_check_at = now.saturating_add(retry_delay(next.discovery_failures));
        next.last_discovery_error = Some(diagnostic);
        self.commit(next)
    }

    pub fn view(&self, now: u64) -> UpdateView {
        if let Some(plan) = &self.record.approved {
            if plan.phase == Phase::Quarantined {
                return UpdateView::Hidden;
            }
            match plan.waiting {
                Some(WaitReason::Connection) => return UpdateView::WaitingForConnection,
                Some(WaitReason::Space { additional_bytes }) => return UpdateView::NeedsSpace { additional_bytes },
                Some(WaitReason::Permission) => return UpdateView::NeedsPermission,
                _ => {}
            }
            return match plan.phase {
                Phase::Ready => UpdateView::Ready { action: if self.device.target.ends_with("-apk") && plan.artifacts.iter().any(|a| a.kind == ArtifactKind::Application) { UpdateAction::Install } else { UpdateAction::Restart } },
                Phase::Activating => UpdateView::Activating,
                Phase::Preparing => UpdateView::Preparing,
                Phase::Quarantined => UpdateView::Hidden,
            };
        }
        let Some(bytes) = &self.record.discovered else { return UpdateView::Hidden };
        let Ok(manifest) = verify(bytes, &self.config, now, self.record.highest_sequence) else { return UpdateView::Hidden };
        let offer = manifest.offer(&self.device.installed());
        if offer.application.is_some() || offer.taxonomy.is_some() {
            UpdateView::Available
        } else {
            UpdateView::Hidden
        }
    }

    /// Explicit user authorization, committed before downloads can begin.
    pub fn approve(&mut self, now: u64) -> Result<(), String> {
        if self.record.approved.is_some() {
            return Ok(());
        }
        let envelope = self.record.discovered.as_ref().ok_or("no update available")?;
        let manifest = verify(envelope, &self.config, now, self.record.highest_sequence)?;
        let offer = manifest.offer(&self.device.installed());
        let mut artifacts = Vec::new();
        if let Some((_, artifact)) = offer.application {
            artifacts.push(PlannedArtifact { kind: ArtifactKind::Application, artifact: artifact.clone() });
        }
        if let Some(taxonomy) = offer.taxonomy {
            artifacts.push(PlannedArtifact { kind: ArtifactKind::Taxonomy, artifact: taxonomy.artifact.clone() });
        }
        if artifacts.is_empty() {
            return Err("no compatible update available".into());
        }
        let mut next = self.record.clone();
        next.approved = Some(ApprovedPlan {
            id: hash(envelope),
            envelope: envelope.clone(),
            approved_at: now,
            installed_at_approval: self.device.clone(),
            artifacts,
            phase: Phase::Preparing,
            completed: Vec::new(),
            ready_session: None,
            retry_at: now,
            failures: 0,
            waiting: None,
            last_error: None,
        });
        self.commit(next)
    }

    /// Revalidate the pinned signature using its approval time: feed expiry must
    /// not revoke an already-authorized plan during a long offline interval.
    pub fn approved_manifest(&self) -> Result<Manifest, String> {
        let plan = self.record.approved.as_ref().ok_or("no approved update")?;
        let verified = verify(&plan.envelope, &self.config, plan.approved_at, 0)?;
        let offer = verified.offer(&plan.installed_at_approval.installed());
        let expected = offer.application.map(|(_, artifact)| (ArtifactKind::Application, artifact)).into_iter().chain(offer.taxonomy.map(|taxonomy| (ArtifactKind::Taxonomy, &taxonomy.artifact))).collect::<Vec<_>>();
        if hash(&plan.envelope) != plan.id || expected.len() != plan.artifacts.len() || expected.iter().zip(&plan.artifacts).any(|((kind, artifact), stored)| *kind != stored.kind || **artifact != stored.artifact) {
            return Err("stored update plan does not match its signed publication".into());
        }
        Ok(verified.manifest().clone())
    }

    pub fn preparation_due(&self, now: u64) -> bool {
        self.record.approved.as_ref().is_some_and(|p| p.phase == Phase::Preparing && now >= p.retry_at)
    }

    /// Call only after length/hash verification and durable file publication.
    pub fn artifact_prepared(&mut self, kind: ArtifactKind) -> Result<(), String> {
        self.approved_manifest()?;
        let mut next = self.record.clone();
        let plan = next.approved.as_mut().ok_or("no approved update")?;
        if plan.phase != Phase::Preparing || !plan.artifacts.iter().any(|a| a.kind == kind) {
            return Err("unexpected prepared artifact".into());
        }
        if !plan.completed.contains(&kind) {
            plan.completed.push(kind);
        }
        plan.failures = 0;
        plan.waiting = None;
        plan.last_error = None;
        if plan.artifacts.iter().all(|a| plan.completed.contains(&a.kind)) {
            plan.phase = Phase::Ready;
            plan.ready_session = Some(self.session.clone());
        }
        self.commit(next)
    }

    /// Temporary errors retry automatically. A missing/corrupt prepared file
    /// returns the plan to preparation; no activation occurs in this session.
    pub fn preparation_failed(&mut self, now: u64, reason: WaitReason, diagnostic: String) -> Result<(), String> {
        let mut next = self.record.clone();
        let plan = next.approved.as_mut().ok_or("no approved update")?;
        if plan.phase != Phase::Preparing {
            return Err("update is not preparing".into());
        }
        plan.failures = plan.failures.saturating_add(1);
        plan.retry_at = now.saturating_add(retry_delay(plan.failures));
        plan.waiting = Some(reason);
        plan.last_error = Some(diagnostic);
        self.commit(next)
    }

    pub fn missing_artifact(&mut self, kind: ArtifactKind) -> Result<(), String> {
        let mut next = self.record.clone();
        let plan = next.approved.as_mut().ok_or("no approved update")?;
        if plan.phase == Phase::Activating {
            return Err("activation requires recovery before restaging".into());
        }
        plan.completed.retain(|k| *k != kind);
        plan.phase = Phase::Preparing;
        plan.ready_session = None;
        plan.retry_at = 0;
        self.commit(next)
    }

    pub fn activation_due(&self) -> bool {
        self.record.approved.as_ref().is_some_and(|p| (p.phase == Phase::Ready || p.phase == Phase::Activating) && p.ready_session.as_deref().is_some_and(|ready| ready != self.session))
    }

    /// Host calls this before opening libraries. Android must first verify the
    /// required APK is installed; returning a plan never installs anything.
    pub fn begin_activation(&mut self) -> Result<Manifest, String> {
        if !self.activation_due() {
            return Err("update activation must wait for a later launch".into());
        }
        let manifest = self.approved_manifest()?;
        let plan = self.record.approved.as_ref().unwrap();
        if plan.artifacts.iter().any(|a| a.kind == ArtifactKind::Application) && self.device.target.ends_with("-apk") {
            let required = manifest.application.as_ref().ok_or("APK update lacks application metadata")?;
            if self.device.application < required.version {
                return Err("Android installation is still pending".into());
            }
        }
        let mut next = self.record.clone();
        next.approved.as_mut().unwrap().phase = Phase::Activating;
        self.commit(next)?;
        Ok(manifest)
    }

    /// Host must have validated every required installation and data migration.
    pub fn activated(&mut self, installed: DeviceVersion) -> Result<(), String> {
        let manifest = self.approved_manifest()?;
        let plan = self.record.approved.as_ref().unwrap();
        if plan.phase != Phase::Activating {
            return Err("update is not activating".into());
        }
        for artifact in &plan.artifacts {
            match artifact.kind {
                ArtifactKind::Application => {
                    let release = manifest.application.as_ref().ok_or("missing application release")?;
                    if installed.application < release.version || installed.schema < release.schema_version {
                        return Err("application or schema upgrade is incomplete".into());
                    }
                }
                ArtifactKind::Taxonomy => {
                    if installed.taxonomy_release < manifest.taxonomy.as_ref().ok_or("missing taxonomy release")?.release_id {
                        return Err("taxonomy upgrade is incomplete".into());
                    }
                }
            }
        }
        let mut next = self.record.clone();
        next.approved = None;
        self.commit(next)?;
        self.device = installed;
        Ok(())
    }

    /// Retry only after the host restored every active file from its journal.
    pub fn activation_recovered_for_retry(&mut self, now: u64, reason: WaitReason, diagnostic: String) -> Result<(), String> {
        let mut next = self.record.clone();
        let plan = next.approved.as_mut().ok_or("no approved update")?;
        plan.phase = Phase::Preparing;
        plan.completed.clear();
        plan.ready_session = None;
        self.commit(next)?;
        self.preparation_failed(now, reason, diagnostic)
    }

    /// Call after restoring a usable previous generation. Do not quarantine
    /// transient transport failures or claim rollback succeeded on error.
    pub fn quarantine_after_recovery(&mut self, diagnostic: String) -> Result<(), String> {
        let mut next = self.record.clone();
        let plan = next.approved.as_mut().ok_or("no approved update")?;
        plan.phase = Phase::Quarantined;
        plan.last_error = Some(diagnostic);
        plan.waiting = None;
        self.commit(next)
    }
}

/// Calculate each filesystem independently. `required_remaining_bytes` includes
/// host-estimated backup/workspace and unallocated download/staging bytes. Free
/// space already excludes allocated files, so do not subtract those twice.
pub fn additional_space(required_remaining_bytes: u64, safety_reserve_bytes: u64, usable_free_bytes: u64) -> Result<u64, String> {
    Ok(required_remaining_bytes.checked_add(safety_reserve_bytes).ok_or("update space estimate overflow")?.saturating_sub(usable_free_bytes))
}

#[cfg(test)]
mod tests;
