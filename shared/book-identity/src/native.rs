use super::*;
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

fn unchanged(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev() || before.ino() != after.ino() || before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec() {
            return false;
        }
    }
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

/// Hash original bytes once, then atomically publish the embedded identity.
/// A failed parser/write leaves the user's file untouched.
pub fn ensure(path: &Path) -> io::Result<ContentHash> {
    let mut source = File::open(path)?;
    if detect(&mut source)? != Some(Format::Pdf) {
        return ensure_embedded(path);
    }
    if let Some(identity) = read(&mut source)? {
        return Ok(identity);
    }
    let before = source.metadata()?;
    match ensure_embedded(path) {
        Ok(identity) => Ok(identity),
        Err(error) => {
            // Failed staging never publishes a partial edit. A concurrent change,
            // however, must be retried rather than assigned a stale byte identity.
            if error.kind() == io::ErrorKind::Interrupted {
                return Err(error);
            }
            source.seek(SeekFrom::Start(0))?;
            let identity = raw_hash(&mut source)?;
            let current = fs::symlink_metadata(path)?;
            if current.file_type().is_symlink() || !unchanged(&before, &source.metadata()?) || !unchanged(&before, &current) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "PDF changed while determining byte identity; retry"));
            }
            Ok(identity)
        }
    }
}

fn ensure_embedded(path: &Path) -> io::Result<ContentHash> {
    let mut source = File::open(path)?;
    #[cfg(not(target_os = "android"))]
    fs2::FileExt::lock_exclusive(&source)?;
    if let Some(identity) = read(&mut source)? {
        return Ok(identity);
    }
    // Only a PDF carries a stamped identity. M4B identity comes from the audio,
    // so reaching here means the audio could not be fingerprinted and the file
    // must be left exactly as it is.
    if detect(&mut source)? != Some(Format::Pdf) {
        return raw_hash(&mut source);
    }
    let before = source.metadata()?;
    if before.permissions().readonly() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "cannot embed book identity in a read-only file"));
    }
    let mut staged = tempfile::NamedTempFile::new_in(path.parent().ok_or_else(|| invalid("book has no parent"))?)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        staged.write_all(&buffer[..count])?;
    }
    let identity = ContentHash::new(hasher.finalize().to_hex().as_str());
    append_pdf_identity(staged.as_file_mut(), identity)?;
    if read(staged.as_file_mut())? != Some(identity) {
        return Err(invalid("embedded identity did not round-trip"));
    }
    staged.as_file().set_permissions(before.permissions())?;
    staged.as_file().sync_all()?;
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !unchanged(&before, &source.metadata()?) || !unchanged(&before, &path_metadata) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "book changed while embedding its identity; retry"));
    }
    staged.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(identity)
}

fn append_pdf_identity(file: &mut File, identity: ContentHash) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0; 1024];
    let count = file.read(&mut header)?;
    let base = header[..count].windows(5).position(|bytes| bytes == b"%PDF-").ok_or_else(|| invalid("invalid PDF header"))? as u64;
    file.seek(SeekFrom::Start(0))?;
    let mut document = pdf_range_reader::document_root(&mut *file).map_err(invalid)?;
    let root = document.trailer.get(b"Root").and_then(lopdf::Object::as_reference).map_err(invalid)?;
    let previous = document.xref_start;
    let mut root_dict = document.catalog().map_err(invalid)?.clone();
    root_dict.set(PDF_KEY, lopdf::Object::string_literal(identity.to_string()));
    // Let lopdf escape/serialize the root dictionary and trailer. Only this root is
    // rewritten; original streams and objects stay byte-for-byte in the prefix.
    document.objects.clear();
    let mut root_object = lopdf::Object::Dictionary(root_dict);
    if let Some(state) = &document.encryption_state {
        // Encrypt only the appended root with the original object key.
        // The existing Encrypt dictionary, file IDs and all original bytes stay intact.
        lopdf::encryption::encrypt_object(state, root, &mut root_object).map_err(invalid)?;
    }
    document.objects.insert(root, root_object);
    document.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceTable;
    document.trailer.remove(b"XRefStm");
    document.trailer.set("Prev", previous as i64);
    let mut serialized = Vec::new();
    document.save_to(&mut serialized)?;
    let object_header = format!("{} {} obj", root.0, root.1);
    let object_start = serialized.windows(object_header.len()).position(|part| part == object_header.as_bytes()).ok_or_else(|| invalid("missing serialized PDF root"))?;
    let xref = serialized.windows(5).rposition(|part| part == b"\nxref").ok_or_else(|| invalid("missing serialized PDF xref"))?;
    let trailer = serialized.windows(8).rposition(|part| part == b"trailer\n").ok_or_else(|| invalid("missing serialized PDF trailer"))?;
    let startxref = serialized.windows(9).rposition(|part| part == b"startxref").ok_or_else(|| invalid("missing serialized PDF pointer"))?;
    let offset = file.seek(SeekFrom::End(0))? + 1 - base;
    if offset > 9_999_999_999 {
        return Err(invalid("PDF is too large for an incremental table update"));
    }
    file.write_all(b"\n")?;
    file.write_all(&serialized[object_start..xref + 1])?;
    let xref_offset = file.stream_position()? - base;
    write!(file, "xref\n{} 1\n{offset:010} {:05} n \n", root.0, root.1)?;
    file.write_all(&serialized[trailer..startxref])?;
    write!(file, "startxref\n{xref_offset}\n%%EOF\n")?;
    Ok(())
}
