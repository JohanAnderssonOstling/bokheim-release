//! A book identity survives metadata edits. A PDF carries its first BLAKE3 hash
//! in the file; an M4B derives its identity from the audio itself, so nothing is
//! embedded and the identity can be recomputed from any later revision. Neither
//! is a checksum of a revision's bytes.
use content_address::ContentHash;
use std::io::{self, Read, Seek, SeekFrom};

pub const PDF_KEY: &[u8] = b"BokheimContentHash";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Pdf,
    M4b,
}
fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

pub fn detect(reader: &mut (impl Read + Seek)) -> io::Result<Option<Format>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = [0; 1024];
    let mut count = 0;
    while count < bytes.len() {
        let read = reader.read(&mut bytes[count..])?;
        if read == 0 {
            break;
        }
        count += read;
    }
    reader.seek(SeekFrom::Start(0))?;
    if count >= 8 && &bytes[4..8] == b"ftyp" {
        return Ok(Some(Format::M4b));
    }
    Ok(bytes[..count].windows(5).any(|value| value == b"%PDF-").then_some(Format::Pdf))
}

/// Reuse readable embedded identities and derive M4B identity from audio.
/// Unreadable PDF documents and unfingerprintable audio use byte identity.
/// A readable but malformed identity remains an error.
pub fn read(reader: &mut (impl Read + Seek)) -> io::Result<Option<ContentHash>> {
    let identity = match detect(reader)? {
        Some(Format::Pdf) => {
            let document = pdf_range_reader::document_root(&mut *reader).ok();
            let root_dict = document.as_ref().and_then(|doc| doc.catalog().ok());
            if let Some(root_dict) = root_dict.filter(|root_dict| root_dict.has(PDF_KEY)) {
                let value = root_dict.get(PDF_KEY).and_then(lopdf::Object::as_str).map_err(invalid)?;
                Some(std::str::from_utf8(value).map_err(invalid)?.parse().map_err(invalid)?)
            } else {
                None
            }
        }
        // Tag edits move every byte of an M4B container but leave the encoded
        // audio alone, so the audio is the identity. Nothing is embedded and
        // any revision recomputes the same value.
        Some(Format::M4b) => match audio_fingerprint(&mut *reader) {
            Ok(fingerprint) => Some(ContentHash::new(fingerprint.to_hex().as_str())),
            // Audio that cannot be fingerprinted is audio enrichment refuses to
            // rewrite, so those bytes never change and byte identity holds --
            // the same fallback an unreadable PDF document takes above.
            Err(error) if error.kind() == io::ErrorKind::InvalidData => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    reader.seek(SeekFrom::Start(0))?;
    Ok(identity)
}

pub fn raw_hash(reader: &mut impl Read) -> io::Result<ContentHash> {
    let mut hash = blake3::Hasher::new();
    let mut bytes = vec![0; 1024 * 1024];
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(ContentHash::new(hash.finalize().to_hex().as_str()))
}

/// Recognize identity without requiring the current file bytes to hash to it.
pub fn identify(reader: &mut (impl Read + Seek)) -> io::Result<ContentHash> {
    if let Some(identity) = read(reader)? {
        return Ok(identity);
    }
    reader.seek(SeekFrom::Start(0))?;
    raw_hash(reader)
}

#[cfg(feature = "native-write")]
mod native;
#[cfg(feature = "native-write")]
pub use native::ensure;

#[cfg(all(test, feature = "native-write"))]
mod tests;

mod m4b_audio;
pub use m4b_audio::audio_fingerprint;
