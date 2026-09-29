//! Windows desktop binding. Live files change only in the external startup helper.
use browser_ui::{UpdateAction, UpdateControls, UpdateView};
use gpui::{App, AppContext, Entity};
use linux_update_host::windows::{WindowsHost, WindowsStartup};
use std::{path::Path, sync::{Arc, Mutex, OnceLock}, time::Duration};
pub use linux_update_host::windows_process::{enter, Entry};
static HOST: OnceLock<Arc<Mutex<WindowsHost>>> = OnceLock::new();

fn config() -> Result<update_client::DiscoveryConfig, String> {
    let mut config = update_client::DiscoveryConfig::bundled()?;
    if let Ok(endpoint) = std::env::var("BOKHEIM_UPDATE_ENDPOINT") { config.endpoint = endpoint; }
    Ok(config)
}

pub fn helper_entry(data: &Path) -> Result<bool, String> {
    linux_update_host::windows_process::helper_entry(data, env!("CARGO_PKG_VERSION"), config)
}

/// False means the copied helper is ready and this process must exit.
pub fn initialize(data: &Path, trial: Option<&str>) -> Result<bool, String> {
    let config = match config() {
        Ok(config) => config,
        Err(error) => {
            if data.join("updates/activation.json").exists() || data.join("updates/active-taxonomy.json").exists() { return Err(error); }
            return Ok(true);
        }
    };
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let installation = executable.parent().ok_or("executable has no directory")?;
    let mut host = WindowsHost::open(data, installation, env!("CARGO_PKG_VERSION"), config)?;
    if matches!(host.startup(trial)?, WindowsStartup::Helper) {
        match linux_update_host::windows_process::spawn_helper(data, installation) {
            Ok(()) => return Ok(false),
            Err(error) => {
                let required = std::fs::metadata(&executable).map_err(|e| e.to_string())?.len();
                let available = fs2::available_space(data).map_err(|e| e.to_string())?;
                let reason = if required > available {
                    update_client::coordinator::WaitReason::Space { additional_bytes: required - available }
                } else { update_client::coordinator::WaitReason::Retry };
                host.helper_unavailable(reason, error)?;
                if !matches!(host.startup(None)?, WindowsStartup::Run) { return Err("update recovery requires its helper".into()); }
            }
        }
    }
    HOST.set(Arc::new(Mutex::new(host))).map_err(|_| "Windows update host already initialized")?;
    Ok(true)
}

pub fn trial_pending() -> bool {
    HOST.get().is_some_and(|h| h.lock().map(|h| h.is_trial()).unwrap_or(false))
}
pub fn healthy() -> Result<(), String> {
    if let Some(host) = HOST.get() { host.lock().map_err(|_| "update host lock poisoned")?.healthy()?; }
    Ok(())
}

pub fn bind(cx: &mut App) -> Option<Entity<UpdateControls>> {
    let host = HOST.get()?.clone();
    let restart = {
        let h = host.lock().ok()?;
        h.installation.join("Bokheim.exe")
    };
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
            .dispatch(move || loop {
                let action = match requests.recv_timeout(Duration::from_secs(1)) {
                    Ok(action) => Some(action),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                    Err(_) => break,
                };
                let result = (|| {
                    let mut host = host.lock().map_err(|_| "update host lock poisoned".to_owned())?;
                    if action == Some(UpdateAction::Update) {
                        host.approve()?;
                    }
                    host.tick()
                })();
                match result {
                    Ok(view) => {
                        if views.send_blocking(view).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        log::warn!("update work will retry: {error}");
                        std::thread::sleep(Duration::from_secs(30));
                    }
                }
            })
            .recv()
            .await;
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(view) = received.recv().await {
            if weak.update(cx, |controls, cx| controls.apply(view, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    Some(controls)
}
