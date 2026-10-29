//! Read the kernel's battery gauge without depending on a Kobo model name.
use std::{fs, io, path::Path};
use std::sync::OnceLock;
use tokio::sync::watch;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatteryStatus {
    pub percent: u8,
    pub status: String,
}

pub type BatteryReading = Result<BatteryStatus, String>;
static READINGS: OnceLock<watch::Sender<Option<BatteryReading>>> = OnceLock::new();

fn readings() -> &'static watch::Sender<Option<BatteryReading>> {
    READINGS.get_or_init(|| watch::channel(None).0)
}

/// The power manager owns periodic sampling. UI subscribers reuse those reads.
pub fn subscribe() -> watch::Receiver<Option<BatteryReading>> {
    readings().subscribe()
}

fn publish(sender: &watch::Sender<Option<BatteryReading>>, reading: BatteryReading) {
    sender.send_if_modified(|previous| {
        if previous.as_ref() == Some(&reading) { return false; }
        *previous = Some(reading);
        true
    });
}

pub fn read() -> io::Result<BatteryStatus> {
    let reading = read_from(Path::new("/sys/class/power_supply"));
    publish(readings(), reading.as_ref().cloned().map_err(ToString::to_string));
    reading
}

fn read_from(root: &Path) -> io::Result<BatteryStatus> {
    // Plato uses these interfaces directly: Kobo drivers need not report
    // type=Battery for their capacity/status attributes to be usable.
    for name in ["bd71827_bat", "mc13892_bat", "battery"] {
        let path = root.join(name);
        if path.exists() {
            return read_gauge(&path);
        }
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if fs::read_to_string(path.join("type")).is_ok_and(|kind| kind.trim() == "Battery") {
            return read_gauge(&path);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "Battery gauge unavailable"))
}

fn read_gauge(path: &Path) -> io::Result<BatteryStatus> {
    let capacity = path.join("capacity");
    let raw = fs::read_to_string(&capacity).map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", capacity.display())))?;
    let percent = raw.trim().parse::<f32>().ok().filter(|value| value.is_finite() && (0.0..=100.0).contains(value)).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("Invalid battery capacity at {}", capacity.display())))?;
    let status = fs::read_to_string(path.join("status")).unwrap_or_else(|_| "Unknown".into()).trim().to_owned();
    Ok(BatteryStatus { percent: percent.round() as u8, status })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_samples_replay_current_state_and_only_notify_changes() {
        let (sender, mut changes) = watch::channel(None);
        let status = BatteryStatus { percent: 73, status: "Discharging".into() };
        publish(&sender, Ok(status.clone()));
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), Some(Ok(status.clone())));
        publish(&sender, Ok(status.clone()));
        assert!(!changes.has_changed().unwrap());
        let mut late_ui = sender.subscribe();
        assert_eq!(*late_ui.borrow_and_update(), Some(Ok(status)));
        publish(&sender, Err("Battery gauge unavailable".into()));
        assert!(changes.has_changed().unwrap());
        assert!(late_ui.has_changed().unwrap());
        changes.borrow_and_update();
        publish(&sender, Err("Battery gauge unavailable".into()));
        assert!(!changes.has_changed().unwrap());
    }
    #[test]
    fn plato_interfaces_work_without_battery_type() {
        for name in ["bd71827_bat", "mc13892_bat", "battery"] {
            let root = tempfile::tempdir().unwrap();
            let battery = root.path().join(name);
            fs::create_dir(&battery).unwrap();
            fs::write(battery.join("capacity"), "72.5\n").unwrap();
            fs::write(battery.join("status"), "Discharging\n").unwrap();
            assert_eq!(read_from(root.path()).unwrap(), BatteryStatus { percent: 73, status: "Discharging".into() });
            fs::write(battery.join("type"), "Unknown\n").unwrap();
            assert_eq!(read_from(root.path()).unwrap().percent, 73);
            for invalid in ["NaN", "inf", "-1", "101", "bad"] {
                fs::write(battery.join("capacity"), invalid).unwrap();
                assert_eq!(read_from(root.path()).unwrap_err().kind(), io::ErrorKind::InvalidData);
            }
        }
    }
    #[test]
    fn reads_battery_instead_of_external_power_supply() {
        let root = tempfile::tempdir().unwrap();
        for (name, kind) in [("usb", "USB"), ("mc13892_bat", "Battery")] {
            let path = root.path().join(name);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("type"), kind).unwrap();
        }
        let battery = root.path().join("mc13892_bat");
        fs::write(battery.join("capacity"), "73\n").unwrap();
        fs::write(battery.join("status"), "Charging\n").unwrap();
        assert_eq!(read_from(root.path()).unwrap(), BatteryStatus { percent: 73, status: "Charging".into() });
        fs::write(battery.join("capacity"), "150").unwrap();
        assert_eq!(read_from(root.path()).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
