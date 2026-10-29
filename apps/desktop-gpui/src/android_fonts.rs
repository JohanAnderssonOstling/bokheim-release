//! Register readable Android fonts on a background worker.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use gpui::PlatformTextSystem;

pub(crate) fn load(text_system: &dyn PlatformTextSystem) {
    let directories = ["/system/fonts", "/product/fonts", "/system_ext/fonts", "/vendor/fonts"].map(Path::new);
    for path in font_files(&directories) {
        let result = fs::read(&path).map_err(|error| error.to_string()).and_then(|bytes| text_system.add_fonts(vec![Cow::Owned(bytes)]).map_err(|error| error.to_string()));
        if let Err(error) = result {
            log::warn!("Could not load system font {}: {error}", path.display());
        }
    }
}

fn font_files(directories: &[&Path]) -> Vec<PathBuf> {
    let mut paths = HashSet::new();
    for directory in directories {
        let Ok(entries) = fs::read_dir(directory) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| ["ttf", "otf", "ttc", "otc"].iter().any(|supported| extension.eq_ignore_ascii_case(supported)))
                && let Ok(path) = path.canonicalize()
                && path.is_file()
            {
                paths.insert(path);
            }
        }
    }
    let mut paths: Vec<_> = paths.into_iter().collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_font_collections_and_vendor_fonts_without_duplicate_files() {
        let root = tempfile::tempdir().unwrap();
        let system = root.path().join("system");
        let vendor = root.path().join("vendor");
        fs::create_dir(&system).unwrap();
        fs::create_dir(&vendor).unwrap();
        for name in ["serif.ttf", "sans.OTF", "cjk.ttc", "collection.otc", "fonts.xml"] {
            fs::write(system.join(name), []).unwrap();
        }
        fs::write(vendor.join("custom.ttf"), []).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(system.join("serif.ttf"), vendor.join("serif-alias.ttf")).unwrap();
        fs::create_dir(system.join("directory.ttf")).unwrap();
        let files = font_files(&[&system, &vendor, &system, &root.path().join("missing")]);
        assert_eq!(files.len(), 5);
        assert!(files.iter().any(|path| path.ends_with("custom.ttf")));
        assert!(files.iter().any(|path| path.ends_with("cjk.ttc")));
        assert!(!files.iter().any(|path| path.ends_with("fonts.xml")));
    }
}
