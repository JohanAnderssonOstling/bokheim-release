//! Preserve abort behavior at JavaScript stream callback boundaries.

use std::future::Future;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};

use js_sys::Promise;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::future_to_promise;

// Stream adapters accept arbitrary user streams/readers/sinks. They cannot
// promise that a partially completed operation is safe to resume after panic.
// Abort before a panic escapes any access, future poll, or destructor instead.
struct AbortDuringUnwind;

impl Drop for AbortDuringUnwind {
    fn drop(&mut self) {
        if std::thread::panicking() {
            std::process::abort();
        }
    }
}

pub struct AbortOnPanic<T> {
    value: Option<T>,
}

impl<T> AbortOnPanic<T> {
    pub fn new(value: T) -> Self {
        Self { value: Some(value) }
    }

    pub fn with<R>(&self, callback: impl FnOnce(&T) -> R) -> R {
        let _guard = AbortDuringUnwind;
        callback(self.value.as_ref().unwrap())
    }

    pub fn with_mut<R>(&mut self, callback: impl FnOnce(&mut T) -> R) -> R {
        let _guard = AbortDuringUnwind;
        callback(self.value.as_mut().unwrap())
    }
}

impl<T> Drop for AbortOnPanic<T> {
    fn drop(&mut self) {
        let _guard = AbortDuringUnwind;
        // Drop arbitrary user state while the guard is still alive.
        drop(self.value.take());
    }
}

// All state is private and accessed/dropped only through the guards above.
// A panic terminates the instance before an outer catch boundary can observe
// partially changed state. This does not assert unwind safety of T itself.
impl<T> UnwindSafe for AbortOnPanic<T> {}
impl<T> RefUnwindSafe for AbortOnPanic<T> {}

impl<F: Future> Future for AbortOnPanic<Pin<Box<F>>> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().with_mut(|future| future.as_mut().poll(context))
    }
}

pub fn future_to_promise_aborting<F>(future: F) -> Promise
where
    F: Future<Output = Result<JsValue, JsValue>> + 'static,
{
    future_to_promise(AbortOnPanic::new(Box::pin(future)))
}

#[cfg(all(test, unix, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::process::Command;

    #[test]
    fn guarded_state_and_pending_future_work() {
        let mut state = AbortOnPanic::new(2);
        state.with_mut(|value| *value += 3);
        assert_eq!(state.with(|value| *value), 5);

        let mut polls = 0;
        let future = futures_util::future::poll_fn(move |_| {
            polls += 1;
            if polls == 1 {
                Poll::Pending
            } else {
                Poll::Ready(7)
            }
        });
        let mut future = AbortOnPanic::new(Box::pin(future));
        let waker = futures_util::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        assert_eq!(Pin::new(&mut future).poll(&mut context), Poll::Pending);
        assert_eq!(Pin::new(&mut future).poll(&mut context), Poll::Ready(7));
    }

    #[test]
    fn panics_abort_before_an_outer_catch_can_resume() {
        for operation in ["read", "write", "poll", "drop"] {
            let output = Command::new(std::env::current_exe().unwrap()).args(["--exact", "tests::panic_boundary_child", "--nocapture"]).env("WASM_STREAMS_PANIC_TEST", operation).output().unwrap();
            assert_eq!(output.status.signal(), Some(6), "{operation}: {output:?}");
        }
    }

    #[test]
    fn panic_boundary_child() {
        let Ok(operation) = std::env::var("WASM_STREAMS_PANIC_TEST") else { return };
        // An outer catch must never get control back from any guarded panic.
        let _ = catch_unwind(AssertUnwindSafe(|| match operation.as_str() {
            "read" => AbortOnPanic::new(0).with(|_| panic!("read callback")),
            "write" => AbortOnPanic::new(0).with_mut(|_| panic!("write callback")),
            "poll" => {
                let mut future = AbortOnPanic::new(Box::pin(async { panic!("stream poll") }));
                let waker = futures_util::task::noop_waker();
                let _ = Pin::new(&mut future).poll(&mut Context::from_waker(&waker));
            }
            "drop" => {
                struct PanickingDrop;
                impl Drop for PanickingDrop {
                    fn drop(&mut self) {
                        panic!("stream drop");
                    }
                }
                drop(AbortOnPanic::new(PanickingDrop));
            }
            _ => unreachable!(),
        }));
        panic!("guard let a panic escape");
    }
}
