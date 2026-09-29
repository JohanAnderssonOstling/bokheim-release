use pdfium_render::prelude::*;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;

#[cfg(not(target_os = "windows"))]
fn library_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("BOKHEIM_PDFIUM_LIBRARY_PATH") {
        return Ok(path.into());
    }
    if cfg!(test) {
        return Ok(env!("PDFIUM_BUILD_LIBRARY_PATH").into());
    }
    if cfg!(target_os = "android") {
        // Android's app linker namespace resolves native libraries packaged in
        // the APK, including libraries loaded directly from an uncompressed APK.
        return Ok(env!("PDFIUM_LIBRARY_NAME").into());
    }
    if cfg!(feature = "packaged-pdfium") {
        return Ok(env!("PDFIUM_PACKAGED_PATH").into());
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let directory = executable.parent().ok_or("executable has no parent directory")?;
    let path = directory.join("pdfium").join(env!("PDFIUM_LIBRARY_NAME"));
    if !path.is_file() {
        return Err(format!("Packaged PDFium is missing: {}. Build/package the application with its PDFium artifacts.", path.display()));
    }
    Ok(path)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn initialize() -> Result<Pdfium, String> {
    let path = library_path()?;
    Pdfium::bind_to_library(&path).map(Pdfium::new).map_err(|error| format!("loading PDFium from {}: {error}", path.display()))
}


#[cfg(target_os = "windows")]
pub(crate) fn initialize() -> Result<Pdfium, String> {
    Pdfium::bind_to_statically_linked_library().map(Pdfium::new)
        .map_err(|error| format!("initializing static PDFium: {error}"))
}
