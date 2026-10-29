//! Browser checks for the local callback patch; not part of the application.

use std::cell::{Cell, RefCell};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures_util::{io::Cursor, sink, stream, AsyncReadExt, SinkExt, Stream};
use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_streams::{ReadableStream, WritableStream};

#[path = "../src/callback.rs"]
mod callback;

#[wasm_bindgen]
pub fn check_streams() -> Promise {
    callback::future_to_promise_aborting(async {
        // Rust operations may still catch panics before the callback boundary.
        assert!(std::panic::catch_unwind(|| panic!("caught operation panic")).is_err());
        let mut polls = 0;
        let mut readable = ReadableStream::from_stream(stream::poll_fn(move |context| {
            polls += 1;
            match polls {
                1 => {
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
                2 => Poll::Ready(Some(Ok(JsValue::from_str("chunk")))),
                3 => Poll::Ready(Some(Err(JsValue::from_str("ordinary stream error")))),
                _ => Poll::Ready(None),
            }
        }));
        let mut reader = readable.get_reader();
        assert_eq!(reader.read().await?, Some(JsValue::from_str("chunk")));
        assert_eq!(reader.read().await, Err(JsValue::from_str("ordinary stream error")));
        drop(reader);

        let bytes: Vec<u8> = (0..8192).map(|n| (n % 251) as u8).collect();
        let mut input = ReadableStream::from_async_read(Cursor::new(bytes.clone()), 1024).into_async_read();
        let mut output = Vec::new();
        input.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, bytes);

        let chunks = Rc::new(RefCell::new(Vec::new()));
        let recorded = chunks.clone();
        let sink = sink::unfold(recorded, |chunks, chunk: JsValue| async move {
            chunks.borrow_mut().push(chunk);
            Ok::<_, JsValue>(chunks)
        });
        let mut writable = WritableStream::from_sink(sink);
        let mut writer = writable.get_writer();
        writer.write(JsValue::from_str("first")).await?;
        writer.write(JsValue::from_str("second")).await?;
        writer.close().await?;
        writer.closed().await?;
        assert_eq!(*chunks.borrow(), vec![JsValue::from_str("first"), JsValue::from_str("second")]);

        let dropped = Rc::new(Cell::new(false));
        let mut readable = ReadableStream::from_stream(PendingStream(dropped.clone()));
        let mut reader = readable.get_reader();
        reader.cancel().await?;
        assert!(dropped.get());

        let mut writable = WritableStream::from_sink(sink::drain().sink_map_err(|never| match never {}));
        writable.abort_with_reason(&JsValue::from_str("cancelled")).await?;
        Ok(JsValue::from_str("chunks, pending poll, errors, bytes, writes, close, cancellation, abort"))
    })
}

struct PendingStream(Rc<Cell<bool>>);
impl Stream for PendingStream {
    type Item = Result<JsValue, JsValue>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
impl Drop for PendingStream {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[wasm_bindgen]
pub fn panicking_stream() -> web_sys::ReadableStream {
    ReadableStream::from_stream(stream::poll_fn(|_| -> Poll<Option<Result<JsValue, JsValue>>> {
        panic!("intentional stream callback panic");
    }))
    .into_raw()
}

#[wasm_bindgen]
pub fn alive() -> u32 {
    7
}

#[wasm_bindgen]
pub fn memory_is_shared() -> bool {
    let memory = wasm_bindgen::memory().unchecked_into::<js_sys::WebAssembly::Memory>();
    memory.buffer().is_instance_of::<js_sys::SharedArrayBuffer>()
}
