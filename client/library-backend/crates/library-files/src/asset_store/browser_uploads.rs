//! Browser imports retain one flushed OPFS generation and an atomic manifest.
use super::{AssetStore, AssetStoreError, StagedBook, imp};
use client_platform_web::web_storage::{self, FileReader};
use std::io::{Read, Seek, SeekFrom};
use sync_common::BookId;
use uuid::Uuid;

fn name(store: &AssetStore, id: Uuid) -> String {
    imp::name(&store.0, format!("book/pending-{id}"))
}
impl StagedBook {
    pub async fn retain_provider_upload(self, store: AssetStore, id: Uuid, metadata: Vec<u8>) -> Result<(), AssetStoreError> {
        if self.content_hash().is_external() {
            return Err(AssetStoreError::operation("pending uploads require a byte checksum"));
        }
        self.inner.writer.retain_provider_upload(name(&store, id), self.inner.content_hash, metadata).map_err(AssetStoreError::operation)
    }
}
impl AssetStore {
    pub fn provider_upload_metadata(&self, id: Uuid) -> Result<Option<Vec<u8>>, AssetStoreError> {
        Ok(self.retained_provider_uploads()?.into_iter().find_map(|(candidate, bytes)| (candidate == id).then_some(bytes)))
    }
    pub fn replace_provider_upload_metadata(&self, id: Uuid, previous: &[u8], metadata: &[u8]) -> Result<(), AssetStoreError> {
        web_storage::replace_provider_upload_metadata(&name(self, id), previous, metadata).map_err(AssetStoreError::operation)
    }

    pub fn retained_provider_uploads(&self) -> Result<Vec<(Uuid, Vec<u8>)>, AssetStoreError> {
        let prefix = imp::name(&self.0, "book/pending-".into());
        web_storage::retained_provider_uploads(&prefix)
            .map_err(AssetStoreError::operation)?
            .into_iter()
            .map(|(name, bytes)| Ok((name.strip_prefix(&prefix).ok_or_else(|| AssetStoreError::operation("invalid pending upload owner"))?.parse().map_err(AssetStoreError::operation)?, bytes)))
            .collect()
    }
    pub async fn verified_provider_upload(&self, id: Uuid, checksum: BookId, size: u64) -> Result<FileReader, AssetStoreError> {
        let mut reader = web_storage::open_reader(&name(self, id)).await.map_err(AssetStoreError::operation)?.ok_or_else(|| AssetStoreError::operation("pending upload bytes are unavailable"))?;
        if reader.seek(SeekFrom::End(0))? != size {
            return Err(AssetStoreError::operation("pending upload bytes changed size"));
        }
        reader.seek(SeekFrom::Start(0))?;
        let mut hasher = self.0.hash_service.start().await?;
        let mut buffer = vec![0; 256 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]).await?;
        }
        if hasher.finish().await? != checksum {
            return Err(AssetStoreError::operation("pending upload bytes changed"));
        }
        reader.seek(SeekFrom::Start(0))?;
        Ok(reader)
    }
    pub fn promote_provider_upload(&self, upload: Uuid, checksum: BookId, book: BookId, size: u64) -> Result<bool, AssetStoreError> {
        web_storage::promote_provider_upload(&name(self, upload), &imp::name(&self.0, format!("book/{book}")), checksum, book, size).map_err(AssetStoreError::operation)
    }
    pub fn remove_provider_upload(&self, id: Uuid) -> Result<(), AssetStoreError> {
        web_storage::remove_provider_upload(&name(self, id)).map_err(AssetStoreError::operation)
    }
}
