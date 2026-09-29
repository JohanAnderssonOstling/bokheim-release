//! Read the kernel's battery gauge without depending on a Kobo model name.
use std::{fs, io, path::Path};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatteryStatus {
    pub percent: u8,
    pub status: String,
}

pub fn read() -> io::Result<BatteryStatus> {
    read_from(Path::new("/sys/class/power_supply"))
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
