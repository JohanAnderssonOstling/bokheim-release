//! Validated coordinator, host and streaming control messages.
use super::{field, protocol::request_id};
use wasm_bindgen::{JsCast, JsValue};

fn text(message: &JsValue, name: &str) -> Result<String, String> {
    field(message, name).as_string().ok_or_else(|| format!("Missing {name}"))
}
fn boolean(message: &JsValue, name: &str) -> Result<bool, String> {
    field(message, name).as_bool().ok_or_else(|| format!("Missing {name}"))
}
pub fn milliseconds(value: JsValue) -> Result<u32, String> {
    let value = value.as_f64().ok_or("Missing duration")?;
    client_platform_runtime::protocol::milliseconds(value)
}
pub enum BackgroundEvent {
    Stop,
    Wake,
    WakeAfter(u32),
    Refresh,
    Result { id: u32, success: bool },
}
impl BackgroundEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match text(message, "kind")?.as_str() {
            "stop" => Ok(Self::Stop),
            "wake" => Ok(Self::Wake),
            "refresh" => Ok(Self::Refresh),
            "wake_after" => Ok(Self::WakeAfter(milliseconds(field(message, "delay"))?)),
            "cycle_result" => Ok(Self::Result { id: request_id(message)?, success: boolean(message, "success")? }),
            _ => Err("Invalid background message".into()),
        }
    }
}
pub enum HostEvent {
    Closed,
    Register { lock: String, port: web_sys::MessagePort },
    WorkerFailed { id: u32, error: String },
    Application,
}
impl HostEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match text(message, "transport")?.as_str() {
            "host_closed" => Ok(Self::Closed),
            "worker_failed" => Ok(Self::WorkerFailed { id: request_id(message)?, error: text(message, "error")? }),
            "register_host" => {
                let lock = text(message, "lock")?;
                let port = field(message, "port").dyn_into().map_err(|_| "Missing host port")?;
                Ok(Self::Register { lock, port })
            }
            _ => Ok(Self::Application),
        }
    }
}
pub enum NetworkCommand {
    Pull,
    Cancel,
}
impl NetworkCommand {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match text(message, "kind")?.as_str() {
            "pull" => Ok(Self::Pull),
            "cancel" => Ok(Self::Cancel),
            _ => Err("Invalid network command".into()),
        }
    }
}
pub enum NetworkEvent {
    Error(String),
    Headers { has_body: bool },
    Chunk(JsValue),
    Done,
}
impl NetworkEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match text(message, "kind")?.as_str() {
            "error" => Ok(Self::Error(text(message, "error")?)),
            "done" => Ok(Self::Done),
            "chunk" => {
                let payload = field(message, "bytes");
                // Validate without coercing arbitrary objects into byte arrays.
                if !payload.is_instance_of::<js_sys::Uint8Array>() {
                    return Err("Invalid network chunk".into());
                }
                Ok(Self::Chunk(payload))
            }
            "headers" => {
                let status = field(message, "status").as_f64().ok_or("Missing HTTP status")?;
                if !(200.0..=599.0).contains(&status) || status.fract() != 0.0 {
                    return Err("Invalid HTTP status".into());
                }
                text(message, "statusText")?;
                text(message, "url")?;
                boolean(message, "redirected")?;
                let headers = field(message, "headers");
                if !js_sys::Array::is_array(&headers) {
                    return Err("Invalid HTTP headers".into());
                }
                for header in js_sys::Array::from(&headers).iter() {
                    if !js_sys::Array::is_array(&header) {
                        return Err("Invalid HTTP header".into());
                    }
                    let pair = js_sys::Array::from(&header);
                    if pair.length() != 2 || pair.get(0).as_string().is_none() || pair.get(1).as_string().is_none() {
                        return Err("Invalid HTTP header".into());
                    }
                }
                Ok(Self::Headers { has_body: boolean(message, "hasBody")? })
            }
            _ => Err("Invalid network response".into()),
        }
    }
}
pub struct NetworkRequest {
    pub url: String,
    pub options: js_sys::Object,
    pub body: JsValue,
}
impl NetworkRequest {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        let url = text(message, "url")?;
        let options = field(message, "options");
        if !options.is_object() || options.is_null() || js_sys::Array::is_array(&options) {
            return Err("Invalid fetch options".into());
        }
        let body = field(message, "body");
        Ok(Self { url, options: options.unchecked_into(), body })
    }
}

pub enum BackgroundReply {
    Stopped,
    Cycle { id: u32, refresh: bool },
}
impl BackgroundReply {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        match text(message, "kind")?.as_str() {
            "stopped" => Ok(Self::Stopped),
            "cycle" => Ok(Self::Cycle { id: request_id(message)?, refresh: boolean(message, "refresh_remote")? }),
            _ => Err("Invalid background reply".into()),
        }
    }
}
pub enum TabEvent {
    Failed,
    Stop(u32),
    Create { id: u32, role: String, port: web_sys::MessagePort },
    Application,
}
impl TabEvent {
    pub fn decode(message: &JsValue) -> Result<Self, String> {
        if field(message, "kind").as_string().as_deref() == Some("failed") {
            text(message, "error")?;
            return Ok(Self::Failed);
        }
        match field(message, "transport").as_string().as_deref() {
            Some("stop_worker") => Ok(Self::Stop(request_id(message)?)),
            Some("create_worker") => Ok(Self::Create { id: request_id(message)?, role: text(message, "role")?, port: field(message, "port").dyn_into().map_err(|_| "Missing worker endpoint")? }),
            _ => Ok(Self::Application),
        }
    }
}
