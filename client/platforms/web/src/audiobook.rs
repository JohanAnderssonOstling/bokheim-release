//! Browser audiobook presentation state and lifetime leases.
use crate::transport::resources::{AsyncCallback, LocalMediaLease};
use wasm_bindgen::prelude::*;

/// Keeps browser-owned local media alive while the player uses its object URL.
pub struct PlaybackLease {
    _local: Option<LocalMediaLease>,
}
impl PlaybackLease {
    pub fn local(local: Option<LocalMediaLease>) -> Self {
        Self { _local: local }
    }
}

/// Browser playback state parameterized by the library's domain metadata.
pub struct BrowserAudiobook<M, D> {
    pub metadata: M,
    pub url: String,
    pub local_tracks: Vec<String>,
    pub direct: Option<D>,
    renew: Option<AsyncCallback>,
    _lease: Option<async_channel::Receiver<()>>,
}
impl<M, D> BrowserAudiobook<M, D>
where
    D: serde::Serialize,
{
    pub fn new(metadata: M, url: String, local_tracks: Vec<String>, direct: Option<D>, lease: Option<async_channel::Receiver<()>>, renew: Option<AsyncCallback>) -> Self {
        Self { metadata, url, local_tracks, direct, renew, _lease: lease }
    }
    pub fn renewal(&self) -> JsValue {
        self.renew.as_ref().map(|renew| renew.value()).unwrap_or(JsValue::UNDEFINED)
    }
    pub fn direct_config(&self) -> Result<JsValue, String> {
        self.direct.as_ref().map(|config| js_sys::JSON::parse(&serde_json::to_string(config).unwrap()).map_err(|_| "Invalid playback configuration".into())).unwrap_or(Ok(JsValue::UNDEFINED))
    }
}
