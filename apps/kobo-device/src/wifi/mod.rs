//! Serialized Kobo Wi-Fi operations. Hardware and supplicant work never runs on GPUI.
mod control;
use control::{Control, Result, decode_ssid, fields, hex};
use std::fs;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    Open,
    WpaPersonal,
    Unsupported,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Network {
    pub ssid: Vec<u8>,
    pub signal: i32,
    pub security: Security,
}
impl Network {
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.ssid).into_owned()
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedNetwork {
    pub id: u32,
    pub name: String,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Phase {
    #[default]
    Off,
    Disconnected,
    Connecting,
    ObtainingAddress,
    Connected,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Snapshot {
    pub phase: Phase,
    pub network: Option<String>,
    pub networks: Vec<Network>,
    pub saved: Vec<SavedNetwork>,
    pub scanning: bool,
    pub power_available: bool,
    pub error: Option<String>,
}
// Deliberately no Debug: requests may contain credentials.
pub enum Request {
    Status,
    Scan,
    Power(bool),
    ConnectSaved(u32),
    Join { network: Network, password: String },
    Disconnect,
}
enum Job {
    Request(Request, async_channel::Sender<Result<Snapshot>>),
    PrepareSleep { reply: mpsc::SyncSender<Result<()>>, deadline: Instant },
    FinishSleep,
    UiReady,
}
static WORKER: OnceLock<std::result::Result<mpsc::Sender<Job>, String>> = OnceLock::new();

fn worker() -> Result<&'static mpsc::Sender<Job>> {
    WORKER
        .get_or_init(|| {
            let (send, receive) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("kobo-wifi".into())
                .spawn(move || {
                    let mut controller = Controller::new();
                    loop {
                        match receive.recv_timeout(Duration::from_secs(2)) {
                            Ok(Job::Request(request, reply)) => {
                                let result = controller.execute(request);
                                let _ = reply.send_blocking(result);
                            }
                            Ok(Job::PrepareSleep { reply, deadline }) => {
                                let result =
                                    if Instant::now() >= deadline { Err("Wi-Fi sleep preparation expired before it could start".into()) } else { controller.prepare_sleep_with(|controller| controller.power_before(false, deadline)) };
                                // Rendezvous with the caller: an expired request cannot leave
                                // a late sleep guard holding the radio off after UI recovery.
                                if reply.send(result).is_err() {
                                    controller.sleeping = false;
                                    controller.resume_after_ui_with(|controller| controller.power(true));
                                }
                            }
                            Ok(Job::FinishSleep) => controller.sleeping = false,
                            Ok(Job::UiReady) => controller.resume_after_ui_with(|controller| controller.power(true)),
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if !controller.sleeping && (controller.attempt.is_some() || controller.scan_started.is_some() || controller.address_requested) {
                                    if let Err(error) = controller.refresh() {
                                        controller.snapshot.error = Some(error);
                                    }
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .map_err(|_| "Could not start Wi-Fi worker".to_owned())?;
            Ok(send)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub async fn request(request: Request) -> Result<Snapshot> {
    let worker = worker()?;
    let (reply, response) = async_channel::bounded(1);
    worker.send(Job::Request(request, reply)).map_err(|_| "Wi-Fi worker stopped")?;
    response.recv().await.map_err(|_| "Wi-Fi worker stopped before replying")?
}

/// Prevents new Wi-Fi operations until suspend (including hardware recovery) ends.
/// Dropping this guard unlocks requests; UI readiness separately triggers restoration.
pub(crate) struct SleepGuard;

pub(crate) fn prepare_sleep() -> Result<SleepGuard> {
    let timeout = Duration::from_secs(20);
    let (reply, response) = mpsc::sync_channel(0);
    worker()?.send(Job::PrepareSleep { reply, deadline: Instant::now() + timeout }).map_err(|_| "Wi-Fi worker stopped")?;
    wait_for_sleep(response, timeout)?;
    Ok(SleepGuard)
}

/// Called after the first restored UI frame is presented. Only enqueues work;
/// radio startup and connection never block the UI thread.
pub fn resume_after_ui() {
    if let Some(Ok(worker)) = WORKER.get() {
        let _ = worker.send(Job::UiReady);
    }
}

fn wait_for_sleep(response: mpsc::Receiver<Result<()>>, timeout: Duration) -> Result<()> {
    response.recv_timeout(timeout).map_err(|error| match error {
        mpsc::RecvTimeoutError::Timeout => "Wi-Fi sleep preparation timed out",
        mpsc::RecvTimeoutError::Disconnected => "Wi-Fi worker stopped before preparing sleep",
    })?
}

fn run_bounded(command: &mut Command, timeout: Duration, operation: &str) -> Result<()> {
    let mut child = command.process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(|_| format!("Could not start {operation}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut diagnostic = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut diagnostic);
                }
                let diagnostic = diagnostic.trim();
                return if status.success() {
                    Ok(())
                } else if diagnostic.is_empty() {
                    Err(format!("{operation} failed ({status})"))
                } else {
                    Err(format!("{operation} failed ({status}): {diagnostic}"))
                };
            }
            Ok(None) if started.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            result => {
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.wait();
                let mut diagnostic = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut diagnostic);
                }
                let diagnostic = diagnostic.trim();
                return Err(if result.is_err() {
                    format!("Could not check {operation}")
                } else if diagnostic.is_empty() {
                    format!("{operation} timed out")
                } else {
                    format!("{operation} timed out: {diagnostic}")
                });
            }
        }
    }
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        if let Ok(worker) = worker() {
            let _ = worker.send(Job::FinishSleep);
        }
    }
}

struct Attempt {
    id: u32,
    temporary: bool,
    previous: Vec<u32>,
    started: Instant,
}
struct Controller {
    sleeping: bool,
    restore_after_ui: bool,
    interface: Option<String>,
    socket_path: Option<PathBuf>,
    snapshot: Snapshot,
    attempt: Option<Attempt>,
    dhcp: Option<Child>,
    scan_started: Option<Instant>,
    monitor: Option<Control>,
    address_requested: bool,
    address_started: Option<Instant>,
}

fn valid_interface(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric) && value.len() < 16 && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

impl Controller {
    fn new() -> Self {
        let interface = std::env::var("INTERFACE")
            .ok()
            .filter(|name| valid_interface(name))
            .or_else(|| fs::read_dir("/var/run/wpa_supplicant").ok()?.filter_map(|entry| entry.ok()).map(|entry| entry.file_name().to_string_lossy().into_owned()).find(|name| valid_interface(name)));
        let power_available = cfg!(target_arch = "arm") && std::env::var_os("BOKHEIM_KOBO_WIFI_HELPER").is_some() && interface.is_some();
        let socket_path = interface.as_ref().map(|name| PathBuf::from("/var/run/wpa_supplicant").join(name));
        Self {
            sleeping: false,
            restore_after_ui: false,
            interface,
            socket_path,
            snapshot: Snapshot { power_available, ..Snapshot::default() },
            attempt: None,
            dhcp: None,
            scan_started: None,
            monitor: None,
            address_requested: false,
            address_started: None,
        }
    }
    fn control(&self) -> Result<Control> {
        Control::open(self.socket_path.as_ref().ok_or("Wi-Fi interface is unknown. Restart Bokheim from the Kobo launcher.")?)
    }
    fn prepare_sleep_with(&mut self, power_off: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        self.restore_after_ui = self.snapshot.phase != Phase::Off || self.control().is_ok();
        self.sleeping = true;
        if let Err(error) = power_off(self) {
            self.restore_after_ui = false;
            self.sleeping = false;
            self.snapshot.error = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }
    fn resume_after_ui_with(&mut self, power_on: impl FnOnce(&mut Self) -> Result<()>) {
        if self.sleeping || !self.restore_after_ui {
            return;
        }
        self.restore_after_ui = false;
        self.snapshot.error = None;
        if let Err(error) = power_on(self) {
            self.snapshot.error = Some(error);
        }
    }
    fn execute(&mut self, request: Request) -> Result<Snapshot> {
        if self.sleeping {
            return if matches!(request, Request::Status) { Ok(self.snapshot.clone()) } else { Err("Wi-Fi is unavailable while the device is going to sleep".into()) };
        }
        if !matches!(request, Request::Status) {
            self.snapshot.error = None;
            // An explicit user action supersedes the deferred automatic restore.
            self.restore_after_ui = false;
        }
        let result = match request {
            Request::Status => self.refresh(),
            Request::Power(enabled) => self.power(enabled),
            Request::Scan => {
                let result = self.ensure_monitor().and_then(|_| self.control()).and_then(|control| control.ok("SCAN"));
                if result.is_ok() {
                    self.scan_started = Some(Instant::now());
                    self.snapshot.scanning = true;
                }
                result
            }
            Request::ConnectSaved(id) => self.connect(id, false),
            Request::Join { network, password } => self.join(network, password),
            Request::Disconnect => self.disconnect(),
        };
        if let Err(error) = result {
            self.snapshot.error = Some(error);
        }
        Ok(self.snapshot.clone())
    }
    fn ensure_monitor(&mut self) -> Result<()> {
        self.monitor = Some(Control::monitor(self.socket_path.as_ref().ok_or("Wi-Fi interface is unknown")?)?);
        Ok(())
    }
    fn service_lost(&mut self) {
        self.stop_dhcp();
        self.monitor = None;
        self.attempt = None;
        self.scan_started = None;
        self.address_requested = false;
        self.address_started = None;
        self.snapshot = Snapshot { power_available: self.snapshot.power_available, ..Snapshot::default() };
    }
    fn refresh(&mut self) -> Result<()> {
        let control = match self.control() {
            Ok(control) => control,
            Err(error) => {
                self.service_lost();
                if let Some(interface) = &self.interface {
                    if !PathBuf::from("/sys/class/net").join(interface).exists() {
                        return Ok(());
                    }
                }
                return Err(error);
            }
        };
        let (status, saved) = match control.command("STATUS").and_then(|status| control.command("LIST_NETWORKS").map(|saved| (status, saved))) {
            Ok(replies) => replies,
            Err(error) => {
                self.service_lost();
                return Err(error);
            }
        };
        let data = fields(&status);
        self.snapshot.phase = phase(&data);
        self.snapshot.network = data.get("ssid").map(|ssid| String::from_utf8_lossy(&decode_ssid(ssid)).into_owned());
        self.snapshot.saved = parse_saved(&saved);
        let events = match self.monitor.as_ref().map(Control::events).transpose() {
            Ok(events) => events.unwrap_or_default(),
            Err(error) => {
                self.service_lost();
                return Err(error);
            }
        };
        if self.attempt.is_some() && events.iter().any(|event| event.contains("reason=WRONG_KEY") || event.contains("CTRL-EVENT-AUTH-REJECT")) {
            self.cancel_attempt(&control)?;
            return Err("Authentication failed. Check the network password.".into());
        }
        if let Some(started) = self.scan_started {
            if events.iter().any(|event| event.contains("CTRL-EVENT-SCAN-RESULTS")) {
                self.snapshot.networks = parse_scan(&control.command("SCAN_RESULTS")?)?;
                self.scan_started = None;
                self.snapshot.scanning = false;
            } else if started.elapsed() > Duration::from_secs(20) {
                self.scan_started = None;
                self.snapshot.scanning = false;
                return Err("Network scan timed out. Try Refresh again.".into());
            }
        }
        if self.address_requested && self.snapshot.phase == Phase::ObtainingAddress && self.address_started.is_none() {
            self.start_dhcp()?;
            self.address_started = Some(Instant::now());
        }
        if self.snapshot.phase == Phase::Connected {
            self.address_requested = false;
        }
        if self.address_started.is_some_and(|started| started.elapsed() > Duration::from_secs(25)) && self.address_requested {
            self.cancel_attempt(&control)?;
            self.address_requested = false;
            return Err("Obtaining a network address timed out".into());
        }
        if self.attempt.is_some() {
            let requested = self.attempt.as_ref().unwrap();
            let current_id = data.get("id").and_then(|id| id.parse::<u32>().ok());
            if self.snapshot.phase == Phase::Connected && current_id == Some(requested.id) {
                self.attempt = None;
            } else if requested.started.elapsed() > Duration::from_secs(45) {
                self.cancel_attempt(&control)?;
                return Err("Connection timed out. Check the password and network availability.".into());
            }
        }
        if let Some(child) = self.dhcp.as_mut() {
            if let Some(status) = child.try_wait().map_err(|_| "Could not check address acquisition")? {
                self.dhcp = None;
                if !status.success() && self.address_requested {
                    self.cancel_attempt(&control)?;
                    return Err("Could not obtain a network address. Try connecting again.".into());
                }
            }
        }
        Ok(())
    }
    fn power(&mut self, enabled: bool) -> Result<()> {
        let helper = std::env::var_os("BOKHEIM_KOBO_WIFI_HELPER").ok_or("Wi-Fi helper is missing")?;
        self.power_with_helper(enabled, &helper)
    }
    fn power_before(&mut self, enabled: bool, deadline: Instant) -> Result<()> {
        let helper = std::env::var_os("BOKHEIM_KOBO_WIFI_HELPER").ok_or("Wi-Fi helper is missing")?;
        let timeout = deadline.saturating_duration_since(Instant::now()).min(Duration::from_secs(15));
        if timeout.is_zero() {
            return Err("Wi-Fi power request expired".into());
        }
        self.power_with_helper_timeout(enabled, &helper, timeout)
    }
    fn power_with_helper(&mut self, enabled: bool, helper: &std::ffi::OsStr) -> Result<()> {
        self.power_with_helper_timeout(enabled, helper, Duration::from_secs(15))
    }
    fn power_with_helper_timeout(&mut self, enabled: bool, helper: &std::ffi::OsStr, timeout: Duration) -> Result<()> {
        if !self.snapshot.power_available {
            return Err("Wi-Fi power control is unavailable on this device".into());
        }
        if !enabled {
            // Radio shutdown must still run when supplicant is unresponsive.
            self.stop_dhcp();
            self.attempt = None;
            self.address_requested = false;
            self.address_started = None;
        }
        run_bounded(Command::new("/bin/sh").env("INTERFACE", self.interface.as_deref().ok_or("Wi-Fi interface is missing")?).arg(helper).arg(if enabled { "on" } else { "off" }), timeout, "Wi-Fi power control")?;
        self.monitor = None;
        self.scan_started = None;
        self.snapshot.scanning = false;
        self.address_requested = enabled;
        self.address_started = None;
        if enabled {
            self.refresh()
        } else {
            self.snapshot = Snapshot { power_available: self.snapshot.power_available, ..Snapshot::default() };
            Ok(())
        }
    }
    fn connect(&mut self, id: u32, temporary: bool) -> Result<()> {
        let control = self.control()?;
        self.cancel_attempt(&control)?;
        let saved = control.command("LIST_NETWORKS")?;
        let previous = enabled_networks(&saved);
        if !parse_saved(&saved).iter().any(|network| network.id == id) {
            return Err("The selected network is no longer available".into());
        }
        self.ensure_monitor()?;
        if cfg!(target_arch = "arm") {
            let interface = self.interface.as_ref().ok_or("Wi-Fi interface is unknown")?;
            // Drop the old lease before changing networks so it cannot count as the new connection.
            run_bounded(Command::new("ifconfig").args([interface.as_str(), "0.0.0.0"]), Duration::from_secs(5), "Resetting the previous network address")?;
        }
        control.ok(&format!("SELECT_NETWORK {id}"))?;
        self.address_requested = true;
        self.address_started = None;
        self.attempt = Some(Attempt { id, temporary, previous, started: Instant::now() });
        self.snapshot.phase = Phase::Connecting;
        self.snapshot.network = parse_saved(&saved).into_iter().find(|network| network.id == id).map(|network| network.name);
        Ok(())
    }
    fn join(&mut self, network: Network, password: String) -> Result<()> {
        if network.ssid.is_empty() || network.ssid.len() > 32 {
            return Err("Network name must contain 1 to 32 bytes".into());
        }
        if network.security == Security::Unsupported {
            return Err("This network's security mode is not supported yet".into());
        }
        let psk = match network.security {
            Security::Open => None,
            Security::WpaPersonal => {
                if !(8..=63).contains(&password.len()) || !password.is_ascii() || password.bytes().any(|b| b.is_ascii_control()) {
                    return Err("Enter a WPA password containing 8 to 63 printable ASCII characters".into());
                }
                let mut derived = [0_u8; 32];
                pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), &network.ssid, 4096, &mut derived);
                Some(hex(&derived))
            }
            Security::Unsupported => unreachable!(),
        };
        let control = self.control()?;
        let id: u32 = control.command("ADD_NETWORK")?.parse().map_err(|_| "Invalid network identifier from Wi-Fi service")?;
        let result = (|| {
            control.ok(&format!("SET_NETWORK {id} ssid {}", hex(&network.ssid)))?;
            control.ok(&format!("SET_NETWORK {id} key_mgmt {}", if psk.is_some() { "WPA-PSK" } else { "NONE" }))?;
            if let Some(psk) = psk {
                control.ok(&format!("SET_NETWORK {id} psk {psk}"))?;
            }
            self.connect(id, true)
        })();
        if result.is_err() {
            let _ = control.ok(&format!("REMOVE_NETWORK {id}"));
        }
        result
    }
    fn cancel_attempt(&mut self, control: &Control) -> Result<()> {
        self.stop_dhcp();
        self.address_requested = false;
        self.address_started = None;
        if let Some(attempt) = self.attempt.take() {
            control.ok("DISCONNECT")?;
            if attempt.temporary {
                control.ok(&format!("REMOVE_NETWORK {}", attempt.id))?;
            } else if !attempt.previous.contains(&attempt.id) {
                control.ok(&format!("DISABLE_NETWORK {}", attempt.id))?;
            }
            for id in attempt.previous {
                control.ok(&format!("ENABLE_NETWORK {id}"))?;
            }
        }
        Ok(())
    }
    fn disconnect(&mut self) -> Result<()> {
        let control = self.control()?;
        self.cancel_attempt(&control)?;
        control.ok("DISCONNECT")?;
        self.snapshot.phase = Phase::Disconnected;
        self.snapshot.network = None;
        Ok(())
    }
    fn start_dhcp(&mut self) -> Result<()> {
        if !cfg!(target_arch = "arm") {
            return Err("IP configuration is only available on Kobo hardware".into());
        }
        let interface = self.interface.as_ref().ok_or("Wi-Fi interface is unknown")?;
        self.dhcp = Some(
            Command::new("udhcpc")
                .args(["-f", "-n", "-q", "-t", "5", "-T", "3", "-i", interface, "-s", "/etc/udhcpc.d/default.script"])
                .env_remove("LD_LIBRARY_PATH")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| "Could not start network address acquisition")?,
        );
        Ok(())
    }
    fn stop_dhcp(&mut self) {
        if let Some(mut child) = self.dhcp.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.stop_dhcp();
    }
}

fn phase(data: &std::collections::HashMap<&str, &str>) -> Phase {
    match data.get("wpa_state").copied() {
        Some("COMPLETED") => {
            if data.get("ip_address").and_then(|ip| ip.parse::<std::net::IpAddr>().ok()).is_some_and(|ip| !ip.is_unspecified() && !ip.is_loopback()) {
                Phase::Connected
            } else {
                Phase::ObtainingAddress
            }
        }
        Some("ASSOCIATING" | "ASSOCIATED" | "AUTHENTICATING" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE") => Phase::Connecting,
        _ => Phase::Disconnected,
    }
}
fn parse_saved(text: &str) -> Vec<SavedNetwork> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            let id = fields.next()?.parse().ok()?;
            let name = String::from_utf8_lossy(&decode_ssid(fields.next()?)).into_owned();
            Some(SavedNetwork { id, name })
        })
        .collect()
}
fn enabled_networks(text: &str) -> Vec<u32> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<_> = line.splitn(4, '\t').collect();
            (fields.len() == 4 && !fields[3].contains("[DISABLED]")).then(|| fields[0].parse().ok()).flatten()
        })
        .collect()
}
fn parse_scan(text: &str) -> Result<Vec<Network>> {
    let mut networks = Vec::<Network>::new();
    for line in text.lines().skip(1) {
        let values: Vec<_> = line.splitn(5, '\t').collect();
        if values.len() != 5 {
            return Err("Invalid Wi-Fi scan result".into());
        }
        let flags = values[3];
        let security = if flags.contains("PSK") && !flags.contains("EAP") {
            Security::WpaPersonal
        } else if flags == "[ESS]" || flags.is_empty() {
            Security::Open
        } else {
            Security::Unsupported
        };
        let ssid = decode_ssid(values[4]);
        if ssid.is_empty() {
            continue;
        }
        let signal = values[2].parse().map_err(|_| "Invalid Wi-Fi signal level")?;
        if let Some(previous) = networks.iter_mut().find(|network| network.ssid == ssid && network.security == security) {
            previous.signal = previous.signal.max(signal);
        } else {
            networks.push(Network { ssid, signal, security });
        }
    }
    networks.sort_by_key(|network| std::cmp::Reverse(network.signal));
    Ok(networks)
}

#[cfg(test)]
mod tests;

/// Kobo's legacy CM_WIFI_CTRL operation, also used by firmware without a power module.
#[cfg(target_arch = "arm")]
pub fn set_radio_power(enabled: bool) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let file = fs::OpenOptions::new().write(true).open("/dev/ntx_io")?;
    let result = unsafe { libc::ioctl(file.as_raw_fd(), 208, libc::c_int::from(enabled)) };
    if result < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}
