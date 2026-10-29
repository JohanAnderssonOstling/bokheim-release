//! Test-only WASM entry points for the browser cloud runtime fixture.
#![cfg(target_arch = "wasm32")]

use library_backend::{executor, BackendError};
use library_enrichment::MetadataClient;
use metadata_contract::books::BookEnrichmentRequest;
use std::{cell::Cell, rc::Rc, time::Duration};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(js_name = prepareCloudChecks)]
pub async fn prepare(endpoint: String, cpu: bool) -> Result<(), JsValue> {
    check_startup_barrier().await.map_err(|error| JsValue::from_str(&error.to_string()))?;
    check_metadata_timers(&endpoint).await.map_err(|error| JsValue::from_str(&error.to_string()))?;
    library_backend::prepare_browser_cloud_checks(&endpoint, cpu).await.map_err(|error| JsValue::from_str(&error.to_string()))
}

#[wasm_bindgen(js_name = runCloudChecks)]
pub async fn run(endpoint: String, writable: bool) -> Result<String, JsValue> {
    library_backend::check_browser_cloud(&endpoint, writable).await.map_err(|error| JsValue::from_str(&error.to_string()))
}

#[wasm_bindgen(js_name = executeCpu)]
pub async fn execute_cpu(bytes: &[u8], source: JsValue) -> Result<Vec<u8>, JsValue> {
    let response = (async {
        let request: cpu_host::CpuRequest = rmp_serde::from_slice(bytes).map_err(library_backend::BackendError::operation)?;
        match request {
            cpu_host::CpuRequest::Book { task, .. } => {
                library_backend::library::web_reader::connect(&source).await.map_err(|error| library_backend::BackendError::message(format!("{error:?}")))?;
                let reader = library_backend::library::web_reader::Reader::new(source).map_err(library_backend::BackendError::operation)?;
                cpu_host::execute_book_with_mobi(task, reader, |_, _| Err("fixture does not inspect MOBI pages".into()))
            }
            request => cpu_host::execute(request),
        }
    })
    .await;
    rmp_serde::to_vec_named(&response).map_err(|error| JsValue::from_str(&error.to_string()))
}

// Run in both workers: their startup gates must remain independent.
async fn check_startup_barrier() -> Result<(), BackendError> {
    let permit = executor::defer_background_work();
    let executor = executor::BackendExecutor::new().map_err(BackendError::operation)?;
    let started = Rc::new(Cell::new(false));
    let running = started.clone();
    let task = executor.spawn(async move { running.set(true) });
    executor::sleep(Duration::ZERO).await.map_err(BackendError::operation)?;
    assert!(!started.get(), "background work ran before startup committed");
    permit.release();
    task.await.map_err(BackendError::operation)?;
    assert!(started.get(), "startup commit did not release background work");
    assert_eq!(executor.spawn(async { 42 }).await.map_err(BackendError::operation)?, 42);
    Ok(())
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = globalThis, js_name = __activeMetadataTimers)]
    fn active_metadata_timers() -> u32;
}

async fn check_metadata_timers(endpoint: &str) -> Result<(), BackendError> {
    let client = MetadataClient::new(&format!("{endpoint}/metadata/")).map_err(BackendError::operation)?;
    let request = BookEnrichmentRequest { books: Vec::new() };
    client.books(&request).await.map_err(BackendError::operation)?;
    assert_eq!(active_metadata_timers(), 0, "successful metadata requests must clear their deadline");

    let mut lookup = Box::pin(client.books(&request));
    assert!(futures_util::poll!(&mut lookup).is_pending());
    assert_eq!(active_metadata_timers(), 1, "an active metadata request must own one deadline");
    drop(lookup);
    assert_eq!(active_metadata_timers(), 0, "cancelled metadata requests must clear their deadline");
    Ok(())
}
