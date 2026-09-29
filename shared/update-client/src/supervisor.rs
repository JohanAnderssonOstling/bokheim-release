//! A trial process must be known to have exited before callers restore its files.
use crate::activation::{Journal, State};
use std::{path::Path, process::{Child, ExitStatus}, time::{Duration, Instant}};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed,
    /// The process has exited/reaped. Recovery must still consult the journal:
    /// a commit racing the timeout must never be rolled back.
    Stopped(String),
}

pub fn stop_process(child: &mut Child) -> Result<(), String> {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        if let Err(error) = child.kill() {
            // A normal exit can race kill. Only proceed when wait proves it exited.
            if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                return Err(format!("cannot stop trial process: {error}"));
            }
        }
    }
    child.wait().map_err(|e| format!("cannot reap trial process: {e}"))?;
    Ok(())
}

/// Wait for a candidate migration to exit. A timeout or observation failure
/// stops and reaps it before returning. If stopping itself fails, callers must
/// keep its private workspace isolated and must not promote its output.
pub fn wait_for_preparation(child: &mut Child, timeout: Duration) -> Result<ExitStatus, String> {
    let deadline = Instant::now().checked_add(timeout).ok_or("preparation timeout overflow")?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                stop_process(child)?;
                return Err(format!("cannot observe candidate migration: {error}"));
            }
        }
        if Instant::now() >= deadline {
            stop_process(child)?;
            return Err("candidate migration exceeded startup time limit".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn wait_for_trial(child: &mut Child, root: &Path, plan_id: &str, timeout: Duration) -> Result<Outcome, String> {
    let deadline = Instant::now().checked_add(timeout).ok_or("trial timeout overflow")?;
    loop {
        match Journal::load(root) {
            Ok(Some(journal)) if journal.plan_id == plan_id && journal.state == State::Committed => return Ok(Outcome::Committed),
            Ok(_) => {}
            Err(error) => {
                stop_process(child)?;
                return Ok(Outcome::Stopped(format!("cannot read trial acknowledgement: {error}")));
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Outcome::Stopped(format!("trial exited before acknowledgement: {status}"))),
            Ok(None) => {}
            Err(error) => {
                stop_process(child)?;
                return Ok(Outcome::Stopped(format!("cannot observe trial process: {error}")));
            }
        }
        if Instant::now() >= deadline {
            stop_process(child)?;
            return Ok(Outcome::Stopped("trial startup timed out".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::{Command, Stdio}};

    #[test]
    fn child_fixture() {
        let Ok(mode) = std::env::var("BOKHEIM_SUPERVISOR_FIXTURE") else { return };
        if mode == "exit" { std::process::exit(23); }
        if mode == "prepared" { return; }
        if mode == "commit" {
            let root = std::path::PathBuf::from(std::env::var_os("BOKHEIM_SUPERVISOR_ROOT").unwrap());
            Journal::load(&root).unwrap().unwrap().commit(&root).unwrap();
        }
        loop { std::thread::sleep(Duration::from_secs(60)); }
    }

    fn spawn(root: &Path, mode: &str) -> Child {
        Command::new(std::env::current_exe().unwrap()).args(["--exact", "supervisor::tests::child_fixture"])
            .env("BOKHEIM_SUPERVISOR_FIXTURE", mode).env("BOKHEIM_SUPERVISOR_ROOT", root)
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap()
    }

    #[test]
    fn healthy_trial_continues_after_supervision() {
        let temp = tempfile::tempdir().unwrap();
        Journal { plan_id: "approved".into(), previous_version: "1".into(), next_version: "2".into(),
            appimage: None, replacements: Vec::new(), state: State::Trial }.save(temp.path()).unwrap();
        let mut child = spawn(temp.path(), "commit");
        let outcome = wait_for_trial(&mut child, temp.path(), "approved", Duration::from_secs(10));
        // Always stop our fixture, even if the assertion fails.
        let running = child.try_wait().unwrap().is_none(); stop_process(&mut child).unwrap();
        assert_eq!(outcome.unwrap(), Outcome::Committed);
        assert!(running);
    }

    #[test]
    fn failed_hung_and_unreadable_trials_stop_before_recovery() {
        for mode in ["exit", "hang", "invalid-receipt"] {
            let temp = tempfile::tempdir().unwrap();
            if mode == "invalid-receipt" { fs::write(temp.path().join("activation.json"), b"invalid").unwrap(); }
            let mut child = spawn(temp.path(), mode);
            let outcome = wait_for_trial(&mut child, temp.path(), "approved", Duration::from_millis(250));
            let exited = child.try_wait().unwrap().is_some(); stop_process(&mut child).unwrap();
            assert!(matches!(outcome.unwrap(), Outcome::Stopped(_)), "{mode}");
            assert!(exited, "recovery cannot race a live {mode} process");
        }
    }

    #[test]
    fn preparation_success_failure_and_timeout_leave_no_running_child() {
        let temp = tempfile::tempdir().unwrap();
        for mode in ["prepared", "exit", "hang"] {
            let mut child = spawn(temp.path(), mode);
            let timeout = if mode == "hang" { Duration::from_millis(250) } else { Duration::from_secs(10) };
            let result = wait_for_preparation(&mut child, timeout);
            let exited = child.try_wait().unwrap().is_some();
            stop_process(&mut child).unwrap();
            assert!(exited, "candidate must exit before inspecting its output");
            match mode {
                "prepared" => assert!(result.unwrap().success()),
                "exit" => assert_eq!(result.unwrap().code(), Some(23)),
                _ => assert!(result.unwrap_err().contains("time limit")),
            }
        }
    }
}
