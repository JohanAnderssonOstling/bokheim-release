//! Kobo release metadata and package rules shared by USB and on-device updates.
mod archive;
mod compatibility;
mod release;

pub use archive::{Package, PackageFile, Policy, destination, inspect, sanitize_path, visit};
pub use release::{RELEASE_API, Release, newer_release, newer_release_for_model};
pub use compatibility::{installed_profile, validate_build_info, validate_device};

pub const APP: &str = ".adds/bokheim";
pub const MENU_CONFIGURATION: &str = ".adds/nm/desktop-gpui-kobo";
pub const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
pub const REQUIRED_FILES: &[&str] = &[
    ".adds/bokheim/desktop-gpui-kobo",
    ".adds/bokheim/fbink",
    ".adds/bokheim/run.sh",
    ".adds/bokheim/run-monitored.sh",
    MENU_CONFIGURATION,
    ".adds/bokheim/pdfium/libpdfium.so",
    ".adds/bokheim/runtime/ld-linux-armhf.so.3",
    ".adds/bokheim/runtime/libc.so.6",
    ".adds/bokheim/taxonomy.sqlite3",
];
