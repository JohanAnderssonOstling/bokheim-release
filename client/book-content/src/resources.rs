//! Prepared bytes shared by text reading preparation.

use crate::resource_path::{is_container_resource, local_resource_uri, resolve_resource_href, resource_entry};
use std::collections::HashMap;
use std::io;

pub(crate) const MAX_BINARY_RESOURCE_BYTES: u64 = 24 * 1024 * 1024;
pub(crate) const MAX_PREPARED_TEXT_BYTES: u64 = 64 * 1024 * 1024;

pub struct PreparedResources {
    bytes: HashMap<String, Vec<u8>>,
    // EPUB media types must come from the manifest. MOBI has no manifest.
    media_types: Option<HashMap<String, String>>,
    documents: Vec<String>,
}

impl PreparedResources {
    pub fn documents(&self) -> &[String] {
        &self.documents
    }

    pub(crate) fn new(bytes: HashMap<String, Vec<u8>>, media_types: Option<HashMap<String, String>>, documents: Vec<String>) -> Self {
        Self { bytes, media_types, documents }
    }

    pub fn read_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        self.bytes(uri).map(<[u8]>::to_vec)
    }

    fn bytes(&self, uri: &str) -> io::Result<&[u8]> {
        let uri = local_resource_uri("", uri)?;
        resource_entry(&self.bytes, &uri).map(Vec::as_slice).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "book resource was not prepared"))
    }

    pub(crate) fn read_prefix(&self, uri: &str, max_bytes: u64) -> io::Result<Vec<u8>> {
        let bytes = self.bytes(uri)?;
        let limit = usize::try_from(max_bytes).unwrap_or(usize::MAX).min(bytes.len());
        Ok(bytes[..limit].to_vec())
    }

    pub fn media_type(&self, uri: &str) -> io::Result<Option<String>> {
        let uri = local_resource_uri("", uri)?;
        let media_type = self.media_types.as_ref().map(|types| resource_entry(types, &uri).map(String::as_str).unwrap_or("application/octet-stream").to_owned());
        Ok(media_type)
    }

    pub fn exists(&self, uri: &str) -> bool {
        is_container_resource(uri) && resource_entry(&self.bytes, &resolve_resource_href("", uri)).is_some()
    }

    pub fn resolve(&self, base: &str, href: &str) -> String {
        resolve_resource_href(base, href)
    }
}
