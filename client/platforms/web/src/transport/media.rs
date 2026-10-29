//! A tab-owned media URL over an authenticated, cancellable range callback.
//! The service worker only brokers bytes; credentials stay with the library.

use futures_channel::oneshot;
use futures_util::future::{select, Either};
use js_sys::{Promise, Uint8Array};
use std::{cell::RefCell, future::Future};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use wasm_callback::future_to_promise_aborting;
use web_sys::AbortSignal;

#[wasm_bindgen(module = "/audio_source.js")]
extern "C" {
    #[wasm_bindgen(js_name = prepareRemoteAudio, catch)]
    fn prepare() -> Result<Promise, JsValue>;
    #[wasm_bindgen(js_name = openRemoteAudio, catch)]
    fn open(length: f64, read: &JsValue, content_type: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = remoteAudioUrl)]
    fn url(source: &JsValue) -> String;
    #[wasm_bindgen(js_name = closeRemoteAudio)]
    fn close(source: &JsValue);
    #[wasm_bindgen(js_name = setRemoteAudioPaused)]
    fn pause(source: &JsValue, paused: bool);
}

type RangeCallback = Closure<dyn FnMut(f64, u32, AbortSignal) -> Promise>;

pub struct RemoteMedia {
    source: JsValue,
    _read: RangeCallback,
}

impl RemoteMedia {
    pub async fn new<F, Fut>(length: u64, content_type: &str, mut read: F) -> Result<Self, JsValue>
    where
        F: FnMut(u64, usize) -> Fut + 'static,
        Fut: Future<Output = Result<Vec<u8>, JsValue>> + 'static,
    {
        if length == 0 || length > 9_007_199_254_740_991 {
            return Err(js_sys::Error::new("Invalid audiobook length").into());
        }
        JsFuture::from(prepare()?).await?;
        let callback = Closure::own_aborting(move |offset: f64, count: u32, signal: AbortSignal| {
            if !offset.is_finite() || offset < 0.0 || offset.fract() != 0.0 || offset > length as f64 || count == 0 || count > 256 * 1024 || offset as u64 + u64::from(count) > length {
                return Promise::reject(&js_sys::Error::new("Invalid audiobook range"));
            }
            let work = read(offset as u64, count as usize);
            future_to_promise_aborting(async move {
                let bytes = cancellable(signal, work).await?;
                Ok(Uint8Array::from(bytes.as_slice()).into())
            })
        });
        let source = open(length as f64, callback.as_ref(), content_type)?;
        Ok(Self { source, _read: callback })
    }

    pub fn url(&self) -> String {
        url(&self.source)
    }

    pub fn pause_control(sources: &[Self]) -> Option<Closure<dyn FnMut(bool)>> {
        if sources.is_empty() {
            return None;
        }
        let sources = sources.iter().map(|source| source.source.clone()).collect::<Vec<_>>();
        Some(Closure::own_aborting(move |paused| {
            for source in &sources {
                pause(source, paused);
            }
        }))
    }
}

impl Drop for RemoteMedia {
    fn drop(&mut self) {
        close(&self.source);
    }
}

struct AbortListener {
    signal: AbortSignal,
    callback: Closure<dyn FnMut()>,
}
impl Drop for AbortListener {
    fn drop(&mut self) {
        let _ = self.signal.remove_event_listener_with_callback("abort", self.callback.as_ref().unchecked_ref());
    }
}

async fn cancellable<T>(signal: AbortSignal, work: impl Future<Output = Result<T, JsValue>>) -> Result<T, JsValue> {
    if signal.aborted() {
        return Err(js_sys::Error::new("Audiobook request cancelled").into());
    }
    let (send, receive) = oneshot::channel();
    let send = RefCell::new(Some(send));
    let callback = Closure::own_aborting(move || {
        if let Some(send) = send.borrow_mut().take() {
            let _ = send.send(());
        }
    });
    signal.add_event_listener_with_callback("abort", callback.as_ref().unchecked_ref())?;
    let listener = AbortListener { signal, callback };
    let result = match select(Box::pin(work), receive).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => Err(js_sys::Error::new("Audiobook request cancelled").into()),
    };
    drop(listener);
    result
}
