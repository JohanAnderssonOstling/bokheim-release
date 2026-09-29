//! Complete Windows application packages. Authenticate the ZIP against the pinned
//! manifest before calling these functions; extraction never changes installed files.
use std::{collections::BTreeMap, fs::{self, File, OpenOptions}, io::{Read, Write}, path::{Path, PathBuf}};

#[derive(Clone, Debug)]
pub struct Package {
    /// Exact declared expanded bytes, used alongside backups for storage checks.
    pub expanded_bytes: u64,
    pub files: Vec<PathBuf>,
}

fn safe_name(raw: &str) -> Result<String, String> {
    let name = raw.strip_suffix('/').unwrap_or(raw);
    if name.is_empty() || !name.is_ascii() { return Err("invalid Windows package path".into()); }
    for part in name.split('/') {
        let stem = part.split('.').next().unwrap().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if part.is_empty() || part == "." || part == ".." || part.len() > 255 || part.ends_with(['.', ' '])
            || reserved || part.bytes().any(|b| b < 32 || b == 127 || b"\\:<>\"|?*".contains(&b)) {
            return Err(format!("unsafe Windows package path: {raw}"));
        }
    }
    Ok(name.into())
}

fn inspect_archive(archive: &mut zip::ZipArchive<File>) -> Result<Package, String> {
    if archive.len() > 4096 { return Err("too many Windows package entries".into()); }
    let mut names = BTreeMap::new();
    let mut files = Vec::new();
    let mut expanded_bytes = 0u64;
    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = safe_name(file.name())?;
        let kind = file.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(kind, 0 | 0o100000 | 0o040000) || (kind == 0o040000 && !file.is_dir()) {
            return Err("links and special files are forbidden in update packages".into());
        }
        if names.insert(name.to_ascii_lowercase(), file.is_dir()).is_some() {
            return Err("duplicate Windows package path (case insensitive)".into());
        }
        if !file.is_dir() {
            expanded_bytes = expanded_bytes.checked_add(file.size()).ok_or("expanded package size overflow")?;
            files.push(PathBuf::from(name));
        }
    }
    for name in names.keys() {
        let parts: Vec<_> = name.split('/').collect();
        for end in 1..parts.len() {
            if names.get(&parts[..end].join("/")) == Some(&false) {
                return Err("file shadows a Windows package directory".into());
            }
        }
    }
    for required in ["bokheim.exe"] {
        if names.get(required) != Some(&false) { return Err(format!("Windows package lacks {required}")); }
    }
    Ok(Package { expanded_bytes, files })
}

pub fn inspect(source: &Path) -> Result<Package, String> {
    let file = File::open(source).map_err(|e| e.to_string())?;
    inspect_archive(&mut zip::ZipArchive::new(file).map_err(|e| e.to_string())?)
}

/// Destination must not already exist. A failed extraction remains isolated and
/// must never be promoted; the caller can remove it before a fresh attempt.
pub fn unpack(source: &Path, destination: &Path, available_bytes: u64) -> Result<Package, String> {
    let mut archive = zip::ZipArchive::new(File::open(source).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let package = inspect_archive(&mut archive)?;
    if package.expanded_bytes > available_bytes {
        return Err(format!("Windows package needs {} additional bytes", package.expanded_bytes - available_bytes));
    }
    fs::create_dir(destination).map_err(|e| e.to_string())?;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
        let path = destination.join(safe_name(file.name())?);
        if file.is_dir() { fs::create_dir_all(&path).map_err(|e| e.to_string())?; continue; }
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
        let size = file.size();
        let mut remaining = size;
        let mut buffer = [0u8; 65536];
        loop {
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 { break; }
            remaining = remaining.checked_sub(n as u64).ok_or("package entry exceeds declared size")?;
            output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        }
        if remaining != 0 { return Err("truncated Windows package entry".into()); }
        output.sync_all().map_err(|e| e.to_string())?;
    }
    // Flush nested directory entries before the host writes its activation journal.
    for path in &package.files {
        for parent in destination.join(path).ancestors().skip(1) {
            if parent.starts_with(destination) { crate::native_files::sync_dir(parent)?; }
        }
    }
    crate::native_files::sync_dir(destination.parent().ok_or("package directory has no parent")?)?;
    Ok(package)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package(path: &Path, names: &[&str]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for name in names {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"payload").unwrap();
        }
        zip.finish().unwrap();
    }
    #[test]
    fn expands_complete_package_into_fresh_directory_with_exact_size() {
        let temp = tempfile::tempdir().unwrap(); let source = temp.path().join("app.zip");
        package(&source, &["Bokheim.exe", "Bokheim.ico"]);
        assert_eq!(inspect(&source).unwrap().expanded_bytes, 14);
        let dest = temp.path().join("candidate");
        assert!(unpack(&source, &dest, 13).is_err()); assert!(!dest.exists());
        unpack(&source, &dest, 14).unwrap();
        assert_eq!(fs::read(dest.join("Bokheim.ico")).unwrap(), b"payload");
        assert!(unpack(&source, &dest, 14).is_err());
    }
    #[test]
    fn rejects_windows_traversal_devices_and_aliases() {
        for name in ["../Bokheim.exe", "/app", "C:/app", "pdfium\\file", "file:stream", "NUL.txt", "com1", "x/LPT9.log", "app.", "app ", "x//y", "x/./y"] {
            assert!(safe_name(name).is_err(), "{name}");
        }
        let temp = tempfile::tempdir().unwrap(); let source = temp.path().join("app.zip");
        for names in [vec!["Bokheim.ico"], vec!["Bokheim.exe", "bokheim.EXE", "Bokheim.ico"], vec!["Bokheim.exe", "assets", "assets/icon"]] {
            package(&source, &names); assert!(inspect(&source).is_err());
        }
    }
}
