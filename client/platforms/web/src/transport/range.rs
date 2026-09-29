//! Shared-memory range transport between dedicated browser workers.
use super::{error_message, field};
use js_sys::{Function, Uint8Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(inline_js = r#"
export function serveBook(read, length) {
    const channel = new MessageChannel();
    const buffer = new SharedArrayBuffer(8 + 256 * 1024);
    const state = new Int32Array(buffer, 0, 2);
    const bytes = new Uint8Array(buffer, 8);
    channel.port1.onmessage = async ({data}) => {
        if (Atomics.load(state, 0) === -2) return;
        if (data.connect === true) {
            try { channel.port1.postMessage({buffer}); }
            catch (error) { channel.port1.postMessage({error: String(error)}); }
            return;
        }
        try {
            if (!Number.isSafeInteger(data.offset) || data.offset < 0 ||
                !Number.isInteger(data.length) || data.length < 0 || data.length > bytes.length)
                throw new Error('invalid book range');
            const chunk = await read(data.offset, data.length);
            if (Atomics.load(state, 0) === -2) return;
            bytes.set(chunk);
            Atomics.store(state, 1, chunk.length);
            Atomics.store(state, 0, 1);
        } catch (_) { if (Atomics.load(state, 0) === -2) return; Atomics.store(state, 0, -1); }
        Atomics.notify(state, 0);
    };
    return {source: {port: channel.port2, length}, close() {
        channel.port1.postMessage({error: 'book source closed'});
        channel.port1.close();
        channel.port2.close();
        Atomics.store(state, 0, -2);
        Atomics.notify(state, 0);
    }};
}
export function closeBook(server) { server.close(); }
// The SharedWorker brokers this port, but never receives shared memory.
// Both dedicated workers belong to the same host tab's agent cluster.
export function connectBook(source) {
    return new Promise((resolve, reject) => {
        const finish = error => {
            clearTimeout(timeout);
            source.port.onmessage = null;
            source.port.onmessageerror = null;
            if (error) reject(new Error(error)); else resolve();
        };
        const timeout = setTimeout(() => finish('book source connection timed out'), 30000);
        source.port.onmessage = ({data}) => {
            if (data.error) { finish(data.error); return; }
            source.buffer = data.buffer;
            finish();
        };
        source.port.onmessageerror = () => finish('invalid book source connection');
        source.port.postMessage({connect: true});
    });
}
function readBookRange(source, offset, length) {
    const state = new Int32Array(source.buffer, 0, 2);
    if (Atomics.load(state, 0) < 0) throw new Error('book source closed');
    Atomics.store(state, 0, 0);
    source.port.postMessage({offset, length});
    const deadline = Date.now() + 30000;
    while (Atomics.load(state, 0) === 0) {
        if (Atomics.wait(state, 0, 0, Math.max(0, deadline - Date.now())) === 'timed-out')
            throw new Error('book range read timed out');
    }
    if (Atomics.load(state, 0) !== 1) throw new Error('book range read failed');
    return new Uint8Array(source.buffer, 8, Atomics.load(state, 1));
}
export function checkBook(source) {
    if (Atomics.load(new Int32Array(source.buffer, 0, 2), 0) < 0)
        throw new Error('book source closed');
}
export function readBook(source, offset, length) {
    return readBookRange(source, offset, length);
}

"#)]
extern "C" {
    #[wasm_bindgen(js_name = serveBook, catch)]
    fn serve(read: &Function, length: f64) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = connectBook)]
    fn connect_source(source: &JsValue) -> js_sys::Promise;
    #[wasm_bindgen(js_name = closeBook)]
    fn close(server: &JsValue);
    #[wasm_bindgen(js_name = checkBook, catch)]
    fn check_source(source: &JsValue) -> Result<(), JsValue>;
    #[wasm_bindgen(js_name = readBook, catch)]
    fn read_range(source: &JsValue, offset: f64, length: u32) -> Result<Uint8Array, JsValue>;
}

pub async fn connect(source: &JsValue) -> Result<(), JsValue> {
    wasm_bindgen_futures::JsFuture::from(connect_source(source)).await.map(|_| ())
}

pub struct Server {
    server: JsValue,
    _read: Box<dyn std::any::Any>,
}
impl Server {
    pub fn sync(length: u64, mut fetch: impl FnMut(u64, usize) -> Result<Vec<u8>, String> + 'static) -> Result<Self, String> {
        let read = Closure::<dyn FnMut(f64, u32) -> Result<Uint8Array, JsValue>>::new(move |offset, length| fetch(offset as u64, length as usize).map(|bytes| Uint8Array::from(bytes.as_slice())).map_err(|e| JsValue::from_str(&e)));
        Self::attach(length, read)
    }
    pub fn asynchronous<F, Fut>(length: u64, mut fetch: F) -> Result<Self, String>
    where
        F: FnMut(u64, usize) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<Vec<u8>, String>> + 'static,
    {
        let read = Closure::<dyn FnMut(f64, u32) -> js_sys::Promise>::new(move |offset, length| {
            let future = fetch(offset as u64, length as usize);
            wasm_bindgen_futures::future_to_promise(async move { future.await.map(|bytes| Uint8Array::from(bytes.as_slice()).into()).map_err(|e| JsValue::from_str(&e)) })
        });
        Self::attach(length, read)
    }
    fn attach<T: ?Sized + wasm_bindgen::closure::WasmClosure + 'static>(length: u64, read: Closure<T>) -> Result<Self, String> {
        if length > 9_007_199_254_740_991 {
            return Err("book exceeds browser offset precision".into());
        }
        let server = serve(read.as_ref().unchecked_ref(), length as f64).map_err(|e| error_message(&e))?;
        Ok(Self { server, _read: Box::new(read) })
    }
    pub fn source(&self) -> JsValue {
        field(&self.server, "source")
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        close(&self.server);
    }
}

// Only numeric IDs cross the parser's Send + Sync boundary. JS handles remain
// local to the worker that connected them.
thread_local! {
    static SOURCES: std::cell::RefCell<std::collections::HashMap<u32, JsValue>> = Default::default();
    static NEXT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
pub struct Source {
    id: u32,
    length: u64,
}
impl Source {
    pub fn new(source: JsValue) -> Result<Self, String> {
        let length = field(&source, "length").as_f64().filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= 9_007_199_254_740_991.0).ok_or("invalid book length")? as u64;
        let id = NEXT.with(|next| {
            let id = next.get().wrapping_add(1);
            next.set(id);
            id
        });
        SOURCES.with(|sources| sources.borrow_mut().insert(id, source));
        Ok(Self { id, length })
    }
    pub fn length(&self) -> u64 {
        self.length
    }
    fn with<T>(&self, operation: impl FnOnce(&JsValue) -> Result<T, JsValue>) -> std::io::Result<T> {
        SOURCES.with(|sources| {
            let sources = sources.borrow();
            let source = sources.get(&self.id).ok_or_else(|| std::io::Error::other("book reader unavailable on this worker"))?;
            operation(source).map_err(|e| std::io::Error::other(error_message(&e)))
        })
    }
    pub fn check(&self) -> std::io::Result<()> {
        self.with(check_source)
    }
    pub fn read(&self, offset: u64, length: u32) -> std::io::Result<Vec<u8>> {
        self.with(|source| read_range(source, offset as f64, length).map(|bytes| bytes.to_vec()))
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        if let Some(source) = SOURCES.with(|sources| sources.borrow_mut().remove(&self.id)) {
            let port: web_sys::MessagePort = field(&source, "port").unchecked_into();
            port.close();
        }
    }
}
