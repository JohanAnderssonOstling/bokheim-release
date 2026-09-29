//! Percentage frontlight control for Kobo's premixed driver, including Libra 2.
//! Hardware interface reference: reference/plato/crates/core/src/frontlight/premixed.rs.
//! Keep warmth untouched: this node accepts intensity directly in 0..=100.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

const BRIGHTNESS: &str = "/sys/class/backlight/mxc_msp430.0/brightness";

pub fn read() -> io::Result<u8> {
    read_at(Path::new(BRIGHTNESS))
}

pub fn set(percent: u8) -> io::Result<u8> {
    set_at(Path::new(BRIGHTNESS), percent)
}

fn read_at(path: &Path) -> io::Result<u8> {
    let value = fs::read_to_string(path).map_err(device_error)?;
    parse_percent(&value)
}

fn parse_percent(value: &str) -> io::Result<u8> {
    value.trim().parse::<u8>().ok().filter(|value| *value <= 100).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Kobo returned an invalid brightness percentage"))
}

fn set_at(path: &Path, percent: u8) -> io::Result<u8> {
    if percent > 100 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Brightness must be between 0 and 100"));
    }
    // Do not create missing device nodes or retain a sysfs file offset between writes.
    let mut file = OpenOptions::new().write(true).truncate(true).open(path).map_err(device_error)?;
    write!(file, "{percent}").map_err(device_error)?;
    drop(file);
    read_at(path)
}

fn device_error(error: io::Error) -> io::Error {
    let message = match error.kind() {
        io::ErrorKind::NotFound => "Brightness control is unavailable on this Kobo frontlight driver",
        io::ErrorKind::PermissionDenied => "Kobo denied access to the frontlight",
        _ => "Could not access the Kobo frontlight",
    };
    io::Error::new(error.kind(), format!("{message}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_percentages_and_rejects_invalid_driver_values() {
        for (text, expected) in [("0\n", 0), ("42\n", 42), ("100\n", 100)] {
            assert_eq!(parse_percent(text).unwrap(), expected);
        }
        for text in ["", "-1", "101", "256", "50%"] {
            assert_eq!(parse_percent(text).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn changes_brightness_repeatedly_including_off_and_rejects_out_of_range() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for percent in [100, 5, 0, 75] {
            assert_eq!(set_at(file.path(), percent).unwrap(), percent);
        }
        assert_eq!(set_at(file.path(), 101).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(read_at(file.path()).unwrap(), 75);
    }

    #[test]
    fn unsupported_driver_is_not_created() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("brightness");
        assert_eq!(set_at(&missing, 50).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!missing.exists());
    }
}
