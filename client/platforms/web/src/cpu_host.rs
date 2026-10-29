//! Browser CPU transport and source lifetimes over the CPU worker.

use super::transport::{cpu_client, range::Server};
use client_platform_runtime::executor::BoxedBackendFuture;
use cpu_host::{Artifact, BackendError, BookOperation, BookTask, BoxedCpuReader, CpuHost, CpuRequest, CpuResponse};
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use wasm_bindgen::JsValue;

thread_local! {
    static TAXONOMY: RefCell<Option<Artifact>> = const { RefCell::new(None) };
}

/// The snapshot the database worker's taxonomy came from. Inspection checks
/// subject codes against the taxonomy, and the CPU worker, a separate module,
/// loads this snapshot itself on its first inspection.
pub fn set_taxonomy(artifact: Artifact) {
    TAXONOMY.with(|slot| *slot.borrow_mut() = Some(artifact));
}

/// All CPU worker requests share encoding, taxonomy selection, and decoding.
async fn send(mut request: CpuRequest, source: JsValue) -> Result<CpuResponse, BackendError> {
    if let CpuRequest::Book { task: BookTask::Inspect { .. }, taxonomy } = &mut request {
        *taxonomy = TAXONOMY.with(|slot| slot.borrow().clone());
    }
    let bytes = rmp_serde::to_vec_named(&request).map_err(BackendError::operation)?;
    let response = cpu_client::request_with_source(bytes, source).map_err(BackendError::operation)?.await.map_err(BackendError::operation)?;
    rmp_serde::from_slice(&response).map_err(BackendError::operation)?
}

#[derive(Default)]
pub struct WebHost;
impl CpuHost for WebHost {
    fn book(&self, task: BookTask, reader: BoxedCpuReader) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        Box::pin(async move {
            let server = serve_reader(reader).map_err(BackendError::operation)?;
            send(CpuRequest::Book { task, taxonomy: None }, server.source()).await
        })
    }
    fn request(&self, request: CpuRequest) -> BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        Box::pin(send(request, JsValue::UNDEFINED))
    }
}

/// Dropping the range server releases the request's worker-side stream.
pub fn serve_reader<R: Read + Seek + Send + 'static>(mut reader: R) -> Result<Server, String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
    Server::sync(length, move |offset, length| crate::reader::read_range(&mut reader, offset, length.min(256 * 1024)).map_err(|error| error.to_string()))
}

pub async fn run_book_operation_with_source<C: BookOperation>(operation: C, source: JsValue) -> Result<C::Reply, String> {
    C::decode(send(operation.request(), source).await.map_err(|error| error.to_string())?).map_err(|error| error.to_string())
}
