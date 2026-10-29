//! Popup ownership for a Google grant. The app validates the grant with Google
//! before retaining it in device-owned connection storage.

use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(module = "/google_cloud_login.js")]
extern "C" {
    #[wasm_bindgen(js_name = prepareGoogleLogin, catch)]
    fn prepare(client_id: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = beginGoogleLogin, catch)]
    fn begin(config: &JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = googleLoginResult)]
    fn result(attempt: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = cancelGoogleLogin)]
    fn cancel(attempt: &JsValue);
}

fn message(error: JsValue) -> String {
    js_sys::Reflect::get(&error, &"message".into()).ok().and_then(|value| value.as_string()).unwrap_or_else(|| "Google sign-in could not be completed".into())
}

pub struct GoogleLogin(JsValue);
impl GoogleLogin {
    pub fn prepare(client_id: &str) -> Result<Self, String> {
        prepare(client_id).map(Self).map_err(message)
    }
    /// Called synchronously by the UI's click handler, before any await, so
    /// opening the sign-in window retains the browser's user activation.
    pub fn begin(&self) -> Result<GoogleLoginAttempt, String> {
        begin(&self.0).map(GoogleLoginAttempt).map_err(message)
    }
}

pub struct GoogleLoginAttempt(JsValue);
impl GoogleLoginAttempt {
    pub fn cancellation(&self) -> GoogleLoginCancellation {
        GoogleLoginCancellation(self.0.clone())
    }
    pub async fn wait(&self) -> Result<JsValue, String> {
        JsFuture::from(result(&self.0)).await.map_err(message)
    }
}
impl Drop for GoogleLoginAttempt {
    fn drop(&mut self) {
        cancel(&self.0);
    }
}

/// The dialog retains this guard while a task awaits the result. Closing the
/// dialog cancels its attempt even though the task still owns its future.
pub struct GoogleLoginCancellation(JsValue);
impl Drop for GoogleLoginCancellation {
    fn drop(&mut self) {
        cancel(&self.0);
    }
}
