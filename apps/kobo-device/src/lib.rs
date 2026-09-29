//! Kobo hardware operations that are independent of any UI framework.

pub mod battery;
#[cfg(target_os = "linux")]
pub mod bluetooth;
pub mod brightness;
pub mod natural_light;
#[cfg(target_os = "linux")]
pub mod wifi;

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const POWER_STATE: &str = "/sys/power/state";
const POWER_STATE_EXTENDED: &str = "/sys/power/state-extended";
const LEGACY_TOUCH_COMMAND: &str = "/sys/devices/virtual/input/input1/neocmd";
const INPUT_CLASS: &str = "/sys/class/input";
const PAGE_BUTTON_CODES: [usize; 2] = [193, 194];
const TOUCH_DEACTIVATION_DELAY: Duration = Duration::from_secs(2);

/// Suspends the Kobo to RAM and performs the hardware recovery required after
/// the kernel returns. This follows Plato's Kobo suspend/resume ordering.
pub fn suspend() -> io::Result<()> {
    let total_started = Instant::now();
    eprintln!("KOBO_SUSPEND phase=requested elapsed_ms=0");

    #[cfg(target_os = "linux")]
    let _wifi_sleep = wifi::prepare_sleep().map_err(|error| io::Error::other(format!("Cannot power Wi-Fi off before sleep: {error}")))?;
    eprintln!("KOBO_SUSPEND phase=wifi_off elapsed_ms={}", total_started.elapsed().as_millis());

    let wake_sources_started = Instant::now();
    if let Err(error) = enable_page_button_wake_sources() {
        eprintln!("Could not configure Kobo page-button wake sources: {error}");
    }
    eprintln!("KOBO_SUSPEND phase=wake_sources_configured elapsed_ms={} phase_ms={}", total_started.elapsed().as_millis(), wake_sources_started.elapsed().as_millis());

    let touch_started = Instant::now();
    write_value(POWER_STATE_EXTENDED, b"1").map_err(|error| power_error("disable touch", POWER_STATE_EXTENDED, error))?;

    // Kobo kernels need time to deactivate touch before entering suspend.
    thread::sleep(TOUCH_DEACTIVATION_DELAY);
    eprintln!("KOBO_SUSPEND phase=touch_deactivated elapsed_ms={} phase_ms={}", total_started.elapsed().as_millis(), touch_started.elapsed().as_millis());

    let sync_started = Instant::now();
    sync_filesystems();
    eprintln!("KOBO_SUSPEND phase=filesystems_synced elapsed_ms={} phase_ms={}", total_started.elapsed().as_millis(), sync_started.elapsed().as_millis());

    let kernel_suspend_started = Instant::now();
    let suspend_result = write_value(POWER_STATE, b"mem").map_err(|error| power_error("suspend", POWER_STATE, error));
    match &suspend_result {
        Ok(()) => eprintln!("KOBO_SUSPEND phase=kernel_returned status=ok elapsed_ms={} suspended_ms={}", total_started.elapsed().as_millis(), kernel_suspend_started.elapsed().as_millis()),
        Err(error) => eprintln!("KOBO_SUSPEND phase=kernel_returned status=error elapsed_ms={} suspended_ms={} error={error}", total_started.elapsed().as_millis(), kernel_suspend_started.elapsed().as_millis()),
    }

    let resume_started = Instant::now();
    let resume_result = resume_hardware();
    match &resume_result {
        Ok(()) => eprintln!("KOBO_SUSPEND phase=resume_complete status=ok elapsed_ms={} phase_ms={}", total_started.elapsed().as_millis(), resume_started.elapsed().as_millis()),
        Err(error) => eprintln!("KOBO_SUSPEND phase=resume_complete status=error elapsed_ms={} phase_ms={} error={error}", total_started.elapsed().as_millis(), resume_started.elapsed().as_millis()),
    }
    suspend_result.and(resume_result)
}

/// Enables kernel wakeup for evdev devices that expose Kobo's page-turn keys.
/// Unsupported kernels are left unchanged and can still use their default
/// wake-source configuration.
pub fn enable_page_button_wake_sources() -> io::Result<usize> {
    let mut enabled = 0;
    for entry in fs::read_dir(INPUT_CLASS)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("event") {
            continue;
        }
        let device = entry.path().join("device");
        let capabilities = match fs::read_to_string(device.join("capabilities/key")) {
            Ok(capabilities) => capabilities,
            Err(_) => continue,
        };
        if !PAGE_BUTTON_CODES.iter().any(|code| key_capability_contains(&capabilities, *code)) {
            continue;
        }
        let canonical = fs::canonicalize(&device)?;
        if let Some(wakeup) = canonical.ancestors().map(|ancestor| ancestor.join("power/wakeup")).find(|candidate| candidate.is_file()) {
            write_value(wakeup, b"enabled")?;
            enabled += 1;
        }
    }
    Ok(enabled)
}

fn key_capability_contains(capabilities: &str, code: usize) -> bool {
    let words = capabilities.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return false;
    }
    let word_width = usize::BITS as usize;
    let word_index = code / word_width;
    let bit_index = code % word_width;
    words.iter().rev().nth(word_index).and_then(|word| u128::from_str_radix(word, 16).ok()).is_some_and(|word| word & (1_u128 << bit_index) != 0)
}

fn resume_hardware() -> io::Result<()> {
    write_value(POWER_STATE_EXTENDED, b"0").map_err(|error| power_error("reactivate touch", POWER_STATE_EXTENDED, error))?;
    if matches!(std::env::var("PRODUCT").as_deref(), Ok("alyssum" | "dahlia")) {
        write_value(LEGACY_TOUCH_COMMAND, b"a")?;
    }
    Ok(())
}

fn sync_filesystems() {
    // Avoid spawning BusyBox from a static musl process on Kobo.
    unsafe { libc::sync() };
}

fn power_error(operation: &str, path: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{operation} via {path}: {error}"))
}

fn write_value(path: impl AsRef<Path>, value: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.write_all(value)
}
