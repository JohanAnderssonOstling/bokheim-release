//! Native CPU execution over the shared blocking executor.

use client_platform_runtime::executor;
use cpu_host::{BackendError, BookTask, BoxedCpuReader, CpuHost, CpuRequest, CpuResponse};

#[derive(Default)]
pub struct NativeHost;
impl CpuHost for NativeHost {
    fn book(&self, task: BookTask, reader: BoxedCpuReader) -> executor::BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        Box::pin(async move {
            executor::run_blocking(move || cpu_host::execute_book_with_mobi(task, reader, |path, bytes| book_inspection::inspect_mobi_pages_reader(path, bytes).map(|book| book.metadata.book).map_err(BackendError::operation)))
                .await
                .map_err(BackendError::operation)?
        })
    }
    fn request(&self, request: CpuRequest) -> executor::BoxedBackendFuture<'_, Result<CpuResponse, BackendError>> {
        Box::pin(async move { executor::run_blocking(move || cpu_host::execute(request)).await.map_err(BackendError::operation)? })
    }
}
