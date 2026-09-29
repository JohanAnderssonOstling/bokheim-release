use std::fs;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use wpa_ctrl::{WpaControlReq, WpaController};

pub(super) type Result<T> = std::result::Result<T, String>;
static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

pub(super) struct Control {
    controller: WpaController,
    directory: PathBuf,
}

impl Control {
    pub(super) fn open(remote: &Path) -> Result<Self> {
        Self::open_with_timeout(remote, Duration::from_secs(3))
    }

    fn open_with_timeout(remote: &Path, timeout: Duration) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let directory = std::env::temp_dir().join(format!("bokheim-wpa-{}-{}", std::process::id(), NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)));
        fs::DirBuilder::new().mode(0o700).create(&directory).map_err(|_| "Cannot create Wi-Fi control directory")?;
        let result = (|| {
            let socket = UnixDatagram::bind(directory.join("client")).map_err(|_| "Cannot bind Wi-Fi control socket")?;
            socket.connect(remote).map_err(|_| "Wi-Fi service is unavailable")?;
            socket.set_read_timeout(Some(timeout)).map_err(|_| "Cannot configure Wi-Fi timeout")?;
            socket.set_write_timeout(Some(timeout)).map_err(|_| "Cannot configure Wi-Fi timeout")?;
            Ok(Self { controller: WpaController::from_socket(socket, directory.join("client")), directory: directory.clone() })
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(directory);
        }
        result
    }

    pub(super) fn command(&self, command: &str) -> Result<String> {
        // Never include commands in errors: SET_NETWORK can contain credentials.
        if command.len() > 127 {
            return Err("Wi-Fi command is too long".into());
        }
        self.controller.request(WpaControlReq::raw(command)).map_err(|_| "Could not contact Wi-Fi service")?;
        let mut bytes = [0; 65536];
        let reply = self.controller.recv(&mut bytes).map_err(|_| "Could not read Wi-Fi response")?.ok_or("Wi-Fi service did not reply")?.raw.to_owned();
        if reply.starts_with("FAIL") || reply == "UNKNOWN COMMAND" {
            return Err("Wi-Fi service rejected the operation".into());
        }
        Ok(reply)
    }

    pub(super) fn ok(&self, command: &str) -> Result<()> {
        if self.command(command)? == "OK" { Ok(()) } else { Err("Unexpected Wi-Fi service response".into()) }
    }

    pub(super) fn monitor(remote: &Path) -> Result<Self> {
        let control = Self::open(remote)?;
        control.ok("ATTACH")?;
        control.controller.set_nonblocking(true).map_err(|_| "Could not monitor Wi-Fi events")?;
        Ok(control)
    }

    pub(super) fn events(&self) -> Result<Vec<String>> {
        let mut events = Vec::new();
        let mut bytes = [0; 4096];
        for _ in 0..128 {
            match self.controller.recv(&mut bytes) {
                Ok(Some(message)) => events.push(message.raw.to_owned()),
                Ok(None) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return Err("Wi-Fi event connection was lost".into()),
            }
        }
        Ok(events)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub(super) fn fields(text: &str) -> std::collections::HashMap<&str, &str> {
    text.lines().filter_map(|line| line.split_once('=')).collect()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn decode_ssid(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut input = text.as_bytes().iter().copied().peekable();
    while let Some(byte) = input.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match input.next() {
            Some(b'x') => {
                let pair = [input.next().unwrap_or(b'?'), input.next().unwrap_or(b'?')];
                match std::str::from_utf8(&pair).ok().and_then(|pair| u8::from_str_radix(pair, 16).ok()) {
                    Some(value) => bytes.push(value),
                    None => {
                        bytes.extend_from_slice(b"\\x");
                        bytes.extend_from_slice(&pair);
                    }
                }
            }
            Some(b'n') => bytes.push(b'\n'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'e') => bytes.push(27),
            Some(value) => bytes.push(value),
            None => bytes.push(b'\\'),
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresponsive_service_times_out() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server");
        let _server = UnixDatagram::bind(&path).unwrap();
        let control = Control::open_with_timeout(&path, Duration::from_millis(25)).unwrap();
        let started = std::time::Instant::now();
        assert!(control.command("STATUS").is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
