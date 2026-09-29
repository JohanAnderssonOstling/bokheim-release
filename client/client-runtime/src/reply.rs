//! An in-process reply. Encoding is deferred until an actual wire boundary.
use crate::{
    BackendError,
    wire::{WorkerPayload, encode_reply},
};
use client_platform_runtime::executor::BackendOutput;
use std::any::Any;

#[cfg(not(target_arch = "wasm32"))]
type Value = Box<dyn Any + Send>;
#[cfg(target_arch = "wasm32")]
type Value = Box<dyn Any>;

pub struct Reply {
    value: Value,
    encode: fn(&dyn Any) -> Result<WorkerPayload, BackendError>,
}

impl Reply {
    pub fn new<T: BackendOutput + serde::Serialize>(value: T) -> Self {
        Self { value: Box::new(value), encode: |value| encode_reply(value.downcast_ref::<T>().expect("reply encoder matches its value")) }
    }

    pub fn take<T: BackendOutput>(self) -> Result<T, BackendError> {
        self.value.downcast::<T>().map(|value| *value).map_err(|_| BackendError::message("unexpected backend reply type"))
    }

    pub fn into_wire(self) -> Result<WorkerPayload, BackendError> {
        (self.encode)(self.value.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Observed(Arc<AtomicUsize>);
    impl serde::Serialize for Observed {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.fetch_add(1, Ordering::Relaxed);
            serializer.serialize_u32(42)
        }
    }

    #[test]
    fn native_delivery_does_not_serialize() {
        let calls = Arc::new(AtomicUsize::new(0));
        let value = Reply::new(Observed(calls.clone())).take::<Observed>().unwrap();
        assert!(Arc::ptr_eq(&calls, &value.0));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn wire_delivery_encodes_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let payload = Reply::new(Observed(calls.clone())).into_wire().unwrap();
        assert_eq!(crate::wire::decode_worker_payload::<u32>(payload).unwrap(), 42);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn wrong_native_reply_type_is_an_error() {
        assert!(Reply::new(42u32).take::<String>().is_err());
    }
}
