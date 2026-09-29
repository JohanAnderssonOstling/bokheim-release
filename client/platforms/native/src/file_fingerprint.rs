//! Native file observation shared by ingestion and local reader opening.
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{fs, time::UNIX_EPOCH};
pub fn file_metadata_fingerprint(metadata: &fs::Metadata) -> std::io::Result<u64> {
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&modified.as_nanos().to_le_bytes());
    hasher.update(&metadata.len().to_le_bytes());
    #[cfg(unix)]
    {
        hasher.update(&metadata.ino().to_le_bytes());
        hasher.update(&metadata.dev().to_le_bytes());
        // A copier can replace bytes in place and restore the original mtime.
        // Unix ctime still changes, so the scanner must inspect that file again.
        hasher.update(&metadata.ctime().to_le_bytes());
        hasher.update(&metadata.ctime_nsec().to_le_bytes());
    }
    Ok(u64::from_le_bytes(hasher.finalize().as_bytes()[..8].try_into().expect("BLAKE3 digest contains eight bytes")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn detects_in_place_replacement_with_restored_mtime() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), b"first").unwrap();
        let before = fs::metadata(file.path()).unwrap();
        let fingerprint = file_metadata_fingerprint(&before).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(file.path(), b"other").unwrap();
        fs::File::options().write(true).open(file.path()).unwrap().set_modified(before.modified().unwrap()).unwrap();
        let after = fs::metadata(file.path()).unwrap();
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(before.ino(), after.ino());
        assert_ne!(fingerprint, file_metadata_fingerprint(&after).unwrap());
    }
}
