//! Android UI transport. Session IDs prevent a dismissed screen receiving a
//! later account response. Credentials exist only in the bounded request queue.
use async_channel::{Receiver, Sender};
use serde::Deserialize;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};
type Transport = dyn Fn(&str) -> Result<(), String> + Send + Sync;
static TRANSPORT: OnceLock<Arc<Transport>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE: Mutex<Option<(u64, Sender<Request>)>> = Mutex::new(None);

#[derive(Deserialize)]
pub(crate) struct Request {
    pub action: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub token: String,
}

pub fn initialize(transport: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) {
    let _ = TRANSPORT.set(Arc::new(transport));
}

/// Called by JNI; never log the message because it contains credentials.
pub fn receive(id: u64, message: &str) {
    let Ok(request) = serde_json::from_str::<Request>(message) else {
        return;
    };
    let active = ACTIVE.lock().unwrap();
    if let Some((active_id, sender)) = active.as_ref() {
        if *active_id == id {
            let _ = sender.try_send(request);
        }
    }
}
fn send(message: serde_json::Value) -> Result<(), String> {
    TRANSPORT.get().ok_or("Android authentication is unavailable")?(&message.to_string())
}
pub(crate) struct Session(pub u64);
impl Session {
    pub fn open(colors: Vec<u32>) -> Result<(Self, Receiver<Request>), String> {
        let session = Self(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        let (sender, receiver) = async_channel::bounded(2);
        *ACTIVE.lock().unwrap() = Some((session.0, sender));
        send(serde_json::json!({"command":"open", "id":session.0, "colors":colors, "minimum":account_contract::PASSWORD_MIN_CHARACTERS}))?;
        Ok((session, receiver))
    }
    pub fn complete(id: u64, error: Option<String>) {
        let _ = send(serde_json::json!({"command":"result", "id":id, "error":error}));
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap();
        if active.as_ref().is_some_and(|(id, _)| *id == self.0) {
            *active = None;
        }
        drop(active);
        let _ = send(serde_json::json!({"command":"close", "id":self.0}));
    }
}
