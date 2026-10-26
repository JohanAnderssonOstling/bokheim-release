use crate::{MAX_UNPACKED_BYTES, MENU_CONFIGURATION, REQUIRED_FILES};
use flate2::read::GzDecoder;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use tar::EntryType;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Accept older installation packages, while always preserving user data.
    Usb,
    /// Only replace known program files; require the firmware recovery helper.
    OnDevice,
}

pub struct PackageFile {
    pub path: PathBuf,
    pub bytes: u64,
}

pub struct Package {
    pub files: Vec<PackageFile>,
    pub unpacked_bytes: u64,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn sanitize_path(path: &Path, kind: EntryType, policy: Policy) -> io::Result<PathBuf> {
    if !kind.is_file() && !kind.is_dir() {
        return Err(invalid("The Kobo package contains an unsupported archive entry"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| invalid("The Kobo package contains an invalid filename"))?;
                if part.contains('\\') || part.contains(':') {
                    return Err(invalid("The Kobo package contains an unsafe filename"));
                }
                if policy == Policy::OnDevice && (part.ends_with('.') || part.ends_with(".bokheim-update-tmp") || part.ends_with(".bokheim-recovery-tmp") || !part.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"._-".contains(&ch))) {
                    return Err(invalid("The Kobo package contains an unsupported filename"));
                }
                parts.push(part);
            }
            _ => return Err(invalid(format!("Unsafe path in Kobo package: {}", path.display()))),
        }
    }
    if parts.is_empty() {
        return if kind.is_dir() { Ok(PathBuf::new()) } else { Err(invalid("The Kobo package contains an invalid file")) };
    }
    let name = parts.join("/");
    let key = name.to_ascii_lowercase();
    let within = |directory: &str| key == directory || key.strip_prefix(directory).is_some_and(|suffix| suffix.starts_with('/'));
    let directories = [".adds/bokheim/data", ".adds/bokheim/libraries", ".adds/bokheim/remote-book-cache", ".adds/bokheim/taxonomy", ".adds/bokheim/.update"];
    let files =
        ["app-state-v1.sqlite3", "app-state-v1.sqlite3-wal", "app-state-v1.sqlite3-shm", "browsing_preferences.json", "detached-libraries.json", "libraries.csv", "reader_preferences.json", "session.json", "transfer_preferences.json"];
    if directories.iter().any(|directory| within(directory)) || files.iter().any(|file| key == format!(".adds/bokheim/{file}")) || (within("bokheim/libraries") && (key != "bokheim/libraries" || !kind.is_dir())) {
        return Err(invalid(format!("Kobo package attempts to overwrite user data: {name}")));
    }
    if policy == Policy::Usb {
        if parts[0] != ".adds" && parts[0] != "Bokheim" {
            return Err(invalid(format!("Unexpected path in Kobo package: {name}")));
        }
    } else {
        if kind.is_dir() && matches!(name.as_str(), ".adds" | ".adds/bokheim" | ".adds/nm" | "Bokheim" | "Bokheim/Libraries") {
            return Ok(PathBuf::new());
        }
        if name != MENU_CONFIGURATION {
            let relative = name.strip_prefix(".adds/bokheim/").ok_or_else(|| invalid(format!("The update cannot replace {name}")))?;
            let top = relative.split('/').next().unwrap_or_default();
            let directory = matches!(top, "runtime" | "pdfium" | "third-party-source" | "command-shims" | "nickelmenu-install" | "nickelmenu-icons");
            let file = matches!(
                top,
                "desktop-gpui-kobo" | "fbink" | "run.sh" | "run-monitored.sh" | "monitor-nickel.sh" | "wifi-power.sh" | "command-timeout.sh" | "probe-kobo-audio.sh" | "update-recovery.sh" | "BUILD-INFO.txt" | "taxonomy.sqlite3"
            ) || top.starts_with("LICENSE-");
            if !directory && (!file || relative.contains('/') || kind.is_dir()) {
                return Err(invalid(format!("The update cannot replace {name}")));
            }
        }
    }
    Ok(parts.iter().collect())
}

/// Check every existing ancestor, so installation cannot follow a symlink out
/// of the mounted Kobo or replace a directory with a package file.
pub fn destination(root: &Path, relative: &Path, directory: bool) -> io::Result<PathBuf> {
    let mut target = root.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        if !matches!(part, Component::Normal(_)) {
            return Err(invalid("Unsafe installation path"));
        }
        target.push(part);
        match fs::symlink_metadata(&target) {
            Ok(metadata) => {
                let expects_directory = directory || parts.peek().is_some();
                if metadata.file_type().is_symlink() || (expects_directory && !metadata.is_dir()) || (!expects_directory && !metadata.is_file()) {
                    return Err(invalid(format!("The installation destination has an incompatible file: {}", target.display())));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(target)
}

/// Walk normalized regular files and directories using one set of archive
/// rules for validation, USB installation, and on-device staging.
pub fn visit<R: Read>(input: R, policy: Policy, mut entry: impl FnMut(&mut tar::Entry<'_, GzDecoder<R>>, &Path) -> io::Result<()>) -> io::Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(input));
    for item in archive.entries()? {
        let mut item = item?;
        let path = sanitize_path(&item.path()?, item.header().entry_type(), policy)?;
        if !path.as_os_str().is_empty() {
            entry(&mut item, &path)?;
        }
    }
    Ok(())
}

pub fn inspect(input: impl Read, policy: Policy, root: Option<&Path>) -> io::Result<Package> {
    let mut package = Package { files: Vec::new(), unpacked_bytes: 0 };
    let mut unique = BTreeSet::new();
    visit(input, policy, |entry, path| {
        let kind = entry.header().entry_type();
        if let Some(root) = root {
            destination(root, path, kind.is_dir())?;
        }
        if kind.is_dir() {
            return Ok(());
        }
        let name = path.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        if !unique.insert(name.to_ascii_lowercase()) {
            return Err(invalid("The Kobo package contains duplicate filenames"));
        }
        let bytes = entry.header().size()?;
        package.unpacked_bytes = package.unpacked_bytes.checked_add(bytes).filter(|total| *total <= MAX_UNPACKED_BYTES).ok_or_else(|| invalid("The unpacked Kobo package is unexpectedly large"))?;
        package.files.push(PackageFile { path: path.into(), bytes });
        if package.files.len() > 4096 {
            return Err(invalid("The Kobo package contains too many files"));
        }
        Ok(())
    })?;
    let extra = if policy == Policy::OnDevice { &[".adds/bokheim/update-recovery.sh", ".adds/bokheim/BUILD-INFO.txt"][..] } else { &[] };
    for required in REQUIRED_FILES.iter().chain(extra) {
        if !unique.contains(&required.to_ascii_lowercase()) {
            return Err(invalid(format!("The Kobo package is incomplete: {required} is missing")));
        }
    }
    Ok(package)
}
