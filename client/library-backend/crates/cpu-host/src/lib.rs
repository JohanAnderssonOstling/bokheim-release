//! Platform-neutral CPU contract for library book and thumbnail work.
//!
//! Hosts own execution, transport, and resource lifetimes: native hosts run
//! work on the blocking executor, browser hosts forward it to the CPU worker.
//! Everything in this crate is portable. `client-platform-native` and
//! `client-platform-web` provide the hosts; callers hold explicit host
//! handles, and launchers may override the process-wide default.

use content_address::BookId;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::sync::OnceLock;

use client_platform_runtime::executor::BoxedBackendFuture;
pub use client_runtime::BackendError;
use std::sync::Arc;
pub use update_protocol::Artifact;

/// Byte reader handed to CPU work. A single trait keeps the object type legal;
/// holders of book-access readers convert them with [`as_cpu_reader`].
pub trait CpuReader: Read + Seek + Send + Sync + 'static {}
impl<T: Read + Seek + Send + Sync + 'static> CpuReader for T {}
pub type BoxedCpuReader = Box<dyn CpuReader>;

/// Adapt an owned book reader (for example a book-access reader) for CPU work.
/// The opened reader owns its bytes; callers must not hold placement leases
/// across the conversion.
pub fn as_cpu_reader<R: Read + Seek + Send + Sync + 'static>(reader: R) -> BoxedCpuReader {
    Box::new(reader)
}

/// Metadata and cover parsers request ranges from the storage owner.
#[derive(Serialize, Deserialize)]
pub enum BookTask {
    Inspect { source_name: String, format: book_model::BookFormat, content_hash: Option<BookId> },
    Thumbnail { extension: String },
}

#[derive(Serialize, Deserialize)]
pub enum CpuResponse {
    Done,
    Inspection(book_inspection::InspectedBook),
    Hash(BookId),
    Thumbnail(Option<ThumbnailBytes>),
    Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
}

#[derive(Serialize, Deserialize)]
pub struct ThumbnailBytes {
    #[serde(with = "serde_bytes")]
    pub browse: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub card_high_density: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub high_density: Vec<u8>,
}

impl From<thumbnail::ThumbnailVersions> for ThumbnailBytes {
    fn from(versions: thumbnail::ThumbnailVersions) -> Self {
        Self { browse: versions.browse, card_high_density: versions.card_high_density, high_density: versions.high_density }
    }
}

pub fn execute(request: CpuRequest) -> Result<CpuResponse, BackendError> {
    match request {
        CpuRequest::Book { .. } => Err("book request requires a range source".into()),
        CpuRequest::HashStart { key } => HASHERS.with(|hashers| {
            let mut hashers = hashers.borrow_mut();
            if hashers.len() >= 32 || hashers.contains_key(&key) {
                return Err("hash session unavailable; retry the job".into());
            }
            hashers.insert(key, blake3::Hasher::new());
            Ok(CpuResponse::Done)
        }),
        CpuRequest::HashUpdate { key, bytes } => HASHERS.with(|hashers| {
            let mut hashers = hashers.borrow_mut();
            let hasher = hashers.get_mut(&key).ok_or("hash session was lost; retry the job")?;
            hasher.update(&bytes);
            Ok(CpuResponse::Done)
        }),
        CpuRequest::HashFinish { key } => HASHERS.with(|hashers| {
            let hasher = hashers.borrow_mut().remove(&key).ok_or("hash session was lost; retry the job")?;
            Ok(CpuResponse::Hash(BookId::from(hasher.finalize())))
        }),
        CpuRequest::HashAbort { key } => {
            HASHERS.with(|hashers| hashers.borrow_mut().remove(&key));
            Ok(CpuResponse::Done)
        }
        CpuRequest::Cover { bytes } => {
            let versions = cover_thumbnails(&bytes).map_err(BackendError::operation)?;
            Ok(CpuResponse::Thumbnail(Some(versions.into())))
        }
        CpuRequest::Resize { width, bytes } => {
            // The worker accepts only the product's supported display buckets.
            if ![thumbnail::BROWSE_THUMBNAIL_WIDTH, thumbnail::THUMBNAIL_WIDTH].contains(&width) {
                return Err("unsupported thumbnail width".into());
            }
            resize_cover(&bytes, width).map(CpuResponse::Bytes).map_err(BackendError::operation)
        }
    }
}

/// Both thumbnail sizes for an uploaded cover image.
fn cover_thumbnails(bytes: &[u8]) -> thumbnail::ThumbnailResult<thumbnail::ThumbnailVersions> {
    let format = image::guess_format(bytes)?;
    thumbnail::ThumbnailVersions::from_image(&thumbnail::decode_cover_image(bytes, format)?)
}

/// A card derivative cropped and transformed at one thumbnail width.
fn resize_cover(bytes: &[u8], width: u32) -> thumbnail::ThumbnailResult<Vec<u8>> {
    let format = image::guess_format(bytes)?;
    thumbnail::encode_card_thumbnail_at_width(&thumbnail::decode_cover_image(bytes, format)?, width)
}

thread_local! {
    static HASHERS: std::cell::RefCell<std::collections::HashMap<String, blake3::Hasher>> = Default::default();
}

pub fn execute_book_with_mobi<R: Read + Seek + Send + Sync + 'static>(task: BookTask, mut reader: R, mobi_pages: fn(&Path, &[u8]) -> Result<book_model::BookMetadata, BackendError>) -> Result<CpuResponse, BackendError> {
    match task {
        BookTask::Inspect { source_name, format, content_hash } => {
            let path = Path::new(&source_name);
            if format != book_model::BookFormat::Mobi {
                return book_inspection::inspect_book_reader_with_hash(path, format, reader, content_hash).map(CpuResponse::Inspection).map_err(BackendError::operation);
            }
            // Page evidence joins the file facts the metadata server is asked about.
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map_err(BackendError::operation)?;
            let evidence = mobi_pages(path, &bytes).ok();
            let mut inspected = book_inspection::inspect_book_reader_with_hash(path, format, Cursor::new(bytes), content_hash).map_err(BackendError::operation)?;
            if let Some(evidence) = evidence {
                let book = &mut inspected.metadata.book;
                for identifier in evidence.identifiers {
                    if !book.identifiers.contains(&identifier) {
                        book.identifiers.push(identifier);
                    }
                }
                for subject in evidence.subjects {
                    if !book.subjects.contains(&subject) {
                        book.subjects.push(subject);
                    }
                }
            }
            Ok(CpuResponse::Inspection(inspected))
        }
        BookTask::Thumbnail { extension } => {
            thumbnail::generate_thumbnail_versions_with_pdf_renderer(&extension, reader, thumbnail::render_pdf_cover).map(|versions| CpuResponse::Thumbnail(versions.map(ThumbnailBytes::from))).map_err(BackendError::operation)
        }
    }
}

/// Callers select an operation; only this boundary decodes the wire variant.
pub trait Operation: client_platform_runtime::executor::BackendSend + 'static {
    type Reply;
    fn request(self) -> CpuRequest;
    fn decode(response: CpuResponse) -> Result<Self::Reply, BackendError>;
}
pub trait BookOperation: Operation {
    fn task(self) -> BookTask;
}
macro_rules! operations {
    ($($name:ident { $($(#[$attribute:meta])* $field:ident: $ty:ty),* } => $reply:ty, $pattern:pat => $value:expr;)*) => {
        #[derive(Serialize, Deserialize)]
        pub enum CpuRequest {
            /// `taxonomy` names the snapshot a browser CPU worker loads before
            /// inspecting. None selects the web app's bundled reference;
            /// native hosts share the process taxonomy and also send none.
            Book { task: BookTask, taxonomy: Option<Artifact> },
            $($name { $($(#[$attribute])* $field: $ty),* }),*
        }
        $(pub struct $name { $(pub $field: $ty),* }
        impl Operation for $name {
            type Reply = $reply;
            fn request(self) -> CpuRequest { let Self { $($field),* } = self; CpuRequest::$name { $($field),* } }
            fn decode(response: CpuResponse) -> Result<Self::Reply, BackendError> {
                match response { $pattern => Ok($value), _ => Err(concat!("invalid CPU reply for ", stringify!($name)).into()) }
            }
        }
    )*};
}
operations! {
    HashStart { key: String } => (), CpuResponse::Done => ();
    HashUpdate { key: String, #[serde(with = "serde_bytes")] bytes: Vec<u8> } => (), CpuResponse::Done => ();
    HashFinish { key: String } => BookId, CpuResponse::Hash(hash) => hash;
    HashAbort { key: String } => (), CpuResponse::Done => ();
    Resize { width: u32, #[serde(with = "serde_bytes")] bytes: Vec<u8> } => Vec<u8>, CpuResponse::Bytes(bytes) => bytes;
    Cover { #[serde(with = "serde_bytes")] bytes: Vec<u8> } => ThumbnailBytes, CpuResponse::Thumbnail(Some(versions)) => versions;
}
macro_rules! book_operations {
    ($($name:ident { $($field:ident: $ty:ty),* } => $reply:ty, $pattern:pat => $value:expr;)*) => {$ (
        pub struct $name { $(pub $field: $ty),* }
        impl BookOperation for $name {
            fn task(self) -> BookTask { let Self { $($field),* } = self; BookTask::$name { $($field),* } }
        }
        impl Operation for $name {
            type Reply = $reply;
            fn request(self) -> CpuRequest { CpuRequest::Book { task: self.task(), taxonomy: None } }
            fn decode(response: CpuResponse) -> Result<Self::Reply, BackendError> {
                match response { $pattern => Ok($value), _ => Err(concat!("invalid CPU reply for ", stringify!($name)).into()) }
            }
        }
    )*};
}
book_operations! {
    Inspect { source_name: String, format: book_model::BookFormat, content_hash: Option<BookId> } => book_inspection::InspectedBook, CpuResponse::Inspection(inspection) => inspection;
    Thumbnail { extension: String } => Option<ThumbnailBytes>, CpuResponse::Thumbnail(versions) => versions;
}

/// Execution host. Native hosts run work on the blocking executor; browser
/// hosts forward it to the CPU worker. The trait stays object-safe so
/// processes share one host; typed submissions are free functions below.
pub trait CpuHost: Send + Sync + 'static {
    fn book(&self, task: BookTask, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>>;
    fn request(&self, request: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>>;
    fn generate_thumbnail(&self, extension: String, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailBytes>, BackendError>> {
        Box::pin(async move { Thumbnail::decode(self.book(BookTask::Thumbnail { extension }, reader).await?) })
    }
}

pub async fn submit<O: Operation>(host: &dyn CpuHost, operation: O) -> Result<O::Reply, BackendError> {
    O::decode(host.request(operation.request()).await?)
}

pub async fn submit_book<B: BookOperation>(host: &dyn CpuHost, operation: B, reader: BoxedCpuReader) -> Result<B::Reply, BackendError> {
    B::decode(host.book(operation.task(), reader).await?)
}

pub async fn cover(host: &dyn CpuHost, bytes: Vec<u8>) -> Result<ThumbnailBytes, BackendError> {
    submit(host, Cover { bytes }).await
}

pub async fn resize(host: &dyn CpuHost, bytes: Vec<u8>, width: u32) -> Result<Vec<u8>, BackendError> {
    submit(host, Resize { bytes, width }).await
}

/// The CPU host is a process resource, like the executor: every library
/// session in the process shares it. Launchers override it with
/// [`set_cpu_host`]; otherwise the first caller installs the default.
pub static HOST: OnceLock<Arc<dyn CpuHost>> = OnceLock::new();

/// Override the process-wide host. Fails when a host is already installed.
pub fn set_cpu_host(host: Arc<dyn CpuHost>) -> Result<(), Arc<dyn CpuHost>> {
    HOST.set(host)
}

/// Remote BLAKE3 session over any host. Dropping an unfinished session
/// releases the worker-side hasher.
pub struct RemoteHashSession {
    host: Arc<dyn CpuHost>,
    key: Option<String>,
}
impl RemoteHashSession {
    pub async fn new(host: &Arc<dyn CpuHost>) -> Result<Self, BackendError> {
        let key = uuid::Uuid::new_v4().to_string();
        submit(&**host, HashStart { key: key.clone() }).await?;
        Ok(Self { host: host.clone(), key: Some(key) })
    }
    pub async fn update(&mut self, bytes: &[u8]) -> Result<(), BackendError> {
        let key = self.key.clone().ok_or("hash session finished")?;
        for chunk in bytes.chunks(256 * 1024) {
            submit(&*self.host, HashUpdate { key: key.clone(), bytes: chunk.to_vec() }).await?;
        }
        Ok(())
    }
    pub async fn finish(mut self) -> Result<BookId, BackendError> {
        let key = self.key.clone().ok_or("hash session finished")?;
        let result = submit(&*self.host, HashFinish { key }).await;
        self.key = None;
        result
    }
}
impl Drop for RemoteHashSession {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let host = self.host.clone();
            client_platform_runtime::executor::spawn_detached(async move {
                let _ = submit(&*host, HashAbort { key }).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(request: CpuRequest) -> CpuResponse {
        let bytes = rmp_serde::to_vec_named(&request).unwrap();
        let response = execute(rmp_serde::from_slice(&bytes).unwrap()).unwrap();
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&response).unwrap()).unwrap()
    }

    #[test]
    fn hashing_worker_returns_digest_bytes() {
        let key = "raw-digest-test".to_owned();
        assert!(matches!(roundtrip(CpuRequest::HashStart { key: key.clone() }), CpuResponse::Done));
        roundtrip(CpuRequest::HashUpdate { key: key.clone(), bytes: b"book bytes".to_vec() });
        let response = roundtrip(CpuRequest::HashFinish { key });
        let CpuResponse::Hash(hash) = &response else { panic!("expected hash") };
        assert_eq!(*hash, BookId::from(blake3::hash(b"book bytes")));
        let encoded = rmp_serde::to_vec_named(&response).unwrap();
        assert!(encoded.windows(32).any(|bytes| bytes == hash.as_bytes()));
        assert!(!encoded.windows(64).any(|bytes| bytes == hash.to_hex().as_bytes()));
    }

    #[test]
    fn cover_worker_transports_both_adjusted_card_densities_and_the_original_master() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 400, image::Rgb([120, 40, 20]))).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let CpuResponse::Thumbnail(Some(versions)) = roundtrip(CpuRequest::Cover { bytes: png.into_inner() }) else { panic!("expected thumbnail versions") };
        assert_eq!(image::load_from_memory(&versions.browse).unwrap().to_rgb8().dimensions(), (300, 387));
        assert_eq!(image::load_from_memory(&versions.card_high_density).unwrap().to_rgb8().dimensions(), (600, 774));
        assert_eq!(image::load_from_memory(&versions.high_density).unwrap().to_rgb8().dimensions(), (600, 600));
    }

    #[test]
    fn resize_transports_a_real_jpeg_at_the_requested_display_size() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 600, image::Rgb([120, 40, 20]))).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let CpuResponse::Bytes(bytes) = roundtrip(CpuRequest::Resize { width: 300, bytes: png.into_inner() }) else { panic!("expected thumbnail") };
        assert_eq!(image::guess_format(&bytes).unwrap(), image::ImageFormat::Jpeg);
        assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgb8().dimensions(), (300, 480));
    }
}
