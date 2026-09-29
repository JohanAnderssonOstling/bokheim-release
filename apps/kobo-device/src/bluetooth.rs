//! Bluetooth adapter control through the Kobo firmware's BlueZ service.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND: &str = "bluetoothctl";
const TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const INFO_TIMEOUT: Duration = Duration::from_secs(1);
const SCAN_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub available: bool,
    pub powered: bool,
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: String,
    pub connected: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Status,
    Power(bool),
    Scan,
    Connect(String),
}

pub fn request(request: Request) -> io::Result<Status> {
    match request {
        Request::Status => status(),
        Request::Power(enabled) => {
            if enabled {
                ensure_controller()?;
            }
            let _ = run(&["power", if enabled { "on" } else { "off" }])?;
            status()
        }
        Request::Scan => scan(),
        Request::Connect(address) => {
            ensure_controller()?;
            let current = status()?;
            if !current.powered {
                let _ = run(&["power", "on"])?;
            }
            let _ = run_with_timeout(&["connect", &address], CONNECT_TIMEOUT)?;
            status()
        }
    }
}

fn status() -> io::Result<Status> {
    // Some Kobo BlueZ versions intermittently return a non-zero status for
    // `show` even though the controller is present. `list` is a reliable
    // controller-presence probe, so use it as a fallback instead of turning a
    // transient bluetoothctl failure into "Bluetooth unavailable".
    let (powered, available) = match run(&["show"]) {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output);
            (text.lines().any(|line| line.trim() == "Powered: yes"), text.lines().any(|line| line.starts_with("Controller ")))
        }
        Err(show_error) => {
            let controllers = run(&["list"]);
            match controllers {
                Ok(output) => {
                    let text = String::from_utf8_lossy(&output);
                    let Some(controller) = text.lines().find_map(controller_address) else {
                        return Ok(Status { available: hardware_present(), powered: false, devices: Vec::new() });
                    };
                    match run(&["show", &controller]) {
                        Ok(output) => {
                            let text = String::from_utf8_lossy(&output);
                            (text.lines().any(|line| line.trim() == "Powered: yes"), true)
                        }
                        Err(_) => (false, true),
                    }
                }
                Err(_) if hardware_present() => {
                    return Ok(Status { available: true, powered: false, devices: Vec::new() });
                }
                Err(_) => return Err(show_error),
            }
        }
    };
    if !available {
        return Ok(Status { available: hardware_present(), powered: false, devices: Vec::new() });
    }
    // BlueZ keeps known/paired devices in its controller cache. Read that
    // cache even while the adapter is powered off so previously connected
    // devices remain remembered and visible after an app restart.
    let devices = list_devices()?;
    Ok(Status { available: true, powered, devices })
}

fn hardware_present() -> bool {
    cfg!(target_arch = "arm") && std::path::Path::new("/sbin/rtk_hciattach").is_file() && std::path::Path::new("/dev/ttymxc1").exists() && std::path::Path::new("/sys/module/8723ds").exists()
}

fn ensure_controller() -> io::Result<()> {
    if std::path::Path::new("/sys/class/bluetooth/hci0").exists() {
        return Ok(());
    }

    // Libra 2 shares the RTL8723DS radio between Wi-Fi and Bluetooth. When
    // Nickel was not the process that enabled Bluetooth, the UART side is
    // powered down even though bluetoothd/bluealsa are still running. Reuse
    // the Kobo radio helper to cycle the shared module before attaching H5.
    if let Some(helper) = std::env::var_os("BOKHEIM_KOBO_WIFI_HELPER") {
        let interface = std::env::var_os("INTERFACE").ok_or_else(|| io::Error::other("Wi-Fi interface is missing while initializing Bluetooth"))?;
        let module = std::env::var_os("WIFI_MODULE").ok_or_else(|| io::Error::other("Wi-Fi driver is missing while initializing Bluetooth"))?;
        run_external(Command::new("/bin/sh").env("INTERFACE", &interface).env("WIFI_MODULE", &module).arg(&helper).arg("off"), Duration::from_secs(15), "power-cycle Wi-Fi/Bluetooth radio")?;
        run_external(Command::new("/bin/sh").env("INTERFACE", &interface).env("WIFI_MODULE", &module).arg(&helper).arg("on"), Duration::from_secs(15), "restore Wi-Fi/Bluetooth radio")?;
    }

    let mut attach = Command::new("/sbin/rtk_hciattach")
        .args(["-n", "-s", "115200", "/dev/ttymxc1", "rtk_h5"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| io::Error::new(error.kind(), format!("Could not initialize Bluetooth controller: {error}")))?;

    for _ in 0..40 {
        if std::path::Path::new("/sys/class/bluetooth/hci0").exists() {
            let _ = run_external(Command::new("/bin/hciconfig").arg("hci0").arg("up"), Duration::from_secs(3), "bring Bluetooth controller up");
            return Ok(());
        }
        if attach.try_wait()?.is_some() {
            return Err(io::Error::other("Bluetooth controller initialization failed; see rtk_hciattach"));
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = attach.kill();
    let _ = attach.wait();
    Err(io::Error::new(io::ErrorKind::TimedOut, "Bluetooth controller initialization timed out"))
}

fn scan() -> io::Result<Status> {
    let current = status()?;
    if !current.available {
        return Ok(current);
    }
    if !current.powered {
        return Ok(current);
    }

    // bluetoothctl keeps the scan command open while discovery is active.
    // Its own timeout lets the command finish cleanly, after which the
    // adapter's device cache can be read with `devices`.
    let _ = run_with_timeout(&["--timeout", "8", "scan", "on"], SCAN_TIMEOUT)?;
    status()
}

fn list_devices() -> io::Result<Vec<Device>> {
    let output = run(&["devices"])?;
    let mut devices = parse_devices(&String::from_utf8_lossy(&output));
    for device in &mut devices {
        // This firmware's bluetoothctl does not support the newer
        // `devices Connected` filter and returns "Too many arguments".
        // Query each known device through the BlueZ Device API exposed by
        // `info`, which also works for devices paired before Bokheim started.
        device.connected = run_with_timeout(&["info", &device.address], INFO_TIMEOUT).ok().map(|output| String::from_utf8_lossy(&output).lines().any(|line| line.trim() == "Connected: yes")).unwrap_or(false);
    }
    devices.sort_by(|left, right| right.connected.cmp(&left.connected).then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase())).then_with(|| left.address.cmp(&right.address)));
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::parse_devices;

    #[test]
    fn parses_device_listing_with_spaces_in_names() {
        assert_eq!(
            parse_devices("Device AA:BB:CC:DD:EE:FF Headphones\nDevice 11:22:33:44:55:66 Kitchen Speaker\nnot a device"),
            vec![super::Device { address: "AA:BB:CC:DD:EE:FF".into(), name: "Headphones".into(), connected: false }, super::Device { address: "11:22:33:44:55:66".into(), name: "Kitchen Speaker".into(), connected: false },]
        );
    }

    #[test]
    fn uses_fallback_for_missing_device_name() {
        assert!(parse_devices("Device AA:BB:CC:DD:EE:FF").is_empty());
    }

    #[test]
    fn never_uses_an_address_as_a_device_name() {
        assert!(parse_devices("Device AA:BB:CC:DD:EE:FF 11:22:33:44:55:66").is_empty());
    }

    #[test]
    fn ignores_scan_prefixes_and_duplicate_addresses() {
        assert_eq!(
            parse_devices("[NEW] Device AA:BB:CC:DD:EE:FF Headphones\nDevice AA:BB:CC:DD:EE:FF Headphones\nDevice 11:22:33:44:55:66"),
            vec![super::Device { address: "AA:BB:CC:DD:EE:FF".into(), name: "Headphones".into(), connected: false }]
        );
    }
}

fn parse_devices(text: &str) -> Vec<Device> {
    let mut devices = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(device_marker) = fields.position(|field| field == "Device") else { continue };
        let Some(address) = fields.next() else { continue };
        if device_marker > 0 && device_marker != 1 {
            continue;
        }
        if !is_device_address(address) {
            continue;
        }
        let name = fields.collect::<Vec<_>>().join(" ");
        if name.is_empty() || is_device_address(&name) || name.eq_ignore_ascii_case("unknown device") {
            continue;
        }
        if devices.iter().any(|device: &Device| device.address.eq_ignore_ascii_case(address)) {
            continue;
        }
        devices.push(Device { address: address.to_owned(), name, connected: false });
    }
    devices
}

fn is_device_address(value: &str) -> bool {
    value.len() == 17 && value.split(':').count() == 6 && value.split(':').all(|part| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn controller_address(line: &str) -> Option<String> {
    let mut fields = line.split_whitespace();
    (fields.next() == Some("Controller")).then(|| fields.next().filter(|address| is_device_address(address)).map(str::to_owned)).flatten()
}

fn run(arguments: &[&str]) -> io::Result<Vec<u8>> {
    run_with_timeout(arguments, TIMEOUT)
}

fn run_external(command: &mut Command, timeout: Duration, operation: &str) -> io::Result<()> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(|error| io::Error::new(error.kind(), format!("Could not start {operation}: {error}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(io::Error::other(format!("{operation} failed ({status})")));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{operation} timed out")));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn run_with_timeout(arguments: &[&str], timeout: Duration) -> io::Result<Vec<u8>> {
    let mut child = Command::new(COMMAND).args(arguments).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|error| io::Error::new(error.kind(), format!("Bluetooth control unavailable: {error}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child.stdout.take().map(|mut pipe| pipe.read_to_end(&mut stdout));
            child.stderr.take().map(|mut pipe| pipe.read_to_end(&mut stderr));
            if child.wait()?.success() {
                return Ok(stdout);
            }
            // bluetoothctl commonly writes connection failures to stdout on
            // Kobo, while other BlueZ builds use stderr. Keep whichever
            // stream contains the diagnostic instead of reducing it to the
            // generic command name.
            let detail = {
                let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
                if stderr.is_empty() { String::from_utf8_lossy(&stdout).trim().to_owned() } else { stderr }
            };
            return Err(io::Error::other(if detail.is_empty() { format!("Bluetooth command failed: bluetoothctl {}", arguments.join(" ")) } else { detail }));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Bluetooth command timed out"));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
