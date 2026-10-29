//! Read native Kobo preferences without changing the firmware's configuration.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::time::Duration;

const NATIVE_CONFIG: &str = "/mnt/onboard/.kobo/Kobo/Kobo eReader.conf";
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Follow Nickel's Energy saving and privacy setting. Zero means Never.
/// Load once at launch: changing it in Nickel requires leaving Bokheim anyway.
pub fn idle_timeout() -> Option<Duration> {
    load_idle_timeout(Path::new(NATIVE_CONFIG))
}

fn load_idle_timeout(path: &Path) -> Option<Duration> {
    match fs::read_to_string(path).and_then(|config| parse_idle_timeout(&config)) {
        Ok(timeout) => timeout,
        Err(error) => {
            if error.kind() != ErrorKind::NotFound {
                eprintln!("Could not read Kobo automatic sleep setting; using five minutes: {error}");
            }
            Some(DEFAULT_IDLE_TIMEOUT)
        }
    }
}

fn parse_idle_timeout(config: &str) -> io::Result<Option<Duration>> {
    let mut power_options = false;
    let mut minutes = None;
    for line in config.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) {
            power_options = section.trim() == "PowerOptions";
            continue;
        }
        if power_options
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "AutoSleepMinutes"
        {
            // Use the last occurrence of the native preference.
            minutes = Some(value.trim());
        }
    }
    let Some(minutes) = minutes else {
        return Ok(Some(DEFAULT_IDLE_TIMEOUT));
    };
    let minutes = minutes.parse::<u32>().map_err(|_| io::Error::new(ErrorKind::InvalidData, "Kobo AutoSleepMinutes must be a non-negative integer"))?;
    Ok((minutes != 0).then(|| Duration::from_secs(u64::from(minutes) * 60)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_native_minutes_and_never() {
        for minutes in [0, 5, 10, 15, 30, 45, 60, 255] {
            let config = format!("[PowerOptions]\nAutoSleepMinutes={minutes}\n");
            let expected = (minutes != 0).then(|| Duration::from_secs(minutes * 60));
            assert_eq!(parse_idle_timeout(&config).unwrap(), expected);
        }
    }

    #[test]
    fn reads_only_the_native_sleep_key_in_power_options() {
        let config = "\u{feff}; Kobo preferences\r\n\
            AutoSleepMinutes=1\r\n\
            [Other]\r\nAutoSleepMinutes=2\r\n\
            [PowerOptions]\r\nAutoOffMinutes=60\r\n\
            # AutoSleepMinutes=3\r\n AutoSleepMinutes = 15 \r\n\
            [Reading]\r\nAutoSleepMinutes=4\r\n";
        assert_eq!(parse_idle_timeout(config).unwrap(), Some(Duration::from_secs(15 * 60)));
    }

    #[test]
    fn missing_sleep_preference_uses_the_fallback() {
        for config in ["", "[PowerOptions]\nAutoOffMinutes=60", "[Reading]\nAutoSleepMinutes=0"] {
            assert_eq!(parse_idle_timeout(config).unwrap(), Some(DEFAULT_IDLE_TIMEOUT));
        }
    }

    #[test]
    fn invalid_preferences_do_not_disable_automatic_sleep() {
        for value in ["", "-1", "never", "abc", "0.5", "4294967296"] {
            assert!(parse_idle_timeout(&format!("[PowerOptions]\nAutoSleepMinutes={value}")).is_err());
        }
    }

    #[test]
    fn native_configuration_is_read_again_at_launch_and_never_rewritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Kobo eReader.conf");
        assert_eq!(load_idle_timeout(&path), Some(DEFAULT_IDLE_TIMEOUT));
        assert!(!path.exists());
        for (value, expected) in [("30", Some(Duration::from_secs(30 * 60))), ("0", None), ("invalid", Some(DEFAULT_IDLE_TIMEOUT))] {
            let config = format!("[PowerOptions]\nAutoSleepMinutes={value}\n[Other]\nKeep=unchanged\n");
            fs::write(&path, &config).unwrap();
            assert_eq!(load_idle_timeout(&path), expected);
            assert_eq!(fs::read_to_string(&path).unwrap(), config);
        }
    }
}
