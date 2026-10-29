//! Browser audiobook presentation state and lifetime leases.
use crate::transport::media::RemoteMedia;
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
    ranges: Vec<RemoteMedia>,
}
impl<M, D> BrowserAudiobook<M, D>
where
    D: serde::Serialize,
{
    pub fn new(metadata: M, url: String, local_tracks: Vec<String>, direct: Option<D>, lease: Option<async_channel::Receiver<()>>, renew: Option<AsyncCallback>) -> Self {
        Self { metadata, url, local_tracks, direct, renew, _lease: lease, ranges: Vec::new() }
    }
    pub fn with_ranges(mut self, ranges: Vec<RemoteMedia>) -> Self {
        self.ranges = ranges;
        self
    }
    pub fn pause_control(&self) -> Option<Closure<dyn FnMut(bool)>> {
        RemoteMedia::pause_control(&self.ranges)
    }
    pub fn renewal(&self) -> JsValue {
        self.renew.as_ref().map(|renew| renew.value()).unwrap_or(JsValue::UNDEFINED)
    }
    pub fn direct_config(&self) -> Result<JsValue, String> {
        let Some(config) = &self.direct else { return Ok(JsValue::UNDEFINED) };
        let json = serde_json::to_string(config).map_err(|error| format!("Invalid playback configuration: {error}"))?;
        js_sys::JSON::parse(&json).map_err(|_| "Invalid playback configuration".into())
    }
}
