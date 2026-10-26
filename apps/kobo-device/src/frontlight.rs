//! Kobo's standard, mixed-channel and premixed frontlight interfaces.
//! Channel response curves and interfaces follow the Plato hardware reference.
use crate::profile::{self, Frontlight, Profile};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const INTENSITY: &str = "sys/class/backlight/mxc_msp430.0/brightness";
const COLOUR: [&str; 3] = ["sys/class/leds/aw99703-bl_FL1/color", "sys/class/backlight/lm3630a_led/color", "sys/class/backlight/tlc5947_bl/color"];
static CONTROLLER: OnceLock<Mutex<Option<Controller>>> = OnceLock::new();

pub(super) fn with_controller<T>(operation: impl FnOnce(&mut Controller) -> io::Result<T>) -> io::Result<T> {
    let mut state = CONTROLLER.get_or_init(|| Mutex::new(None)).lock().map_err(|_| io::Error::other("Kobo frontlight state lock failed"))?;
    if state.is_none() {
        *state = Some(Controller::new(*profile::current()?, PathBuf::from("/")));
    }
    operation(state.as_mut().unwrap())
}

pub(super) struct Controller {
    profile: Profile,
    root: PathBuf,
    // Retain the logical pair for channel drivers, particularly while off.
    levels: Option<(u8, u8)>,
}

impl Controller {
    fn new(profile: Profile, root: PathBuf) -> Self {
        Self { profile, root, levels: None }
    }

    pub(super) fn brightness(&mut self) -> io::Result<u8> {
        match self.profile.frontlight() {
            Frontlight::None => Err(unsupported("This Kobo has no frontlight")),
            Frontlight::NaturalTwoChannel | Frontlight::NaturalThreeChannel => self.mixed_levels().map(|levels| levels.0),
            Frontlight::Premixed { .. } => read_number(&self.root.join(INTENSITY), 100),
            Frontlight::Standard => {
                if self.root.join(INTENSITY).exists() {
                    read_number(&self.root.join(INTENSITY), 100)
                } else if let Some(levels) = self.levels {
                    Ok(levels.0)
                } else {
                    self.nickel_intensity()
                }
            }
        }
    }

    pub(super) fn warmth(&mut self) -> io::Result<Option<u8>> {
        match self.profile.frontlight() {
            Frontlight::None | Frontlight::Standard => Ok(None),
            Frontlight::NaturalTwoChannel | Frontlight::NaturalThreeChannel => self.mixed_levels().map(|levels| Some(levels.1)),
            Frontlight::Premixed { inverted_warmth } => {
                let raw = read_number(&self.colour_node()?, 10)?;
                Ok(Some(if inverted_warmth { 10 - raw } else { raw } * 10))
            }
        }
    }

    pub(super) fn set_brightness(&mut self, percent: u8) -> io::Result<u8> {
        validate_percent(percent)?;
        match self.profile.frontlight() {
            Frontlight::None => return Err(unsupported("This Kobo has no frontlight")),
            Frontlight::NaturalTwoChannel | Frontlight::NaturalThreeChannel => {
                let (_, warmth) = self.mixed_levels()?;
                self.set_mixed(percent, warmth)?;
            }
            Frontlight::Premixed { .. } => write_number(&self.root.join(INTENSITY), percent)?,
            Frontlight::Standard => {
                self.standard_ioctl(percent)?;
                self.levels = Some((percent, 0));
            }
        }
        Ok(percent)
    }

    pub(super) fn set_warmth(&mut self, percent: u8) -> io::Result<u8> {
        validate_percent(percent)?;
        match self.profile.frontlight() {
            Frontlight::None | Frontlight::Standard => Err(unsupported("This Kobo has no natural-light control")),
            Frontlight::NaturalTwoChannel | Frontlight::NaturalThreeChannel => {
                let (intensity, _) = self.mixed_levels()?;
                self.set_mixed(intensity, percent)?;
                Ok(percent)
            }
            Frontlight::Premixed { inverted_warmth } => {
                let level = (percent + 5) / 10;
                write_number(&self.colour_node()?, if inverted_warmth { 10 - level } else { level })?;
                Ok(level * 10)
            }
        }
    }

    fn colour_node(&self) -> io::Result<PathBuf> {
        COLOUR.iter().map(|path| self.root.join(path)).find(|path| path.is_file()).ok_or_else(|| unsupported("The Kobo natural-light driver is unavailable"))
    }

    fn channels(&self) -> &'static [&'static str] {
        match self.profile.frontlight() {
            Frontlight::NaturalThreeChannel => &["lm3630a_led1b", "lm3630a_led1a", "lm3630a_ledb"],
            _ => &["lm3630a_ledb", "lm3630a_leda"],
        }
    }

    fn channel_dir(&self, channel: &str) -> PathBuf {
        self.root.join("sys/class/backlight").join(channel)
    }

    fn channel_level(&self, channel: &str) -> io::Result<f64> {
        let directory = self.channel_dir(channel);
        if read_integer(&directory.join("bl_power"))? == 0 {
            return Ok(0.0);
        }
        let max = read_integer(&directory.join("max_brightness"))?;
        let value = read_integer(&directory.join("brightness"))?;
        if max == 0 || value > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid Kobo frontlight channel range"));
        }
        Ok(100.0 * f64::from(value) / f64::from(max))
    }

    fn mixed_levels(&mut self) -> io::Result<(u8, u8)> {
        if let Some(levels) = self.levels {
            return Ok(levels);
        }
        let white = self.channel_level(self.channels()[0])? / 80.0;
        let (warm_channel, scale) = if self.channels().len() == 3 { (self.channels()[2], 64.0) } else { (self.channels()[1], 95.0) };
        let warm = (self.channel_level(warm_channel)? / scale).powi(2);
        let intensity = (warm + (warm * warm + 4.0 * white * white).sqrt()) / 2.0;
        let warmth = if intensity > 0.0 { warm / intensity } else { 0.0 };
        let levels = ((100.0 * intensity).round().clamp(0.0, 100.0) as u8, (100.0 * warmth).round().clamp(0.0, 100.0) as u8);
        self.levels = Some(levels);
        Ok(levels)
    }

    fn set_mixed(&mut self, intensity: u8, warmth: u8) -> io::Result<()> {
        let i = f64::from(intensity) / 100.0;
        let w = f64::from(warmth) / 100.0;
        let white = 80.0 * i * (1.0 - w).sqrt();
        let warm = (w * i).sqrt();
        let values = if self.channels().len() == 3 {
            let green = 64.0 * warm;
            let red = if green == 0.0 { 0.0 } else { green + 20.0 + 7.0 * (1.0 - green / 64.0) + w * 4.0 };
            vec![white, red, green]
        } else {
            vec![white, 95.0 * warm]
        };
        // Validate/open every node before writing, without creating missing nodes.
        let mut channels = Vec::new();
        for (name, percent) in self.channels().iter().zip(values) {
            let directory = self.channel_dir(name);
            let max = read_integer(&directory.join("max_brightness"))?;
            if max == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid Kobo frontlight maximum"));
            }
            channels.push((directory, (percent.clamp(0.0, 100.0) * f64::from(max) / 100.0) as u32));
        }
        let previous: Vec<_> = channels.iter().map(|(directory, _)| Ok((read_integer(&directory.join("brightness"))?, read_integer(&directory.join("bl_power"))?))).collect::<io::Result<_>>()?;
        for (directory, _) in &channels {
            OpenOptions::new().write(true).open(directory.join("brightness"))?;
            OpenOptions::new().write(true).open(directory.join("bl_power"))?;
        }
        let result = channels.iter().try_for_each(|(directory, value)| {
            write_value(&directory.join("brightness"), *value)?;
            write_value(&directory.join("bl_power"), if *value > 0 { 31 } else { 0 })
        });
        if let Err(error) = result {
            for ((directory, _), (value, power)) in channels.iter().zip(previous) {
                let _ = write_value(&directory.join("brightness"), value);
                let _ = write_value(&directory.join("bl_power"), power);
            }
            self.levels = None;
            return Err(error);
        }
        self.levels = Some((intensity, warmth));
        Ok(())
    }

    fn nickel_intensity(&self) -> io::Result<u8> {
        let config = fs::read_to_string(self.root.join("mnt/onboard/.kobo/Kobo/Kobo eReader.conf"))?;
        if config.lines().any(|line| line.trim() == "FrontLightState=false") {
            return Ok(0);
        }
        config.lines().find_map(|line| line.trim().strip_prefix("FrontLightLevel=")).and_then(|value| value.parse::<u8>().ok()).filter(|value| *value <= 100).ok_or_else(|| unsupported("Cannot read the initial Kobo frontlight level"))
    }

    #[cfg(target_os = "linux")]
    fn standard_ioctl(&self, percent: u8) -> io::Result<()> {
        let file = OpenOptions::new().write(true).open(self.root.join("dev/ntx_io"))?;
        // Legacy Kobo ioctl takes the integer itself, not an address.
        if unsafe { libc::ioctl(file.as_raw_fd(), 241, libc::c_int::from(percent)) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn standard_ioctl(&self, _: u8) -> io::Result<()> {
        Err(unsupported("Kobo frontlight ioctl requires Linux"))
    }
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
fn validate_percent(percent: u8) -> io::Result<()> {
    if percent > 100 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Light level must be between 0 and 100"));
    }
    Ok(())
}
fn read_integer(path: &Path) -> io::Result<u32> {
    fs::read_to_string(path)?.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid Kobo frontlight value"))
}
fn read_number(path: &Path, maximum: u8) -> io::Result<u8> {
    let number = read_integer(path)?;
    if number > u32::from(maximum) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Kobo frontlight value is out of range"));
    }
    Ok(number as u8)
}
fn write_value(path: &Path, value: u32) -> io::Result<()> {
    write!(OpenOptions::new().write(true).truncate(true).open(path)?, "{value}")
}
fn write_number(path: &Path, value: u8) -> io::Result<()> {
    write_value(path, u32::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(root: &Path, path: &str, value: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    #[test]
    fn premixed_families_preserve_brightness_and_use_their_warmth_scale() {
        for (id, inverted) in [(388, true), (383, false), (386, true), (390, true)] {
            let root = tempfile::tempdir().unwrap();
            node(root.path(), INTENSITY, "37");
            node(root.path(), COLOUR[0], "5");
            let mut control = Controller::new(*Profile::from_id(id).unwrap(), root.path().into());
            assert_eq!(control.set_warmth(24).unwrap(), 20);
            assert_eq!(control.warmth().unwrap(), Some(20));
            assert_eq!(read_number(&root.path().join(COLOUR[0]), 10).unwrap(), if inverted { 8 } else { 2 });
            assert_eq!(control.brightness().unwrap(), 37);
            control.set_brightness(0).unwrap();
            assert_eq!(control.warmth().unwrap(), Some(20));
            assert!(control.set_brightness(101).is_err());
        }
    }

    #[test]
    fn mixed_channel_lights_keep_warmth_while_off_and_restore_every_channel() {
        for id in [373, 374, 378] {
            let root = tempfile::tempdir().unwrap();
            let mut control = Controller::new(*Profile::from_id(id).unwrap(), root.path().into());
            for channel in control.channels() {
                for (name, value) in [("brightness", "0"), ("max_brightness", "255"), ("bl_power", "0")] {
                    node(root.path(), &format!("sys/class/backlight/{channel}/{name}"), value);
                }
            }
            control.set_warmth(60).unwrap();
            control.set_brightness(37).unwrap();
            assert_eq!(control.warmth().unwrap(), Some(60));
            assert_eq!(control.brightness().unwrap(), 37);
            control.levels = None;
            assert!(control.brightness().unwrap().abs_diff(37) <= 1);
            assert!(control.warmth().unwrap().unwrap().abs_diff(60) <= 2);
            let warmth = control.warmth().unwrap();
            control.set_brightness(0).unwrap();
            assert_eq!(control.warmth().unwrap(), warmth);
            control.set_brightness(37).unwrap();
            assert!(control.channel_level(control.channels()[0]).unwrap() > 0.0);
        }
    }

    #[test]
    fn missing_and_unsupported_drivers_are_never_created() {
        let root = tempfile::tempdir().unwrap();
        let mut touch = Controller::new(*Profile::from_id(320).unwrap(), root.path().into());
        assert_eq!(touch.warmth().unwrap(), None);
        assert_eq!(touch.set_brightness(50).unwrap_err().kind(), io::ErrorKind::Unsupported);
        let mut clara = Controller::new(*Profile::from_id(376).unwrap(), root.path().into());
        assert!(clara.set_warmth(50).is_err());
        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    }
}
