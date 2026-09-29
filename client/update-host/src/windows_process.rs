//! Windows process handoff. The helper gate closes the gap between releasing
//! desktop ownership and the trial acquiring it. Only that helper's child may
//! bypass the gate; every live-file mutation additionally holds desktop ownership.
use crate::{activation, windows::WindowsHost, DiscoveryConfig};
use fs2::FileExt;
use std::os::windows::process::CommandExt;
use serde::{Deserialize, Serialize};
use std::{fs::{self, File, OpenOptions}, path::{Path, PathBuf}, process::{Child, Command}, time::{Duration, Instant}};

fn error(e: impl std::fmt::Display) -> String { e.to_string() }
fn arg(name: &str) -> Option<PathBuf> {
    let args: Vec<_> = std::env::args_os().collect();
    args.windows(2).find(|v| v[0] == name).map(|v| PathBuf::from(&v[1]))
}
fn open_lock(path: &Path) -> Result<File, String> {
    OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).map_err(error)
}
fn try_lock(file: &File) -> Result<bool, String> {
    match FileExt::try_lock_exclusive(file) {
        Ok(()) => Ok(true),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(false),
        Err(e) => Err(error(e)),
    }
}
fn gate(data: &Path) -> Result<File, String> { open_lock(&data.join("windows-update-launch.lock")) }
fn owner(data: &Path) -> PathBuf { data.join("updates/helper-owner.json") }
fn canonical(path: &Path) -> Result<PathBuf, String> { path.canonicalize().map_err(error) }

#[derive(Serialize, Deserialize)]
struct Job { installation: PathBuf, parent: u32 }
#[derive(Serialize, Deserialize)]
struct Permit { trial: Option<String> }

pub struct Entry {
    _gate: Option<File>,
    directory: Option<PathBuf>,
    pub trial: Option<String>,
    data: PathBuf,
}
impl Entry {
    /// Call only after DesktopInstance::Primary has been acquired. Returning
    /// releases the normal-launch gate, or acknowledges the supervised child.
    pub fn acquired(self) -> Result<Option<String>, String> {
        if self._gate.is_some() { cleanup_helpers(&self.data); }
        if let Some(directory) = self.directory {
            activation::write_json(&directory.join(format!("accepted-{}.json", std::process::id())), &true)?;
        }
        Ok(self.trial)
    }
}

/// None means another launcher/helper owns startup; the caller must not open
/// library databases. A supervised child is bound to the current helper, its PID
/// and the expected installed executable, so stale restart arguments cannot work.
pub fn enter(data: &Path) -> Result<Option<Entry>, String> {
    let lock = gate(data)?;
    let available = try_lock(&lock)?;
    let Some(directory) = arg("--bokheim-supervised-launch") else {
        if !available {
            fs::write(data.join("desktop-instance.activate"), []).map_err(error)?;
            return Ok(None);
        }
        return Ok(Some(Entry { _gate: Some(lock), directory: None, trial: None, data: data.into() }));
    };
    if available { return Err("supervised launch has no active helper".into()); }
    let directory = canonical(&directory)?;
    let helper = validate_directory(data, directory.parent().ok_or("missing launch directory parent")?)?;
    let current: PathBuf = activation::read_json(&owner(data))?;
    if current != helper { return Err("supervised launch belongs to a different helper".into()); }
    let job: Job = activation::read_json(&helper.join("job.json"))?;
    if canonical(&std::env::current_exe().map_err(error)?)? != canonical(&job.installation.join("Bokheim.exe"))? {
        return Err("supervised launch executable differs from installation".into());
    }
    let path = directory.join(format!("permit-{}.json", std::process::id()));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        if Instant::now() >= deadline { return Err("helper did not authorize child launch".into()); }
        std::thread::sleep(Duration::from_millis(25));
    }
    let permit: Permit = activation::read_json(&path)?;
    Ok(Some(Entry { _gate: None, directory: Some(directory), trial: permit.trial, data: data.into() }))
}

// Called under the launch gate, before any helper can take ownership. Locked
// files are left for the next startup; cleanup never prevents application use.
fn cleanup_helpers(data: &Path) {
    let Ok(entries) = fs::read_dir(data.join("updates/helpers")) else { return };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if let Err(error) = fs::remove_dir_all(entry.path()) {
                log::debug!("old update helper cleanup will retry: {error}");
            }
        }
    }
    let _ = fs::remove_file(owner(data));
}

fn validate_directory(data: &Path, directory: &Path) -> Result<PathBuf, String> {
    let directory = canonical(directory)?;
    if directory.parent() != Some(canonical(&data.join("updates/helpers"))?.as_path()) {
        return Err("helper directory is outside application update storage".into());
    }
    Ok(directory)
}

/// Caller still owns DesktopInstance. The helper signals readiness only after
/// opening a handle to this process and taking the launch gate. The caller must
/// then exit; dropping the desktop lock alone does not satisfy the handoff.
pub fn spawn_helper(data: &Path, installation: &Path) -> Result<(), String> {
    let helpers = data.join("updates/helpers");
    fs::create_dir_all(&helpers).map_err(error)?;
    let unique = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let directory = helpers.join(unique);
    fs::create_dir(&directory).map_err(error)?;
    let directory = canonical(&directory)?;
    let executable = directory.join("Bokheim.exe");
    let prepared = (|| {
        activation::copy_file(&std::env::current_exe().map_err(error)?, &executable)?;
        activation::write_json(&directory.join("job.json"), &Job { installation: canonical(installation)?, parent: std::process::id() })?;
        Command::new(&executable).creation_flags(0x08000000).arg("--bokheim-update-helper").arg(&directory).spawn().map_err(error)
    })();
    let mut child = match prepared {
        Ok(child) => child,
        Err(error) => {
            // Remove a partial copy before reporting the remaining space needed.
            // No process was created, so no running image can reference it.
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if directory.join("ready.json").exists() { return Ok(()); }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = fs::remove_dir_all(&directory);
                return Err(format!("update helper exited: {status}"));
            }
            Ok(None) => {}
            Err(error) => {
                update_client::supervisor::stop_process(&mut child)?;
                let _ = fs::remove_dir_all(&directory);
                return Err(error.to_string());
            }
        }
        if Instant::now() >= deadline {
            update_client::supervisor::stop_process(&mut child)?;
            let _ = fs::remove_dir_all(&directory);
            return Err("update helper startup timed out".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn helper_entry(data: &Path, version: &str, config: impl FnOnce() -> Result<DiscoveryConfig, String>) -> Result<bool, String> {
    let Some(directory) = arg("--bokheim-update-helper") else { return Ok(false); };
    let directory = validate_directory(data, &directory)?;
    if canonical(&std::env::current_exe().map_err(error)?)? != canonical(&directory.join("Bokheim.exe"))? {
        return Err("update helper must run from its private copy".into());
    }
    let job: Job = activation::read_json(&directory.join("job.json"))?;
    let parent = ParentProcess::open(job.parent)?;
    let launch = gate(data)?;
    if !try_lock(&launch)? { return Err("another update helper owns startup".into()); }
    activation::write_json(&owner(data), &directory)?;
    activation::write_json(&directory.join("ready.json"), &true)?;
    parent.wait(Duration::from_secs(120))?;
    let desktop = open_lock(&data.join("desktop-instance.lock"))?;
    if !try_lock(&desktop)? { return Err("desktop ownership was not released".into()); }
    let mut host = WindowsHost::open(data, &job.installation, version, config()?)?;
    let trial = host.prepare_or_recover_in_process()?;
    drop(desktop);
    launch_and_supervise(data, &directory, &mut host, trial.as_deref())?;
    let _ = fs::remove_file(owner(data));
    // Own EXE remains locked until exit. Future normal startup cleans helpers.
    Ok(true)
}

fn launch_and_supervise(data: &Path, directory: &Path, host: &mut WindowsHost, trial: Option<&str>) -> Result<(), String> {
    let executable = host.installation.join("Bokheim.exe");
    let outcome = match spawn_application(directory, &executable, trial) {
        Ok(mut child) => {
            if let Some(id) = trial {
                update_client::supervisor::wait_for_trial(&mut child.process, &host.root, id, Duration::from_secs(120))?
            } else {
                wait_for_acceptance(&mut child)?;
                return Ok(());
            }
        }
        Err(LaunchFailure::NotRunning(reason)) if trial.is_some() =>
            update_client::supervisor::Outcome::Stopped(reason),
        Err(failure) => return Err(failure.message()),
    };
    if let update_client::supervisor::Outcome::Stopped(reason) = outcome {
        // Launch failure or supervision proved no child remains. Keep the
        // launch gate while taking desktop ownership again for recovery.
        let desktop = open_lock(&data.join("desktop-instance.lock"))?;
        if !try_lock(&desktop)? { return Err("cannot acquire desktop ownership for recovery".into()); }
        host.recover_stopped_trial(reason)?;
        drop(desktop);
        let mut stable = spawn_application(directory, &executable, None).map_err(LaunchFailure::message)?;
        wait_for_acceptance(&mut stable)?;
    }
    Ok(())
}

enum LaunchFailure {
    NotRunning(String),
    Uncertain(String),
}
impl LaunchFailure {
    fn message(self) -> String { match self { Self::NotRunning(s) | Self::Uncertain(s) => s } }
}
struct Started { process: Child, directory: PathBuf }
fn spawn_application(directory: &Path, executable: &Path, trial: Option<&str>) -> Result<Started, LaunchFailure> {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let directory = directory.join(format!("launch-{nonce}"));
    fs::create_dir(&directory).map_err(|e| LaunchFailure::NotRunning(error(e)))?;
    let mut child = Command::new(executable).creation_flags(0x08000000).arg("--bokheim-supervised-launch").arg(&directory)
        .spawn().map_err(|e| LaunchFailure::NotRunning(error(e)))?;
    if let Err(error) = activation::write_json(&directory.join(format!("permit-{}.json", child.id())), &Permit { trial: trial.map(str::to_owned) }) {
        update_client::supervisor::stop_process(&mut child).map_err(LaunchFailure::Uncertain)?;
        return Err(LaunchFailure::NotRunning(error));
    }
    Ok(Started { process: child, directory })
}
fn wait_for_acceptance(child: &mut Started) -> Result<(), String> {
    let path = child.directory.join(format!("accepted-{}.json", child.process.id()));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if path.exists() { return Ok(()); }
        if let Some(status) = child.process.try_wait().map_err(error)? { return Err(format!("application startup exited: {status}")); }
        if Instant::now() >= deadline {
            update_client::supervisor::stop_process(&mut child.process)?;
            return Err("application did not acquire desktop ownership".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct ParentProcess(*mut std::ffi::c_void);
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}
impl ParentProcess {
    fn open(pid: u32) -> Result<Self, String> {
        // SYNCHRONIZE only; open before readiness to avoid PID reuse.
        let handle = unsafe { OpenProcess(0x00100000, 0, pid) };
        if handle.is_null() { Err(error(std::io::Error::last_os_error())) } else { Ok(Self(handle)) }
    }
    fn wait(&self, timeout: Duration) -> Result<(), String> {
        match unsafe { WaitForSingleObject(self.0, timeout.as_millis().min(u32::MAX as u128 - 1) as u32) } {
            0 => Ok(()),
            258 => Err("original application did not exit before update".into()),
            _ => Err(error(std::io::Error::last_os_error())),
        }
    }
}
impl Drop for ParentProcess { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_gate_is_exclusive_and_released_by_owner_exit() {
        let dir = tempfile::tempdir().unwrap();
        let a = gate(dir.path()).unwrap(); assert!(try_lock(&a).unwrap());
        let b = gate(dir.path()).unwrap(); assert!(!try_lock(&b).unwrap());
        drop(a); assert!(try_lock(&b).unwrap());
    }
    #[test]
    fn failed_creation_proves_no_trial_process_exists() {
        let dir = tempfile::tempdir().unwrap();
        let result = spawn_application(dir.path(), &dir.path().join("missing.exe"), Some("plan"));
        assert!(matches!(result, Err(LaunchFailure::NotRunning(_))));
    }
    #[test]
    fn current_process_handle_is_not_signalled_while_running() {
        let process = ParentProcess::open(std::process::id()).unwrap();
        assert!(process.wait(Duration::from_millis(1)).unwrap_err().contains("did not exit"));
    }
}
