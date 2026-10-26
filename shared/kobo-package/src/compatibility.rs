//! Package/device compatibility is checked before USB writes and update staging.
use crate::{Policy, visit};
use kobo_profile::{Profile, firmware_supported};
use std::io::{self, Read};

pub fn installed_profile(marker: &str) -> io::Result<&'static Profile> {
    let firmware = marker.trim().split(',').nth(2).unwrap_or_default();
    if !firmware_supported(firmware) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "Bokheim requires Kobo firmware 4.6 or later in the 4.x series"));
    }
    Profile::detect("", marker).map_err(|error| io::Error::new(io::ErrorKind::Unsupported, error))
}

/// Existing packages without a device manifest are Libra 2 builds. Never infer
/// broad hardware support merely from an ARM ELF header or a release filename.
pub fn validate_device(input: impl Read, policy: Policy, profile: &Profile) -> io::Result<()> {
    let mut info = None;
    visit(input, policy, |entry, path| {
        if path == std::path::Path::new(".adds/bokheim/BUILD-INFO.txt") {
            if info.is_some() || !entry.header().entry_type().is_file() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid Kobo package build information"));
            }
            let mut text = String::new();
            entry.take(8193).read_to_string(&mut text)?;
            if text.len() > 8192 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "Kobo package build information is too large"));
            }
            info = Some(text);
        }
        Ok(())
    })?;
    validate_build_info(info.as_deref().unwrap_or_default(), profile)
}

pub fn validate_build_info(info: &str, profile: &Profile) -> io::Result<()> {
    let mut declarations = info.lines().filter_map(|line| line.strip_prefix("Supported model IDs: "));
    let Some(models) = declarations.next() else {
        return if profile.id == 388 { Ok(()) } else { Err(io::Error::new(io::ErrorKind::Unsupported, format!("This older package targets Kobo Libra 2. A multi-device package is required for {}.", profile.name))) };
    };
    if declarations.next().is_some() || !info.lines().any(|line| line == "CPU profile: generic") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "The Kobo package has inconsistent device compatibility information"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for id in models.split(',') {
        let id = id.trim().parse::<u16>().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid Kobo package model identifier"))?;
        if Profile::from_id(id).is_none() || !ids.insert(id) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Unknown or repeated Kobo package model identifier"));
        }
    }
    if !ids.contains(&profile.id) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, format!("This Kobo package does not support {}", profile.name)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn libra_legacy_packages_cannot_be_installed_on_other_models() {
        assert!(validate_build_info("CPU profile: libra2\n", Profile::from_id(388).unwrap()).is_ok());
        assert!(validate_build_info("CPU profile: libra2\n", Profile::from_id(376).unwrap()).is_err());
        assert!(validate_build_info("CPU profile: generic\nSupported model IDs: 376,388\n", Profile::from_id(376).unwrap()).is_ok());
        assert!(validate_build_info("CPU profile: libra2\nSupported model IDs: 376,388\n", Profile::from_id(376).unwrap()).is_err());
        assert!(validate_build_info("CPU profile: generic\nSupported model IDs: 999,388\n", Profile::from_id(388).unwrap()).is_err());
        assert!(validate_build_info("CPU profile: generic\nSupported model IDs: 388\n", Profile::from_id(376).unwrap()).is_err());
    }
}
