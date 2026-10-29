//! Bounded OPFS writes retain provider identity independently of byte digests.

use super::{imp, AssetStore, AssetStoreError, StagedBook};
use crate::hashing::HashSession;
use client_platform_web::web_storage::{self, FileWriter, PublishedFile};
use sync_common::storage::ProviderFile;

pub struct BrowserProviderWriter {
    store: AssetStore,
    file: ProviderFile,
    writer: FileWriter,
    hasher: Option<Box<dyn HashSession>>,
}

impl AssetStore {
    /// Stream a storage copy directly into its destination, without buffering
    /// the whole book or changing the provider's file or source cache.
    pub async fn stage_transfer_chunks<F, Fut>(&self, length: u64, expected: Option<content_address::ContentHash>, mut read: F) -> Result<StagedBook, AssetStoreError>
    where
        F: FnMut(u64, usize) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, AssetStoreError>>,
    {
        if length == 0 {
            return Err(AssetStoreError::operation("book is empty"));
        }
        let mut writer = imp::StagingWriter::new(&self.0).await?;
        let mut offset = 0;
        while offset < length {
            let count = (length - offset).min(256 * 1024) as usize;
            let bytes = read(offset, count).await?;
            if bytes.len() != count {
                return Err(AssetStoreError::operation("transferred book ended unexpectedly"));
            }
            writer.write_all(&bytes).await?;
            offset += count as u64;
        }
        Ok(StagedBook { inner: writer.finish(&self.0, expected).await? })
    }

    pub async fn begin_provider_write(&self, file: &ProviderFile) -> Result<BrowserProviderWriter, AssetStoreError> {
        file.validate().map_err(AssetStoreError::operation)?;
        let writer = web_storage::begin_write(&imp::name(&self.0, "book/staged".into())).await.map_err(AssetStoreError::operation)?;
        let hasher = if file.content_hash.is_some() { Some(self.0.hash_service.start().await?) } else { None };
        Ok(BrowserProviderWriter { store: self.clone(), file: file.clone(), writer, hasher })
    }
}

impl BrowserProviderWriter {
    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), AssetStoreError> {
        let written = self.writer.length().checked_add(bytes.len() as u64).ok_or_else(|| AssetStoreError::operation("provider download length overflow"))?;
        if written > self.file.size_bytes {
            return Err(AssetStoreError::operation("provider download exceeds its declared length"));
        }
        self.writer.write_all(bytes).map_err(AssetStoreError::operation)?;
        if let Some(hasher) = &mut self.hasher {
            hasher.update(bytes).await?;
        }
        Ok(())
    }

    pub async fn publish(mut self) -> Result<PublishedFile, AssetStoreError> {
        if self.writer.length() != self.file.size_bytes {
            return Err(AssetStoreError::operation("provider download ended before its declared length"));
        }
        if let Some(hasher) = self.hasher.take() {
            if hasher.finish().await?.content_hash() != self.file.content_hash {
                return Err(AssetStoreError::operation("provider download checksum does not match"));
            }
        }
        let id = self.file.identity.book_id();
        self.writer.publish_pinned_as(imp::name(&self.store.0, format!("book/{id}")), id).map_err(AssetStoreError::operation)
    }
}
