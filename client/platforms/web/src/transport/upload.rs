use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
struct AbortUpload(web_sys::AbortController);
impl Drop for AbortUpload {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn put_blob(file: &web_sys::Blob, url: &str, request_headers: &[(&str, &str)]) -> Result<u16, String> {
    let abort = AbortUpload(web_sys::AbortController::new().map_err(|e| super::error_message(&e))?);
    let headers = web_sys::Headers::new().map_err(|e| super::error_message(&e))?;
    for (name, value) in request_headers {
        headers.set(name, value).map_err(|e| super::error_message(&e))?;
    }
    let options = web_sys::RequestInit::new();
    options.set_method("PUT");
    options.set_headers(&headers);
    options.set_body(file);
    options.set_signal(Some(&abort.0.signal()));
    // fetch may be the coordinator bridge; it must preserve the Blob body.
    let scope: web_sys::WorkerGlobalScope = js_sys::global().unchecked_into();
    let response: web_sys::Response = JsFuture::from(scope.fetch_with_str_and_init(url, &options)).await.map_err(|e| super::error_message(&e))?.dyn_into().map_err(|e| super::error_message(&e))?;
    Ok(response.status())
}
