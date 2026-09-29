//! Premixed warmth control, following Plato's frontlight/premixed.rs.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

const NODES: [&str; 3] = ["/sys/class/leds/aw99703-bl_FL1/color", "/sys/class/backlight/lm3630a_led/color", "/sys/class/backlight/tlc5947_bl/color"];

fn driver() -> io::Result<Option<(PathBuf, bool)>> {
    let Some(path) = NODES.iter().map(PathBuf::from).find(|path| path.exists()) else {
        return Ok(None);
    };
    let product = std::env::var("PRODUCT").unwrap_or_default();
    let inverted = match product.as_str() {
        "cadmus" | "europa" => false, // Mark 8
        "io" | "nova" | "frost" | "storm" | "goldfinch" | "condor" | "spaBW" | "spaBWTPV" | "spaColour" | "monza" => true,
        // The USB version marker also identifies Libra 2 if Nickel omitted PRODUCT.
        _ if fs::read_to_string("/mnt/onboard/.kobo/version").is_ok_and(|version| version.trim().ends_with("418")) => true,
        _ => return Err(io::Error::new(io::ErrorKind::Unsupported, "Cannot identify this Kobo's natural-light scale")),
    };
    Ok(Some((path, inverted)))
}

pub fn read() -> io::Result<Option<u8>> {
    driver()?.map(|(path, inverted)| read_at(&path, inverted)).transpose()
}

pub fn set(percent: u8) -> io::Result<u8> {
    let (path, inverted) = driver()?.ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "Natural light is not supported by this Kobo"))?;
    set_at(&path, inverted, percent)
}

fn read_at(path: &Path, inverted: bool) -> io::Result<u8> {
    let value = fs::read_to_string(path)?.trim().parse::<u8>().ok().filter(|value| *value <= 10).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid natural-light driver value"))?;
    Ok(if inverted { 10 - value } else { value } * 10)
}

fn set_at(path: &Path, inverted: bool, percent: u8) -> io::Result<u8> {
    if percent > 100 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Natural light must be between 0 and 100"));
    }
    let level = (percent + 5) / 10;
    let level = if inverted { 10 - level } else { level };
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    write!(file, "{level}")?;
    drop(file);
    read_at(path, inverted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn warmth_roundtrips_both_driver_scales_and_quantizes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for inverted in [false, true] {
            for (input, expected) in [(0, 0), (24, 20), (25, 30), (50, 50), (100, 100)] {
                assert_eq!(set_at(file.path(), inverted, input).unwrap(), expected);
                let raw: u8 = fs::read_to_string(file.path()).unwrap().parse().unwrap();
                assert_eq!(raw, if inverted { 10 - expected / 10 } else { expected / 10 });
            }
        }
        assert_eq!(set_at(file.path(), true, 101).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        fs::write(file.path(), "11").unwrap();
        assert_eq!(read_at(file.path(), true).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
    #[test]
    fn missing_driver_is_not_created() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("color");
        assert_eq!(set_at(&missing, true, 50).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!missing.exists());
    }
}
