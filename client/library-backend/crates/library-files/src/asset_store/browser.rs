//! Browser OPFS stores book bytes; folder and trash changes are metadata-only.
use super::AssetStoreError;
use book_access::BoxedBookReader;
use sync_common::ContentHash;

#[derive(Clone, Debug)]
pub(super) struct Handle {
    namespace: String,
    pub(super) hash_service: std::sync::Arc<dyn crate::hashing::HashService>,
}

pub(super) async fn open_verified_reader(handle: &Handle, hash: &ContentHash) -> Result<Option<client_platform_web::web_storage::FileReader>, AssetStoreError> {
    client_platform_web::web_storage::open_reader(&name(handle, format!("book/{hash}"))).await.map_err(AssetStoreError::operation)
}

thread_local! {
    static LEASES: std::cell::RefCell<std::collections::HashMap<String, (usize, bool)>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

pub(super) struct Lease(String);
impl Drop for Lease {
    fn drop(&mut self) {
        LEASES.with(|leases| {
            let mut leases = leases.borrow_mut();
            if let Some(count) = leases.get_mut(&self.0) {
                count.0 -= 1;
                if count.0 == 0 {
                    leases.remove(&self.0);
                }
            }
        });
    }
}

pub(super) fn lease(handle: &Handle, hash: &str, exclusive: bool) -> Result<Option<Lease>, AssetStoreError> {
    let key = name(handle, format!("book/{hash}"));
    LEASES.with(|leases| {
        let mut leases = leases.borrow_mut();
        if leases.get(&key).is_some_and(|(_, held_exclusive)| exclusive || *held_exclusive) {
            return Ok(None);
        }
        let entry = leases.entry(key.clone()).or_insert((0, exclusive));
        entry.0 += 1;
        Ok(Some(Lease(key)))
    })
}

pub(super) fn open(library_locator: &str, hash_service: std::sync::Arc<dyn crate::hashing::HashService>) -> Result<Handle, AssetStoreError> {
    Ok(Handle { namespace: namespace(library_locator)?, hash_service })
}

fn namespace(locator: &str) -> Result<String, AssetStoreError> {
    let id = locator.rsplit('/').next().ok_or_else(|| AssetStoreError::operation("library locator has no id"))?;
    let id = id.parse::<sync_common::LibraryId>().map_err(AssetStoreError::operation)?;
    Ok(format!("assets/{id}"))
}

fn name(handle: &Handle, asset_name: String) -> String {
    format!("{}/{}", handle.namespace, asset_name)
}

pub(super) struct StagedBook {
    writer: client_platform_web::web_storage::FileWriter,
    pub(super) content_hash: String,
    pub(super) checksum: ContentHash,
    pub(super) size_bytes: u64,
    destination: String,
}

pub(super) fn finish_staged_write(handle: &Handle, writer: client_platform_web::web_storage::FileWriter, content_hash: String) -> Result<StagedBook, AssetStoreError> {
    let checksum = ContentHash::new(&content_hash);
    let size_bytes = std::io::Seek::seek(&mut writer.open_reader(), std::io::SeekFrom::End(0))?;
    let content_hash = book_identity::read(&mut writer.open_reader())?.map(|identity| identity.to_string()).unwrap_or(content_hash);
    let destination = name(handle, format!("book/{content_hash}"));
    Ok(StagedBook { writer, content_hash, checksum, size_bytes, destination })
}

pub(super) async fn stage_book(handle: &Handle, source: &mut (dyn std::io::Read + Send)) -> Result<StagedBook, AssetStoreError> {
    let mut writer = client_platform_web::web_storage::begin_write(&name(handle, "book/staged".into())).await.map_err(AssetStoreError::operation)?;
    let mut hasher = handle.hash_service.start().await.map_err(AssetStoreError::operation)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut length = 0_u64;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).map_err(AssetStoreError::operation)?;
        hasher.update(&buffer[..read]).await.map_err(AssetStoreError::operation)?;
        length += read as u64;
    }
    if length == 0 {
        return Err(AssetStoreError::operation("book is empty"));
    }
    let content_hash = hasher.finish().await.map_err(AssetStoreError::operation)?;
    finish_staged_write(handle, writer, content_hash)
}

impl StagedBook {
    pub(super) async fn commit(self) -> Result<(), AssetStoreError> {
        self.writer.publish_verified_as(self.destination, self.checksum).map_err(AssetStoreError::operation)
    }
}

pub(super) fn open_staged_reader(staged: &StagedBook) -> Result<BoxedBookReader, AssetStoreError> {
    Ok(Box::new(staged.writer.open_reader()))
}

pub(super) struct AsyncWriter(client_platform_web::web_storage::FileWriter);

impl AsyncWriter {
    pub(super) fn embedded_identity(&self) -> Result<Option<ContentHash>, AssetStoreError> {
        Ok(book_identity::read(&mut self.0.open_reader())?)
    }
    pub(super) async fn write_all(&mut self, bytes: &[u8]) -> Result<(), AssetStoreError> {
        self.0.write_all(bytes).map_err(AssetStoreError::operation)
    }
    pub(super) async fn commit_verified(self, checksum: ContentHash) -> Result<(), AssetStoreError> {
        self.0.commit_verified(checksum).map_err(AssetStoreError::operation)
    }
}

pub(super) async fn begin_stream_write(handle: &Handle, asset_name: String) -> Result<AsyncWriter, AssetStoreError> {
    client_platform_web::web_storage::begin_write(&name(handle, asset_name)).await.map(AsyncWriter).map_err(AssetStoreError::operation)
}

pub(super) fn write_capacity(_handle: &Handle) -> Result<Option<u64>, AssetStoreError> {
    // OPFS enforces its dynamic quota on each chunk write. No in-memory book buffer.
    Ok(None)
}

pub(super) async fn read(handle: &Handle, asset_name: String) -> Result<Option<Vec<u8>>, AssetStoreError> {
    client_platform_web::web_storage::read_file(&name(handle, asset_name)).await.map_err(AssetStoreError::operation)
}

pub(super) async fn open_reader(handle: &Handle, asset_name: String) -> Result<Option<BoxedBookReader>, AssetStoreError> {
    let name = name(handle, asset_name);
    client_platform_web::web_storage::open_reader(&name).await.map(|reader| reader.map(|reader| Box::new(reader) as BoxedBookReader)).map_err(AssetStoreError::operation)
}

pub(super) async fn write(handle: &Handle, asset_name: String, bytes: &[u8]) -> Result<(), AssetStoreError> {
    client_platform_web::web_storage::write_file(&name(handle, asset_name), bytes).await.map_err(AssetStoreError::operation)
}

pub(super) fn remove(handle: &Handle, asset_name: String) -> Result<bool, AssetStoreError> {
    client_platform_web::web_storage::remove_file(&name(handle, asset_name)).map_err(AssetStoreError::operation)
}

pub(super) fn exists(handle: &Handle, asset_name: String) -> Result<bool, AssetStoreError> {
    client_platform_web::web_storage::file_exists(&name(handle, asset_name)).map_err(AssetStoreError::operation)
}

pub(super) async fn book_bytes_at_handle(handle: &Handle) -> Result<u64, AssetStoreError> {
    client_platform_web::web_storage::total_file_bytes(&format!("{}/book/", handle.namespace)).await.map_err(AssetStoreError::operation)
}
