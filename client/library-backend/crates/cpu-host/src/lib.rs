//! Platform-neutral CPU contract for library book and thumbnail work.
//!
//! Hosts own execution, transport, and resource lifetimes: native hosts run
//! work on the blocking executor, browser hosts forward it to the CPU worker.
//! Everything in this crate is portable. `client-platform-native` and
//! `client-platform-web` provide the hosts; callers hold explicit host
//! handles, and launchers may override the process-wide default.

use serde::{Deserialize, Serialize};

use client_platform_runtime::executor::BoxedBackendFuture;
pub use client_runtime::BackendError;
use std::sync::Arc;

/// Byte reader handed to CPU work. A single trait keeps the object type legal;
/// holders of book-access readers convert them with [`as_cpu_reader`].
pub trait CpuReader: std::io::Read + std::io::Seek + Send + Sync + 'static {}
impl<T: std::io::Read + std::io::Seek + Send + Sync + 'static> CpuReader for T {}
pub type BoxedCpuReader = Box<dyn CpuReader>;

struct HostReader<R>(R);
impl<R: std::io::Read> std::io::Read for HostReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
impl<R: std::io::Seek> std::io::Seek for HostReader<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

/// Adapt an owned book reader (for example a book-access reader) for CPU work.
/// The opened reader owns its bytes; callers must not hold placement leases
/// across the conversion.
pub fn as_cpu_reader<R: std::io::Read + std::io::Seek + Send + Sync + 'static>(reader: R) -> BoxedCpuReader {
    Box::new(HostReader(reader))
}

#[derive(Clone, Copy)]
pub struct InspectionCapabilities {
    pub mobi_pages: bool,
}
impl InspectionCapabilities {
    pub fn supports(self, format: book_model::BookFormat) -> bool {
        use book_model::BookFormat;
        matches!(format, BookFormat::Epub | BookFormat::Pdf) || (self.mobi_pages && format == BookFormat::Mobi)
    }
}

/// Metadata and cover parsers request ranges from the storage owner.
#[derive(Serialize, Deserialize)]
pub enum BookTask {
    Inspect { source_name: String, format: book_model::BookFormat, checksum: Option<content_address::ContentHash> },
    SubjectPages { source_name: String, format: book_model::BookFormat },
    Thumbnail { extension: String },
}

#[derive(Serialize, Deserialize)]
pub enum CpuResponse {
    Done,
    Inspection(book_metadata::InspectedBook),
    SubjectEvidence(book_model::BookMetadata),
    Hash(String),
    Thumbnail(Option<ThumbnailBytes>),
    Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
}

#[derive(Serialize, Deserialize)]
pub struct ThumbnailBytes {
    #[serde(with = "serde_bytes")]
    pub browse: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub high_density: Vec<u8>,
}

impl From<thumbnail::ThumbnailVersions> for ThumbnailBytes {
    fn from(versions: thumbnail::ThumbnailVersions) -> Self {
        Self { browse: versions.browse, high_density: versions.high_density }
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
            Ok(CpuResponse::Hash(hasher.finalize().to_hex().to_string()))
        }),
        CpuRequest::HashAbort { key } => {
            HASHERS.with(|hashers| hashers.borrow_mut().remove(&key));
            Ok(CpuResponse::Done)
        }
        CpuRequest::Cover { bytes } => {
            let versions = thumbnail::generate_thumbnail_versions_from_image_bytes(&bytes).map_err(BackendError::operation)?;
            Ok(CpuResponse::Thumbnail(Some(versions.into())))
        }
        CpuRequest::Resize { width, bytes } => {
            // The worker accepts only the product's supported display buckets.
            if ![thumbnail::BROWSE_THUMBNAIL_WIDTH, thumbnail::THUMBNAIL_WIDTH].contains(&width) {
                return Err("unsupported thumbnail width".into());
            }
            thumbnail::generate_thumbnail_at_width_from_image_bytes(&bytes, width).map(CpuResponse::Bytes).map_err(BackendError::operation)
        }
    }
}

thread_local! {
    static HASHERS: std::cell::RefCell<std::collections::HashMap<String, blake3::Hasher>> = Default::default();
}

/// Worker-side book execution. MOBI page inspection is host-dependent, so this
/// entry point rejects it; native hosts use [`execute_book_with_mobi`].
pub fn execute_book<R: std::io::Read + std::io::Seek + Send + Sync + 'static>(task: BookTask, reader: R) -> Result<CpuResponse, BackendError> {
    execute_book_with_mobi(task, reader, mobi_unsupported)
}

fn mobi_unsupported<R>(_: &std::path::Path, _: R) -> Result<book_model::BookMetadata, BackendError> {
    Err("subject page inspection is not supported for this format".into())
}

/// Native MOBI page inspection, shared by the native host and native workers.
pub fn local_mobi_pages<R: std::io::Read>(path: &std::path::Path, reader: R) -> Result<book_model::BookMetadata, BackendError> {
    book_inspection::inspect_mobi_pages_reader(path, reader).map(|book| book.book).map_err(BackendError::operation)
}

pub fn execute_book_with_mobi<R: std::io::Read + std::io::Seek + Send + Sync + 'static>(task: BookTask, reader: R, mobi_pages: fn(&std::path::Path, R) -> Result<book_model::BookMetadata, BackendError>) -> Result<CpuResponse, BackendError> {
    match task {
        BookTask::Inspect { source_name, format, checksum } => book_metadata::inspect_book_reader_with_checksum(std::path::Path::new(&source_name), format, reader, checksum).map(CpuResponse::Inspection).map_err(BackendError::operation),
        BookTask::SubjectPages { source_name, format } => {
            let path = std::path::Path::new(&source_name);
            let book = match format {
                book_model::BookFormat::Epub => book_inspection::inspect_epub_pages_reader(path, reader),
                book_model::BookFormat::Pdf => book_inspection::inspect_pdf_pages_reader(path, reader).map(|b| b.metadata),
                book_model::BookFormat::Mobi => return mobi_pages(path, reader).map(CpuResponse::SubjectEvidence),
                _ => return Err("subject page inspection is not supported for this format".into()),
            }
            .map_err(BackendError::operation)?;
            Ok(CpuResponse::SubjectEvidence(book.book))
        }
        BookTask::Thumbnail { extension } => thumbnail::generate_thumbnail_versions_from_reader(&extension, reader).map(|versions| CpuResponse::Thumbnail(versions.map(ThumbnailBytes::from))).map_err(BackendError::operation),
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
            Book { task: BookTask },
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
    HashFinish { key: String } => String, CpuResponse::Hash(hash) => hash;
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
            fn request(self) -> CpuRequest { CpuRequest::Book { task: self.task() } }
            fn decode(response: CpuResponse) -> Result<Self::Reply, BackendError> {
                match response { $pattern => Ok($value), _ => Err(concat!("invalid CPU reply for ", stringify!($name)).into()) }
            }
        }
    )*};
}
book_operations! {
    SubjectPages { source_name: String, format: book_model::BookFormat } => book_model::BookMetadata, CpuResponse::SubjectEvidence(evidence) => evidence;
    Inspect { source_name: String, format: book_model::BookFormat, checksum: Option<content_address::ContentHash> } => book_metadata::InspectedBook, CpuResponse::Inspection(inspection) => inspection;
    Thumbnail { extension: String } => Option<ThumbnailBytes>, CpuResponse::Thumbnail(versions) => versions;
}

/// Execution host. Native hosts run work on the blocking executor; browser
/// hosts forward it to the CPU worker. The trait stays object-safe so
/// processes share one host; typed submissions are free functions below.
pub trait CpuHost: Send + Sync + 'static {
    fn capabilities(&self) -> InspectionCapabilities;
    fn book(&self, task: BookTask, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>>;
    fn request(&self, request: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>>;
    fn generate_thumbnail(&self, extension: String, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailBytes>, BackendError>>;
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
static HOST: std::sync::OnceLock<Arc<dyn CpuHost>> = std::sync::OnceLock::new();

/// Override the process-wide host. Fails when a host is already installed.
pub fn set_cpu_host(host: Arc<dyn CpuHost>) -> Result<(), Arc<dyn CpuHost>> {
    HOST.set(host)
}

/// The installed host, installing `default` first when none is set.
pub fn cpu_host_or_init(default: impl FnOnce() -> Arc<dyn CpuHost>) -> Arc<dyn CpuHost> {
    HOST.get_or_init(default).clone()
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
    pub async fn finish(mut self) -> Result<String, BackendError> {
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
    fn generated_requests_preserve_named_fields_and_binary_payloads() {
        let update = HashUpdate { key: "stream".into(), bytes: vec![1, 2, 3] }.request();
        assert_eq!(rmp_serde::to_vec_named(&update).unwrap(), b"\x81\xaaHashUpdate\x82\xa3key\xa6stream\xa5bytes\xc4\x03\x01\x02\x03");
        let resize = Resize { width: 32, bytes: vec![4, 5] }.request();
        assert_eq!(rmp_serde::to_vec_named(&resize).unwrap(), b"\x81\xa6Resize\x82\xa5width\x20\xa5bytes\xc4\x02\x04\x05");
    }

    #[test]
    fn independent_streams_hash_their_own_bytes_and_release_sessions() {
        for key in ["first", "second"] {
            roundtrip(CpuRequest::HashStart { key: key.into() });
        }
        roundtrip(CpuRequest::HashUpdate { key: "first".into(), bytes: b"hello ".to_vec() });
        roundtrip(CpuRequest::HashUpdate { key: "second".into(), bytes: b"another book".to_vec() });
        roundtrip(CpuRequest::HashUpdate { key: "first".into(), bytes: b"world".to_vec() });
        for (key, bytes) in [("first", b"hello world".as_slice()), ("second", b"another book".as_slice())] {
            let CpuResponse::Hash(actual) = roundtrip(CpuRequest::HashFinish { key: key.into() }) else { panic!("expected digest") };
            assert_eq!(actual, blake3::hash(bytes).to_hex().as_str());
            assert!(execute(CpuRequest::HashUpdate { key: key.into(), bytes: Vec::new() }).is_err());
        }
    }

    #[test]
    fn lost_or_cancelled_hash_sessions_cannot_silently_start_again() {
        roundtrip(CpuRequest::HashStart { key: "cancel".into() });
        roundtrip(CpuRequest::HashAbort { key: "cancel".into() });
        assert!(execute(CpuRequest::HashFinish { key: "cancel".into() }).is_err());
        assert!(execute(CpuRequest::HashUpdate { key: "missing".into(), bytes: vec![1] }).is_err());
    }

    #[test]
    fn seekable_book_inspection_matches_direct_inspection() {
        let bytes = include_bytes!("../../../../app/tests/fixtures/storage-contract.epub").to_vec();
        let actual = execute_book(BookTask::Inspect { source_name: "fixture.epub".into(), format: book_model::BookFormat::Epub, checksum: None }, std::io::Cursor::new(bytes.clone())).unwrap();
        let expected = CpuResponse::Inspection(book_metadata::inspect_book_bytes("fixture.epub", book_model::BookFormat::Epub, bytes).unwrap());
        assert_eq!(rmp_serde::to_vec_named(&actual).unwrap(), rmp_serde::to_vec_named(&expected).unwrap());
    }

    #[test]
    fn resize_transports_a_real_jpeg_at_the_requested_display_size() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400, 600, image::Rgb([120, 40, 20]))).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let CpuResponse::Bytes(bytes) = roundtrip(CpuRequest::Resize { width: 300, bytes: png.into_inner() }) else { panic!("expected thumbnail") };
        assert_eq!(image::guess_format(&bytes).unwrap(), image::ImageFormat::Jpeg);
        assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgb8().dimensions(), (300, 450));
    }
}
