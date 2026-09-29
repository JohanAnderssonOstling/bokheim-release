//! Browser file and worker adapters for the platform-independent staged workflow.
pub mod queue;

use crate::library::commands::library_requests;
use client_platform_web::transport::import_io::{ImportIo, OpenFile};
use import_pipeline::{
    import_journal::{self, Progress},
    staged_import::{self, Error, Manifest},
};
use std::rc::Rc;
use wasm_bindgen::prelude::*;

fn error(value: impl ToString) -> JsValue {
    JsValue::from_str(&value.to_string())
}
fn from_js(value: JsValue) -> Error {
    Error {
        storage_full: client_platform_web::transport::import_io::storage_full(&value),
        source: crate::BackendError::message(client_platform_web::transport::import_io::error_text(&value)),
        transport: client_platform_web::transport::field(&value, "transport").as_bool().unwrap_or(false),
    }
}
fn from_backend(source: crate::BackendError) -> Error {
    let report = source.report();
    let storage_full = report.causes.iter().any(|cause| match cause {
        client_runtime::ErrorCause::Sqlite { extended_code } => *extended_code & 0xff == 13,
        client_runtime::ErrorCause::Io { os_code, .. } => matches!(os_code, Some(28) | Some(112)),
        _ => false,
    }) || ["QuotaExceededError", "database or disk is full", "browser storage is full", "Incomplete asset write"].iter().any(|marker| report.message.contains(marker));
    Error { transport: source.is_transport(), source, storage_full }
}
fn to_js(value: Error) -> JsValue {
    if value.transport || value.storage_full {
        let error = js_sys::Error::new(&value.source.to_string());
        if value.transport {
            let _ = js_sys::Reflect::set(&error, &"transport".into(), &true.into());
        }
        if value.storage_full {
            let _ = js_sys::Reflect::set(&error, &"storageFull".into(), &true.into());
        }
        error.into()
    } else {
        error(value.source)
    }
}
pub type ProgressSink = Rc<dyn Fn(JsValue) -> Result<(), JsValue>>;

struct Reporter {
    progress: ProgressSink,
    id: String,
    name: String,
}
impl Reporter {
    fn emit(&self, phase: staged_import::Phase, counts: Progress, current_file: Option<&str>, failures: Option<&[crate::ImportFailure]>) -> Result<(), JsValue> {
        let phase = match phase {
            staged_import::Phase::Copying => "copying",
            staged_import::Phase::Adding => "adding",
            staged_import::Phase::Complete => "complete",
        };
        let value = client_platform_web::transport::object(&[
            ("id", self.id.as_str().into()),
            ("name", self.name.as_str().into()),
            ("phase", phase.into()),
            ("totalBytes", (counts.total_bytes as f64).into()),
            ("copiedBytes", (counts.copied_bytes as f64).into()),
            ("totalFiles", (counts.total_files as f64).into()),
            ("copiedFiles", (counts.copied_files as f64).into()),
            ("processed", (counts.processed as f64).into()),
            ("failed", (counts.failed as f64).into()),
        ]);
        if let Some(file) = current_file {
            js_sys::Reflect::set(&value, &"currentFile".into(), &file.into())?;
        }
        if let Some(failures) = failures {
            js_sys::Reflect::set(&value, &"failures".into(), &failures_value(failures)?)?;
        }
        (self.progress)(value)
    }
}

struct BrowserFile(OpenFile);
struct BrowserIo(ImportIo);
pub trait ImportClient {
    fn request<T: crate::executor::BackendOutput + serde::de::DeserializeOwned>(&self, command: crate::library::LibraryCommand) -> impl std::future::Future<Output = Result<T, crate::BackendError>>;
}
struct BrowserClient<'a, C>(&'a C);

impl staged_import::FileAccess for BrowserFile {
    fn read(&self) -> Result<Vec<u8>, Error> {
        self.0 .0.read().map(|bytes| bytes.to_vec()).map_err(from_js)
    }
    fn write(&self, bytes: &[u8], offset: u64) -> Result<(), Error> {
        OpenFile::write(&self.0, bytes, offset).map_err(from_js)
    }
    fn truncate(&self, offset: u64) -> Result<(), Error> {
        self.0 .0.truncate(offset as f64).map_err(from_js)
    }
    fn flush(&self) -> Result<(), Error> {
        self.0 .0.flush().map_err(from_js)
    }
}
impl staged_import::Storage for BrowserIo {
    type File = BrowserFile;
    fn staged_path(&self, manifest: &Manifest, index: usize) -> String {
        format!("__libraries/{}/imports/{}/{index}", manifest.library, manifest.id)
    }
    async fn open_file(&self, name: &str) -> Result<BrowserFile, Error> {
        ImportIo::open_file(&self.0, name).await.map(BrowserFile).map_err(from_js)
    }
    fn source_metadata(&self, index: u32) -> Result<(u64, f64), Error> {
        ImportIo::source_metadata(&self.0, index).map_err(from_js)
    }
    async fn read_source(&self, index: u32, start: u64, end: u64) -> Result<Vec<u8>, Error> {
        ImportIo::read_source(&self.0, index, start as f64, end as f64).await.map(|value| value.unchecked_into::<js_sys::Uint8Array>().to_vec()).map_err(from_js)
    }
}
impl<C: ImportClient> staged_import::Library for BrowserClient<'_, C> {
    async fn prepare(&self, activity: uuid::Uuid, manifest: &Manifest) -> Result<staged_import::PreparedImport, Error> {
        self.0
            .request(library_requests::PrepareStagedDirectoryImport { activity, job: manifest.id.clone(), parent_id: manifest.parent, name: manifest.create_root.then(|| manifest.name.clone()), directories: manifest.directories.clone() })
            .await
            .map(|prepared: crate::library::PreparedDirectoryImport| staged_import::PreparedImport { directories: prepared.directories, failures: prepared.failures })
            .map_err(from_backend)
    }
    async fn import(&self, book: staged_import::StagedBook) -> Result<(), Error> {
        self.0.request(library_requests::ImportStagedBook { parent_id: book.parent_id, file_name: book.file_name, physical: book.physical, length: book.length, hash: book.hash }).await.map(|_: crate::ContentHash| ()).map_err(from_backend)
    }
    async fn advance(&self, progress: staged_import::AdvanceProgress) -> Result<(), Error> {
        self.0.request(library_requests::AdvanceDirectoryImport { activity: progress.activity, total: progress.total, succeeded: progress.succeeded, failed: progress.failed }).await.map_err(from_backend)
    }
    async fn finish(&self, activity: uuid::Uuid) -> Result<(), Error> {
        self.0.request(library_requests::FinishDirectoryImport { activity }).await.map_err(from_backend)
    }
    async fn cleanup(&self, job: String, files: u32) -> Result<(), Error> {
        self.0.request(library_requests::CleanupStagedImport { job, files }).await.map_err(from_backend)
    }
}

pub fn failures_value(values: &[crate::ImportFailure]) -> Result<JsValue, JsValue> {
    js_sys::JSON::parse(&serde_json::to_string(values).map_err(error)?)
}
pub fn recover(manifest: &[u8], journal: &[u8]) -> Result<JsValue, JsValue> {
    if import_journal::replay(journal).map_err(error)?.completed.is_some() {
        return Ok(JsValue::UNDEFINED);
    }
    let manifest: Manifest = serde_json::from_slice(manifest).map_err(error)?;
    js_sys::JSON::parse(&serde_json::to_string(&manifest).map_err(error)?)
}
pub async fn run<C: ImportClient>(job: JsValue, io: ImportIo, client: &C, progress: ProgressSink) -> Result<Vec<crate::ImportFailure>, JsValue> {
    let manifest: Manifest = serde_json::from_str(&String::from(js_sys::JSON::stringify(&job)?)).map_err(error)?;
    staged_import::run(manifest, BrowserIo(io), &BrowserClient(client), move |update| {
        let reporter = Reporter { progress: progress.clone(), id: update.id, name: update.name };
        reporter.emit(update.phase, update.counts, update.current_file.as_deref(), update.failures.as_deref()).map_err(from_js)
    })
    .await
    .map_err(to_js)
}
pub fn submission(id: &str, library_id: crate::LibraryId, parent_id: crate::DirId, directory: crate::DirectoryImport, create_root: bool) -> Result<(String, js_sys::Array), String> {
    let files = js_sys::Array::new();
    for file in directory.files {
        if !import_pipeline::import_workflow::supported(&file.path) {
            continue;
        }
        let source = file.source.into_file()?;
        let entry = js_sys::Object::new();
        let path = js_sys::Array::new();
        for component in file.path {
            path.push(&JsValue::from_str(&component));
        }
        js_sys::Reflect::set(&entry, &"path".into(), &path).map_err(|e| format!("{e:?}"))?;
        js_sys::Reflect::set(&entry, &"file".into(), &source).map_err(|e| format!("{e:?}"))?;
        files.push(&entry);
    }
    let manifest = serde_json::json!({ "id": id, "library": library_id, "parent": parent_id, "name": directory.name, "createRoot": create_root, "directories": directory.directories });
    Ok((manifest.to_string(), files))
}
