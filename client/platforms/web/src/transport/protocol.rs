//! Typed transport envelopes. Application payloads and scheduling remain opaque.
use super::{error_message, field};
use wasm_bindgen::{JsCast, JsValue};

pub(super) fn request_id(message: &JsValue) -> Result<u32, String> {
    let id = field(message, "id").as_f64().ok_or("Missing request id")?;
    client_platform_runtime::protocol::request_id(id)
}

fn bytes(value: &JsValue) -> Result<Vec<u8>, String> {
    value.dyn_ref::<js_sys::Uint8Array>().map(|bytes| bytes.to_vec()).ok_or_else(|| "Expected Uint8Array payload".into())
}

pub(super) fn byte_result(message: &JsValue) -> Result<Result<Vec<u8>, String>, String> {
    byte_payload(message).map(|result| result.map(|bytes| bytes.to_vec()))
}

// Inspect the envelope without copying buffers for callers that only forward it.
fn byte_payload(message: &JsValue) -> Result<Result<js_sys::Uint8Array, String>, String> {
    let error = field(message, "error");
    let payload = field(message, "bytes");
    let payload = if payload.is_undefined() { None } else { Some(payload.dyn_into::<js_sys::Uint8Array>().map_err(|_| "Expected Uint8Array payload".to_owned())?) };
    let error = if error.is_undefined() {
        None
    } else {
        if error.as_string().is_none() && field(&error, "message").as_string().is_none() {
            return Err("Expected error message".into());
        }
        Some(error_message(&error))
    };
    client_platform_runtime::protocol::reply(payload, error)
}

pub struct CpuRequest {
    pub id: u32,
    pub bytes: Vec<u8>,
    pub source: JsValue,
}
impl CpuRequest {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        let source = source(message)?;
        Ok(Self { id: request_id(message)?, bytes: bytes(&field(message, "bytes"))?, source })
    }
    pub fn encode(self) -> (JsValue, js_sys::Array) {
        let (message, transfer) = super::byte_reply(&[("id", self.id.into()), ("source", self.source.clone())], Ok(self.bytes));
        if !self.source.is_undefined() {
            transfer.push(&field(&self.source, "port"));
        }
        (message, transfer)
    }
}

pub enum CpuEvent {
    Ready,
    Failed(String),
    Reply { id: u32, result: Result<Vec<u8>, String> },
}
impl CpuEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        if field(message, "transport").as_string().as_deref() == Some("endpoint_failed") {
            return Ok(Self::Failed(field(message, "error").as_string().ok_or("Missing endpoint error")?));
        }
        if field(message, "ready").as_bool() == Some(true) {
            return Ok(Self::Ready);
        }
        Ok(Self::Reply { id: request_id(message)?, result: byte_result(message)? })
    }
}

pub enum SyncEvent {
    Cancel,
    Reply { id: u32, result: Result<Vec<u8>, String> },
}
impl SyncEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        if field(message, "kind").as_string().as_deref() == Some("cancel") {
            return Ok(Self::Cancel);
        }
        Ok(Self::Reply { id: request_id(message)?, result: byte_result(message)? })
    }
}

pub enum TransferEvent {
    Cancel,
    Complete,
    Progress(f64),
}
impl TransferEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match field(message, "kind").as_string().as_deref() {
            Some("cancel") => Ok(Self::Cancel),
            Some("progress") => Ok(Self::Progress(progress_fraction(message)?)),
            Some("result") => {
                let _ = byte_payload(message)?;
                Ok(Self::Complete)
            }
            Some("error") => {
                failure(message)?;
                Ok(Self::Complete)
            }
            _ => Err("Invalid transfer response".into()),
        }
    }
}

/// Commands from the coordinator to a caller-owned storage/transfer capability.
pub enum BridgeEvent {
    Complete(Result<Vec<u8>, String>),
    Storage { id: u32, bytes: Vec<u8> },
    Run,
    Progress(f64),
}
impl BridgeEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match field(message, "kind").as_string().as_deref() {
            Some("done" | "result") => Ok(Self::Complete(byte_result(message)?)),
            Some("error") => Ok(Self::Complete(Err(failure(message)?))),
            Some("storage") => Ok(Self::Storage { id: request_id(message)?, bytes: bytes(&field(message, "bytes"))? }),
            Some("run") => Ok(Self::Run),
            Some("progress") => Ok(Self::Progress(progress_fraction(message)?)),
            _ => Err("Invalid worker bridge message".into()),
        }
    }
}

fn progress_fraction(message: &JsValue) -> Result<f64, String> {
    let fraction = field(message, "fraction").as_f64().ok_or("Missing transfer progress")?;
    if fraction.is_finite() && (0.0..=1.0).contains(&fraction) {
        Ok(fraction)
    } else {
        Err("Invalid transfer progress".into())
    }
}

pub enum ImportCommand {
    Initialize(web_sys::MessagePort),
    Submit { id: String, job: JsValue },
}
impl ImportCommand {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match field(message, "transport").as_string().as_deref() {
            Some("initialize_import") => field(message, "database").dyn_into().map(Self::Initialize).map_err(|_| "Missing import database port".into()),
            Some("import_submit") => {
                let job = field(message, "job");
                let id = field(&job, "id").as_string().filter(|id| !id.is_empty()).ok_or("Missing import job id")?;
                Ok(Self::Submit { id, job })
            }
            _ => Err("Invalid import command".into()),
        }
    }
}

fn failure(message: &JsValue) -> Result<String, String> {
    match byte_payload(message)? {
        Err(error) => Ok(error),
        Ok(_) => Err("Error reply contains a success payload".into()),
    }
}

pub enum WorkerRequest {
    Cpu { bytes: Vec<u8>, source: JsValue },
    Sync { key: String, bytes: Vec<u8> },
    Transfer { key: String },
}
impl WorkerRequest {
    /// The reply port is validated separately so malformed requests can be rejected.
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        let key = || field(message, "key").as_string().filter(|key| !key.is_empty()).ok_or_else(|| "Missing worker job key".to_owned());
        match field(message, "transport").as_string().as_deref() {
            Some("cpu_request") => {
                let source = source(message)?;
                Ok(Self::Cpu { bytes: bytes(&field(message, "bytes"))?, source })
            }
            Some("sync_request") => Ok(Self::Sync { key: key()?, bytes: bytes(&field(message, "bytes"))? }),
            Some("transfer_request") => Ok(Self::Transfer { key: key()? }),
            _ => Err("Invalid worker request".into()),
        }
    }
}

fn source(message: &JsValue) -> Result<JsValue, String> {
    let source = field(message, "source");
    if !source.is_undefined() && !field(&source, "port").is_instance_of::<web_sys::MessagePort>() {
        return Err("Missing book source capability".into());
    }
    Ok(source)
}
