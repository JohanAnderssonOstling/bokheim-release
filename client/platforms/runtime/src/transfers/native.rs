#[cfg(not(target_os = "android"))]
use super::NetworkCost;
use super::POLICY;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "android")]
use std::sync::{Mutex, PoisonError};
#[cfg(not(target_os = "android"))]
use std::sync::{Mutex, Once, PoisonError};
#[cfg(not(target_os = "android"))]
use std::time::Duration;

static FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
#[cfg(not(target_os = "android"))]
static START: Once = Once::new();

/// The registry owns the preference; this small atomic mirror lets the browser
/// helper and native update workers observe the same setting across processes.
pub fn initialize(directory: &Path, allow: bool) -> io::Result<()> {
    *FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(directory.join("transfer-policy.json"));
    save_allow_metered(allow)?;
    start();
    Ok(())
}

/// Devices without an OS network-cost service update preferences in-process.
/// Do not start the desktop polling thread on these hosts.
pub fn initialize_device(directory: &Path, allow: bool) -> io::Result<()> {
    *FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(directory.join("transfer-policy.json"));
    save_allow_metered(allow)?;
    POLICY.set_allow_seeding(false);
    POLICY.set_network_cost(super::NetworkCost::Unknown);
    Ok(())
}

#[cfg(all(test, not(target_os = "android")))]
mod device_tests {
    use super::*;

    #[test]
    fn device_preferences_update_without_starting_the_desktop_monitor() {
        let directory = tempfile::tempdir().unwrap();
        initialize_device(directory.path(), false).unwrap();
        assert!(!START.is_completed());
        assert_eq!(fs::read_to_string(directory.path().join("transfer-policy.json")).unwrap(), "false");
        save_allow_metered(true).unwrap();
        assert_eq!(fs::read_to_string(directory.path().join("transfer-policy.json")).unwrap(), "true");
        assert!(POLICY.allowed());
        assert!(!START.is_completed());
        assert!(!POLICY.seeding_allowed());
        POLICY.set_allow_seeding(true);
        *FILE.lock().unwrap() = None;
    }
}

pub fn configure_reader(path: PathBuf) {
    *FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(path);
    refresh();
    start();
}

pub fn save_allow_metered(allow: bool) -> io::Result<()> {
    let file = FILE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(path) = file.as_ref() {
        let mut staged = tempfile::NamedTempFile::new_in(path.parent().ok_or_else(|| io::Error::other("missing policy directory"))?)?;
        staged.write_all(if allow { b"true" } else { b"false" })?;
        staged.as_file().sync_all()?;
        staged.persist(path).map_err(|error| error.error)?;
    }
    POLICY.set_allow_metered(allow);
    Ok(())
}

fn refresh() {
    if let Some(path) = FILE.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        if let Ok(value) = fs::read_to_string(path) {
            match value.trim() {
                "true" => POLICY.set_allow_metered(true),
                "false" => POLICY.set_allow_metered(false),
                _ => {}
            }
        }
    }
}

// Android publishes an initial observation and subsequent network callbacks
// before opening the backend. Preferences update POLICY directly in-process.
#[cfg(target_os = "android")]
fn start() {}

#[cfg(not(target_os = "android"))]
fn start() {
    START.call_once(|| {
        // Wait for the first OS observation instead of starting a transfer
        // during the monitor's startup race. Unknown after probing is allowed.
        POLICY.cost.store(3, std::sync::atomic::Ordering::Release);
        #[cfg(target_os = "macos")]
        macos::start();
        if let Err(error) = std::thread::Builder::new().name("network-cost".into()).spawn(|| loop {
            refresh();
            #[cfg(not(target_os = "macos"))]
            POLICY.set_network_cost(platform_cost());
            std::thread::sleep(Duration::from_secs(1));
        }) {
            log::warn!("could not start network cost monitor: {error}");
            POLICY.set_network_cost(NetworkCost::Unknown);
        }
    });
}

#[cfg(target_os = "linux")]
fn platform_cost() -> NetworkCost {
    use zbus::blocking::{connection::Builder, Proxy};
    let cost = (|| {
        let connection = Builder::system().ok()?.method_timeout(Duration::from_secs(1)).build().ok()?;
        let proxy = Proxy::new(&connection, "org.freedesktop.NetworkManager", "/org/freedesktop/NetworkManager", "org.freedesktop.NetworkManager").ok()?;
        proxy.get_property::<u32>("Metered").ok()
    })();
    match cost {
        Some(1 | 3) => NetworkCost::Metered,
        Some(2 | 4) => NetworkCost::Unmetered,
        _ => NetworkCost::Unknown,
    }
}

#[cfg(target_os = "windows")]
fn platform_cost() -> NetworkCost {
    use windows::Win32::Networking::NetworkListManager::{
        INetworkCostManager, NetworkListManager, NLM_CONNECTION_COST_FIXED, NLM_CONNECTION_COST_OVERDATALIMIT, NLM_CONNECTION_COST_ROAMING, NLM_CONNECTION_COST_UNRESTRICTED, NLM_CONNECTION_COST_VARIABLE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
    // This runs on our dedicated monitor thread, outside the UI's apartment.
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return NetworkCost::Unknown;
        }
        let cost = CoCreateInstance::<_, INetworkCostManager>(&NetworkListManager, None, CLSCTX_ALL).and_then(|manager| {
            let mut value = 0;
            manager.GetCost(&mut value, std::ptr::null())?;
            Ok(value)
        });
        CoUninitialize();
        match cost {
            Ok(value) if value & (NLM_CONNECTION_COST_FIXED.0 | NLM_CONNECTION_COST_VARIABLE.0 | NLM_CONNECTION_COST_ROAMING.0 | NLM_CONNECTION_COST_OVERDATALIMIT.0) as u32 != 0 => NetworkCost::Metered,
            Ok(value) if value & NLM_CONNECTION_COST_UNRESTRICTED.0 as u32 != 0 => NetworkCost::Unmetered,
            _ => NetworkCost::Unknown,
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos", target_os = "android")))]
fn platform_cost() -> NetworkCost {
    NetworkCost::Unknown
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use block2::{Block, RcBlock};
    use dispatch2::DispatchQueue;
    use std::ffi::c_void;

    #[link(name = "Network", kind = "framework")]
    extern "C" {
        fn nw_path_monitor_create() -> *mut c_void;
        fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: &DispatchQueue);
        fn nw_path_monitor_set_update_handler(monitor: *mut c_void, handler: &Block<dyn Fn(*mut c_void)>);
        fn nw_path_monitor_start(monitor: *mut c_void);
        fn nw_path_is_expensive(path: *mut c_void) -> bool;
        fn nw_path_is_constrained(path: *mut c_void) -> bool;
    }

    pub(super) fn start() {
        let queue = DispatchQueue::new("se.bokheim.network-cost", None);
        let handler = RcBlock::new(|path: *mut c_void| {
            // Network.framework lends a valid path during this callback.
            let metered = unsafe { nw_path_is_expensive(path) || nw_path_is_constrained(path) };
            POLICY.set_network_cost(if metered { NetworkCost::Metered } else { NetworkCost::Unmetered });
        });
        // The process owns one monitor for its lifetime; Network.framework
        // retains the queue and copies the handler when registering them.
        unsafe {
            let monitor = nw_path_monitor_create();
            if monitor.is_null() {
                return;
            }
            nw_path_monitor_set_queue(monitor, &queue);
            nw_path_monitor_set_update_handler(monitor, &handler);
            nw_path_monitor_start(monitor);
        }
    }
}
