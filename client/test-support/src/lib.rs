pub use sync_common::fixture_content_hash;
use sync_common::{DirId, LibraryId, ReplicaId, ROOT_DIR_ID};

pub fn fixture_uuid(value: &str) -> uuid::Uuid {
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes[..12].iter_mut().zip(value.bytes()) {
        *target = source;
    }
    let hash = value.bytes().fold(2_166_136_261_u32, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(16_777_619));
    bytes[12..].copy_from_slice(&hash.to_be_bytes());
    uuid::Uuid::from_bytes(bytes)
}

pub fn fixture_dir_id(value: &str) -> DirId {
    if value == ROOT_DIR_ID.to_string() {
        ROOT_DIR_ID
    } else {
        fixture_uuid(value)
    }
}

pub fn fixture_library_id(value: &str) -> LibraryId {
    fixture_uuid(value)
}

pub fn fixture_replica_id(value: &str) -> ReplicaId {
    fixture_uuid(value)
}

#[cfg(feature = "pdf-fixtures")]
mod pdf;
#[cfg(feature = "pdf-fixtures")]
pub use pdf::write_annotated_pdf;
