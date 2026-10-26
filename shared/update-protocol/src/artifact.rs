//! Streaming size and SHA-256 verification for release artifacts.

use crate::{Artifact, decode_hex};
use ring::digest::{Context, SHA256};
use std::io::{self, Read, Write};
use std::{fs::File, path::Path};
use update_files::copy_observed;

pub fn copy_verified(input: impl Read, output: &mut impl Write, artifact: &Artifact) -> Result<(), String> {
    copy_verified_with_progress(input, output, artifact, |_, _| {})
}

pub fn verify_file(path: &Path, artifact: &Artifact) -> Result<(), String> {
    copy_verified(File::open(path).map_err(|e| e.to_string())?, &mut io::sink(), artifact)
}

/// Only publish a staging file after its complete authenticated contents are
/// flushed; all hosts use the same cleanup and atomic replacement behavior.
pub fn stage_verified(input: impl Read, destination: &Path, partial: &Path, artifact: &Artifact, progress: impl FnMut(u64, u64)) -> Result<(), String> {
    artifact.validate()?;
    update_files::with_atomic_file(destination, partial, None, |output| copy_verified_with_progress(input, output, artifact, progress).map_err(io::Error::other)).map_err(|e| e.to_string())
}

pub fn copy_verified_with_progress(input: impl Read, output: &mut impl Write, artifact: &Artifact, mut progress: impl FnMut(u64, u64)) -> Result<(), String> {
    let mut digest = Context::new(&SHA256);
    let total = copy_observed(
        input,
        output,
        |bytes, total| {
            if total > artifact.bytes {
                return Err(io::Error::other("update exceeds published size"));
            }
            digest.update(bytes);
            Ok(())
        },
        |_, total| progress(total, artifact.bytes),
    )
    .map_err(|e| format!("cannot copy update: {e}"))?;
    if total != artifact.bytes {
        return Err("update is shorter than published size".into());
    }
    if digest.finish().as_ref() != decode_hex::<32>(&artifact.sha256)? {
        return Err("update failed SHA-256 verification".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncated_oversized_and_corrupt_artifacts() {
        let artifact = Artifact { url: "https://downloads.example.org/file".into(), bytes: 3, sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into() };
        let mut output = Vec::new();
        copy_verified(&b"abc"[..], &mut output, &artifact).unwrap();
        assert_eq!(output, b"abc");
        for bytes in [&b"ab"[..], &b"abcd"[..], &b"xyz"[..]] {
            assert!(copy_verified(bytes, &mut Vec::new(), &artifact).is_err());
        }
    }
}
