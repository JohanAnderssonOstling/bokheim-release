//! Persistent private update state and staging for native hosts. These operations
//! belong on the backend worker, never the UI thread. They never install files
//! into the application or modify a library database.
use crate::coordinator::{additional_space, ArtifactKind, Coordinator, Phase, Record, Store, WaitReason};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    pub fn open(update_root: &Path) -> Result<Self, String> {
        fs::create_dir_all(update_root).map_err(|e| e.to_string())?;
        let connection = Connection::open(update_root.join("state.sqlite")).map_err(|e| e.to_string())?;
        connection.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(|e| e.to_string())?;
        if version > 1 {
            return Err("unsupported update state schema".into());
        }
        connection
            .execute_batch(
                "PRAGMA synchronous=FULL;
            BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS update_state(id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL, payload TEXT NOT NULL);
            PRAGMA user_version=1;
            COMMIT;",
            )
            .map_err(|e| e.to_string())?;
        Ok(Self { connection })
    }
}

impl Store for SqliteStore {
    fn load(&self) -> Result<Record, String> {
        let raw: Option<String> = self.connection.query_row("SELECT payload FROM update_state WHERE id=1", [], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(|e| format!("invalid durable update state: {e}"))).transpose().map(|r| r.unwrap_or_default())
    }
    fn save(&mut self, expected_revision: u64, next: &Record) -> Result<(), String> {
        let payload = serde_json::to_string(next).map_err(|e| e.to_string())?;
        let transaction = self.connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let current: Option<i64> = transaction.query_row("SELECT revision FROM update_state WHERE id=1", [], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        if current.unwrap_or(0) != i64::try_from(expected_revision).map_err(|_| "state revision overflow")? || next.revision != expected_revision.checked_add(1).ok_or("state revision overflow")? {
            return Err("update state changed concurrently; reload before continuing".into());
        }
        transaction
            .execute(
                "INSERT INTO update_state(id,revision,payload) VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload=excluded.payload",
                params![i64::try_from(next.revision).map_err(|_| "state revision overflow")?, payload],
            )
            .map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())
    }
}

/// Hold across discovery/preparation state changes in a background worker, or
/// across startup activation. Another process waits rather than mutating files.
pub struct UpdateLock {
    file: File,
    root: PathBuf,
}
impl UpdateLock {
    pub fn acquire(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.join("update.lock")).map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|e| format!("another process owns update work: {e}"))?;
        Ok(Self { file, root: root.canonicalize().map_err(|e| e.to_string())? })
    }
}
impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn artifact_path(root: &Path, plan_id: &str, kind: ArtifactKind) -> Result<PathBuf, String> {
    crate::decode_hex::<32>(plan_id)?;
    Ok(root.join("staging").join(plan_id).join(kind.filename()))
}

fn verify_file(path: &Path, artifact: &crate::Artifact) -> Result<(), String> {
    let input = File::open(path).map_err(|e| e.to_string())?;
    crate::blocking::copy_verified(input, &mut std::io::sink(), artifact)
}

impl UpdateLock {
    fn check_root(&self, root: &Path) -> Result<(), String> {
        if root.canonicalize().map_err(|e| e.to_string())? != self.root {
            return Err("update lock belongs to another storage root".into());
        }
        Ok(())
    }
}

impl<S: Store> Coordinator<S> {
    /// Background discovery tick. Invalid metadata and network errors are
    /// recorded for diagnostics and scheduled for automatic retry.
    pub fn check_for_updates(&mut self, root: &Path, lock: &UpdateLock, client: &reqwest::blocking::Client, now: u64) -> Result<(), String> {
        lock.check_root(root)?;
        self.reload()?;
        if !self.discovery_due(now) {
            return Ok(());
        }
        let result = crate::blocking::fetch_manifest(client, self.config()).and_then(|bytes| self.discovered(bytes, now));
        if let Err(error) = result {
            self.discovery_failed(now, error)?;
        }
        Ok(())
    }

    /// A preparation tick downloads at most one artifact. Host owns retry timing
    /// and must keep the returned lock alive while calling this method.
    /// Additional workspace and safety reserve must come from the host's real
    /// installation/migration requirements for this volume.
    pub fn prepare_next(&mut self, root: &Path, lock: &UpdateLock, client: &reqwest::blocking::Client, now: u64, workspace_bytes: u64, reserve_bytes: u64) -> Result<(), String> {
        lock.check_root(root)?;
        self.reload()?;
        if !self.preparation_due(now) {
            return Ok(());
        }
        self.approved_manifest()?;
        let plan = self.record().approved.as_ref().ok_or("no approved update")?.clone();
        for entry in &plan.artifacts {
            if plan.completed.contains(&entry.kind) {
                continue;
            }
            let path = artifact_path(root, &plan.id, entry.kind)?;
            if verify_file(&path, &entry.artifact).is_ok() {
                return self.artifact_prepared(entry.kind);
            }
            let parent = path.parent().ok_or("invalid staging path")?;
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let partial = path.with_extension("part");
            // Completed artifacts are reused. An interrupted partial download
            // restarts cleanly; range requests can be added independently.
            let mut output = File::create(&partial).map_err(|e| e.to_string())?;
            let remaining = plan.artifacts.iter().filter(|a| !plan.completed.contains(&a.kind)).try_fold(workspace_bytes, |sum, a| sum.checked_add(a.artifact.bytes).ok_or("staging size overflow"))?;
            let free = fs2::available_space(parent).map_err(|e| e.to_string())?;
            let missing = additional_space(remaining, reserve_bytes, free)?;
            if missing > 0 {
                return self.preparation_failed(now, WaitReason::Space { additional_bytes: missing }, "insufficient staging space".into());
            }
            if let Err(error) = crate::blocking::download(client, &entry.artifact, &mut output) {
                drop(output);
                let _ = fs::remove_file(&partial);
                self.preparation_failed(now, WaitReason::Retry, error)?;
                return Ok(());
            }
            output.sync_all().map_err(|e| e.to_string())?;
            drop(output);
            crate::native_files::replace(&partial, &path)?;
            return self.artifact_prepared(entry.kind);
        }
        Ok(())
    }

    /// Must be called under the update lock before activation. A missing or
    /// modified staged artifact returns to preparation, requiring a later launch.
    pub fn staged_paths(&mut self, root: &Path, lock: &UpdateLock) -> Result<Vec<(ArtifactKind, PathBuf)>, String> {
        lock.check_root(root)?;
        self.reload()?;
        self.approved_manifest()?;
        let plan = self.record().approved.as_ref().ok_or("no approved update")?.clone();
        if plan.phase != Phase::Ready && plan.phase != Phase::Activating {
            return Err("update is not ready".into());
        }
        let mut paths = Vec::new();
        for entry in &plan.artifacts {
            let path = artifact_path(root, &plan.id, entry.kind)?;
            if let Err(error) = verify_file(&path, &entry.artifact) {
                if plan.phase != Phase::Activating {
                    self.missing_artifact(entry.kind)?;
                }
                return Err(format!("staged artifact requires recovery: {error}"));
            }
            paths.push((entry.kind, path));
        }
        Ok(paths)
    }
}
