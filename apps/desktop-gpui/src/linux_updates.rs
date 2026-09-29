//! GPUI binding for the Linux startup/update host. Never activates from a UI tick.
use browser_ui::{UpdateAction, UpdateControls, UpdateView};
use gpui::{App, AppContext, Entity};
use linux_update_host::Host;
pub use linux_update_host::Startup;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
static HOST: OnceLock<Arc<Mutex<Host>>> = OnceLock::new();

pub use crate::update_entry::candidate_entry;

pub fn initialize(data: &Path) -> Result<Startup, String> {
    let config = match update_client::DiscoveryConfig::bundled() {
        Ok(mut config) => {
            if let Ok(endpoint) = std::env::var("BOKHEIM_UPDATE_ENDPOINT") {
                config.endpoint = endpoint;
            }
            config
        }
        Err(error) => {
            if data.join("updates/activation.json").exists() || data.join("updates/active-taxonomy.json").exists() {
                return Err(error);
            }
            return Ok(Startup::Run);
        }
    };
    let appimage = std::env::var_os("APPIMAGE").filter(|_| std::env::var_os("BOKHEIM_DISABLE_SELF_UPDATE").is_none()).map(PathBuf::from).map(|p| p.canonicalize().map_err(|e| e.to_string())).transpose()?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut host = Host::open(data, env!("CARGO_PKG_VERSION"), config, appimage, executable)?;
    let args = std::env::args().collect::<Vec<_>>();
    let trial = args.windows(2).find(|a| a[0] == "--bokheim-update-trial").map(|a| a[1].as_str());
    let startup = host.startup(trial)?;
    if matches!(startup, Startup::Run) {
        HOST.set(Arc::new(Mutex::new(host))).map_err(|_| "Linux update host already initialized")?;
    }
    Ok(startup)
}

pub fn trial_pending() -> bool {
    HOST.get().is_some_and(|h| h.lock().map(|h| h.is_trial()).unwrap_or(false))
}

pub fn healthy() -> Result<(), String> {
    if let Some(host) = HOST.get() {
        host.lock().map_err(|_| "update host lock poisoned")?.healthy()?;
    }
    Ok(())
}

/// Parent waits only during a new launch, while no previous user session exists.
/// Failure returns to the old process, whose next startup recovers the journal.
pub fn launch(data: &Path, executable: &Path, trial: Option<&str>) -> Result<bool, String> {
    let mut command = std::process::Command::new(executable);
    command.env_remove("APPIMAGE").env_remove("APPDIR");
    if let Some(id) = trial {
        command.arg("--bokheim-update-trial").arg(id);
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(error) => {
            log::warn!("could not launch update: {error}");
            return Ok(false);
        }
    };
    let Some(id) = trial else { return Ok(true) };
    match update_client::supervisor::wait_for_trial(&mut child, &data.join("updates"), id, Duration::from_secs(120))? {
        update_client::supervisor::Outcome::Committed => Ok(true),
        update_client::supervisor::Outcome::Stopped(reason) => {
            log::warn!("startup trial stopped; checking recovery journal: {reason}");
            Ok(false)
        }
    }
}

pub fn bind(cx: &mut App) -> Option<Entity<UpdateControls>> {
    let host = HOST.get()?.clone();
    let restart = {
        let h = host.lock().ok()?;
        h.executable.clone()
    };
    // APPIMAGE points to the persistent package, current_exe to its temporary mount.
    let restart = std::env::var_os("APPIMAGE").map(PathBuf::from).unwrap_or(restart);
    let (actions, requests) = std::sync::mpsc::channel();
    let (views, received) = async_channel::unbounded();
    let controls = cx.new(|_| {
        UpdateControls::new(
            UpdateView::Hidden,
            std::rc::Rc::new(move |action, cx| {
                if action == UpdateAction::Restart {
                    cx.set_restart_path(restart.clone());
                    cx.restart();
                } else {
                    let _ = actions.send(action);
                }
            }),
        )
    });
    let weak = controls.downgrade();
    let worker = crate::blocking_worker::BlockingWorker::new(cx.background_executor(), 1);
    // Scheduling this after the startup callback lets healthy() commit before polling.
    cx.spawn(async move |_| {
        let _ = worker
            .dispatch(move || {
                let mut retry_delay = Duration::from_secs(1);
                loop {
                    let action = match requests.recv_timeout(retry_delay) {
                        Ok(action) => Some(action),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                        Err(_) => break,
                    };
                    let result = (|| {
                        let mut host = host.lock().map_err(|_| "update host lock poisoned".to_owned())?;
                        if action == Some(UpdateAction::Update) {
                            host.approve()?;
                            // Publish the committed approval before tick downloads.
                            views.try_send(Ok(host.view())).map_err(|_| "update UI closed".to_owned())?;
                        }
                        host.tick()
                    })();
                    match result {
                        Ok(view) => {
                            retry_delay = Duration::from_secs(1);
                            if views.send_blocking(Ok(view)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            log::warn!("update work will retry: {error}");
                            if views.try_send(Err(())).is_err() {
                                break;
                            }
                            // A new button press can wake the worker during backoff.
                            retry_delay = Duration::from_secs(30);
                        }
                    }
                }
            })
            .recv()
            .await;
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(view) = received.recv().await {
            if weak
                .update(cx, |controls, cx| match view {
                    Ok(view) => controls.apply(view, cx),
                    Err(()) => controls.work_failed(cx),
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
    Some(controls)
}
