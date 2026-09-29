use wasm_bindgen::prelude::*;
const SHORT_WRITE: &str = "Short import write: browser storage is full";

pub fn storage_full(error: &JsValue) -> bool {
    js_sys::Reflect::get(error, &"name".into()).ok().and_then(|name| name.as_string()).as_deref() == Some("QuotaExceededError") || error.as_string().as_deref() == Some(SHORT_WRITE)
}
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = String)]
    pub fn error_text(value: &JsValue) -> String;
    #[derive(Clone)]
    pub type ImportIo;
    #[wasm_bindgen(method, catch)]
    pub async fn open(this: &ImportIo, name: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn source(this: &ImportIo, index: u32) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, catch, js_name = readSource)]
    pub async fn read_source(this: &ImportIo, index: u32, start: f64, end: f64) -> Result<JsValue, JsValue>;
    pub type ImportFile;
    #[wasm_bindgen(method, catch)]
    pub fn read(this: &ImportFile) -> Result<js_sys::Uint8Array, JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn write(this: &ImportFile, bytes: &[u8], offset: f64) -> Result<usize, JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn truncate(this: &ImportFile, offset: f64) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn flush(this: &ImportFile) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn close(this: &ImportFile) -> Result<(), JsValue>;
}

pub struct OpenFile(pub ImportFile);
impl Drop for OpenFile {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}
impl ImportIo {
    pub async fn open_file(&self, name: &str) -> Result<OpenFile, JsValue> {
        Ok(OpenFile(self.open(name).await?.unchecked_into()))
    }
    pub fn source_metadata(&self, index: u32) -> Result<(u64, f64), JsValue> {
        let file = self.source(index)?.dyn_into::<web_sys::File>().map_err(|_| JsValue::from_str("The source folder must be selected again to finish copying"))?;
        Ok((file.size() as u64, file.last_modified()))
    }
}
impl OpenFile {
    pub fn write(&self, bytes: &[u8], offset: u64) -> Result<(), JsValue> {
        if self.0.write(bytes, offset as f64)? != bytes.len() {
            return Err(JsValue::from_str(SHORT_WRITE));
        }
        Ok(())
    }
}

/// A picker-owned browser source transferable to the import worker.
/// JavaScript capabilities stay in this adapter rather than the import model.
pub struct ImportSource(Option<Box<dyn FnOnce() -> JsValue + Send>>);
pub type ImportReader = std::pin::Pin<Box<dyn futures_util::io::AsyncRead>>;
pub type ImportReaderFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<ImportReader, String>>>>;
impl ImportSource {
    pub fn from_export(export: Option<Box<dyn FnOnce() -> JsValue + Send>>) -> Self {
        Self(export)
    }
    pub fn into_file(self) -> Result<web_sys::File, String> {
        let export = self.0.ok_or("Selected file cannot be transferred to the import worker")?;
        export().dyn_into().map_err(|_| "Selected source is not a browser file".to_owned())
    }
}
