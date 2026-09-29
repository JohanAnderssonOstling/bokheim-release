//! Browser CPU transport and source lifetimes over the CPU worker.

use super::transport::{cpu_client, range::Server};
use client_platform_runtime::executor::BoxedBackendFuture;
use cpu_host::{BackendError, BookOperation, BookTask, BoxedCpuReader, CpuHost, CpuRequest, CpuResponse, InspectionCapabilities, Operation, ThumbnailBytes};
use std::io::{Read, Seek, SeekFrom};
use wasm_bindgen::JsValue;

#[derive(Default)]
pub struct WebHost;
impl CpuHost for WebHost {
    fn capabilities(&self) -> InspectionCapabilities {
        InspectionCapabilities { mobi_pages: false }
    }
    fn book(&self, task: BookTask, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        Box::pin(async move {
            let server = serve_reader(reader).map_err(BackendError::operation)?;
            let bytes = rmp_serde::to_vec_named(&CpuRequest::Book { task }).map_err(BackendError::operation)?;
            let response = cpu_client::request_with_source(bytes, server.source()).map_err(BackendError::operation)?.await.map_err(BackendError::operation)?;
            rmp_serde::from_slice(&response).map_err(BackendError::operation)?
        })
    }
    fn request(&self, request: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        Box::pin(async move {
            let bytes = rmp_serde::to_vec_named(&request).map_err(BackendError::operation)?;
            let response = cpu_client::request(bytes).map_err(BackendError::operation)?.await.map_err(BackendError::operation)?;
            rmp_serde::from_slice(&response).map_err(BackendError::operation)?
        })
    }
    fn generate_thumbnail(&self, extension: String, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<Option<ThumbnailBytes>, BackendError>> {
        Box::pin(async move {
            let server = serve_reader(reader).map_err(BackendError::operation)?;
            let bytes = rmp_serde::to_vec_named(&cpu_host::Thumbnail { extension }.request()).map_err(BackendError::operation)?;
            let response = cpu_client::request_with_source(bytes, server.source()).map_err(BackendError::operation)?.await.map_err(BackendError::operation)?;
            cpu_host::Thumbnail::decode(rmp_serde::from_slice(&response).map_err(BackendError::operation)?)
        })
    }
}

/// Range source owned by one book request. The worker pulls parser-requested
/// ranges from it; dropping it releases the worker-side stream.
pub struct ReaderSource(Server);
impl ReaderSource {
    pub fn source(&self) -> JsValue {
        self.0.source()
    }
}

pub fn serve_reader<R: Read + Seek + Send + 'static>(mut reader: R) -> Result<ReaderSource, String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
    Server::sync(length, move |offset, length| {
        let mut bytes = vec![0; length.min(256 * 1024)];
        reader.seek(SeekFrom::Start(offset)).map_err(|error| error.to_string())?;
        let count = reader.read(&mut bytes).map_err(|error| error.to_string())?;
        bytes.truncate(count);
        Ok(bytes)
    })
    .map(ReaderSource)
}

pub async fn run_book_operation<C: BookOperation, R: Read + Seek + Send + 'static>(operation: C, reader: R) -> Result<C::Reply, String> {
    let server = serve_reader(reader)?;
    run_book_operation_with_source(operation, server.source()).await
}

pub async fn run_book_operation_with_source<C: BookOperation>(operation: C, source: JsValue) -> Result<C::Reply, String> {
    let bytes = rmp_serde::to_vec_named(&operation.request()).map_err(|error| error.to_string())?;
    let response = cpu_client::request_with_source(bytes, source).map_err(|error| error.to_string())?.await.map_err(|error| error.to_string())?;
    C::decode(rmp_serde::from_slice(&response).map_err(|error| error.to_string())?).map_err(|error| error.to_string())
}
