//! Conventional desktop locations for device-local application state.
use std::path::PathBuf;

pub fn data_dir() -> Result<PathBuf, String> {
    Ok(dirs::data_dir().ok_or("failed to resolve desktop data dir")?.join("bokheim"))
}

pub fn default_library_root() -> Result<PathBuf, String> {
    Ok(dirs::document_dir().or_else(|| dirs::home_dir().map(|home| home.join("Documents"))).ok_or("could not locate the Documents directory")?.join("Bokheim"))
}

pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}
