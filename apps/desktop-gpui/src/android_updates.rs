//! Android startup and GPUI binding. Downloads never invoke the package installer.
use browser_ui::{UpdateAction, UpdateControls, UpdateView};
use gpui::{App, AppContext, Entity};
use linux_update_host::android::AndroidHost;
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

type Bridge = dyn Fn(&str) -> Result<String, String> + Send + Sync;
static BRIDGE: OnceLock<Box<Bridge>> = OnceLock::new();
static HOST: OnceLock<Arc<Mutex<AndroidHost>>> = OnceLock::new();

fn command(value: serde_json::Value) -> Result<String, String> {
    BRIDGE.get().ok_or("Android update bridge is unavailable")?(&value.to_string())
}

pub fn initialize(data: &Path, bridge: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Result<bool, String> {
    BRIDGE.set(Box::new(bridge)).map_err(|_| "Android update bridge already initialized")?;
    let config = match update_client::DiscoveryConfig::bundled() {
        Ok(config) => config,
        Err(error) => {
            if data.join("updates/activation.json").exists() || data.join("updates/active-taxonomy.json").exists() {
                return Err(error);
            }
            return Ok(false);
        }
    };
    let mut host = AndroidHost::open(data, env!("CARGO_PKG_VERSION"), config, &format!("android-{}-apk", std::env::consts::ARCH))?;
    let trial = host.startup(|path| command(serde_json::json!({ "action": "prepare", "path": path })).map(|_| ()))?;
    HOST.set(Arc::new(Mutex::new(host))).map_err(|_| "Android update host already initialized")?;
    Ok(trial)
}

pub fn healthy() -> Result<(), String> {
    if let Some(host) = HOST.get() {
        host.lock().map_err(|_| "Android update host lock poisoned")?.healthy()?;
    }
    Ok(())
}

pub fn bind(cx: &mut App) -> Option<Entity<UpdateControls>> {
    let host = HOST.get()?.clone();
    let (actions, requests) = std::sync::mpsc::channel();
    let (views, received) = async_channel::unbounded();
    let controls = cx.new(|_| {
        UpdateControls::new(
            UpdateView::Hidden,
            std::rc::Rc::new(move |action, _| {
                let _ = actions.send(action);
            }),
        )
    });
    let weak = controls.downgrade();
    let worker = crate::blocking_worker::BlockingWorker::new(cx.background_executor(), 1);
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
                        let mut host = host.lock().map_err(|_| "Android update host lock poisoned".to_string())?;
                        match action {
                            Some(UpdateAction::Update) => {
                                host.approve()?;
                                // Show approval before the blocking APK download starts.
                                views.try_send(Ok(host.view())).map_err(|_| "update UI closed".to_owned())?;
                            }
                            Some(UpdateAction::Install) => {
                                let mut request = serde_json::to_value(host.install_request()?).map_err(|e| e.to_string())?;
                                request["action"] = "install".into();
                                if command(request)? == "invalidPackage" {
                                    host.reject_package()?;
                                }
                            }
                            Some(UpdateAction::GrantPermission) => {
                                command(serde_json::json!({"action":"grantPermission"}))?;
                            }
                            Some(UpdateAction::Restart) => {
                                command(serde_json::json!({"action":"restart"}))?;
                            }
                            None => {}
                        }
                        let mut view = host.tick()?;
                        if view == (UpdateView::Ready { action: UpdateAction::Install }) && command(serde_json::json!({"action":"permission"}))? != "true" {
                            view = UpdateView::NeedsPermission;
                        }
                        Ok::<_, String>(view)
                    })();
                    match result {
                        Ok(view) => {
                            retry_delay = Duration::from_secs(1);
                            if views.send_blocking(Ok(view)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            log::warn!("Android update work will retry: {error}");
                            if views.try_send(Err(())).is_err() {
                                break;
                            }
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
