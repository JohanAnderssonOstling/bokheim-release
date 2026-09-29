use crate::{AudiobookBook, PlaybackPosition};
use std::time::Duration;
use wasm_bindgen::{closure::Closure, prelude::*};

#[wasm_bindgen(module = "/src/web_audio.js")]
extern "C" {
    type BrowserAudio;
    #[wasm_bindgen(constructor, catch)]
    fn new(url: &str, artwork: &[u8], metadata: &JsValue, initial: f64, save: &js_sys::Function, direct: &JsValue, renew: &JsValue) -> Result<BrowserAudio, JsValue>;
    #[wasm_bindgen(method)]
    fn play(this: &BrowserAudio);
    #[wasm_bindgen(method)]
    fn toggle(this: &BrowserAudio);
    #[wasm_bindgen(method)]
    fn pause(this: &BrowserAudio);
    #[wasm_bindgen(method)]
    fn seek(this: &BrowserAudio, position: f64);
    #[wasm_bindgen(method, js_name = setRate)]
    fn set_rate(this: &BrowserAudio, rate: f64);
    #[wasm_bindgen(method, js_name = setVolume)]
    fn set_volume(this: &BrowserAudio, volume: f32);
    #[wasm_bindgen(method)]
    fn position(this: &BrowserAudio) -> f64;
    #[wasm_bindgen(method, js_name = confirmedPosition)]
    fn confirmed_position(this: &BrowserAudio) -> Option<f64>;
    #[wasm_bindgen(method, js_name = lastConfirmedPosition)]
    fn last_confirmed_position(this: &BrowserAudio) -> f64;
    #[wasm_bindgen(method)]
    fn buffering(this: &BrowserAudio) -> bool;
    #[wasm_bindgen(method)]
    fn playing(this: &BrowserAudio) -> bool;
    #[wasm_bindgen(method)]
    fn rate(this: &BrowserAudio) -> f64;
    #[wasm_bindgen(method, js_name = takeError)]
    fn take_error(this: &BrowserAudio) -> Option<String>;
    #[wasm_bindgen(method)]
    fn failed(this: &BrowserAudio) -> bool;
    #[wasm_bindgen(method)]
    fn dispose(this: &BrowserAudio);
}

pub(crate) struct AudioEngine {
    browser: BrowserAudio,
    // Keep the callback valid until dispose has detached browser event handlers.
    _save: Closure<dyn FnMut(f64, f64)>,
    _source: app::BrowserAudiobook,
}

impl AudioEngine {
    pub fn open(source: app::BrowserAudiobook, book: &AudiobookBook, initial: Duration, updates: async_channel::Sender<PlaybackPosition>) -> Result<Self, String> {
        let metadata = js_sys::JSON::parse(&serde_json::json!({"title": book.title, "artist": book.author.clone().unwrap_or_default(), "tracks": source.metadata.tracks, "localTrackUrls": source.local_tracks, "bookDuration": book.duration.as_secs_f64()}).to_string()).map_err(|e| format!("{e:?}"))?;
        let save = Closure::new(move |position: f64, progress: f64| {
            if let Some(position) = crate::browser_position_update(position, progress) {
                let _ = updates.try_send(position);
            }
        });
        let browser =
            BrowserAudio::new(&source.url, book.cover.as_ref().map(|cover| cover.bytes()).unwrap_or_default(), &metadata, initial.min(book.duration).as_secs_f64(), save.as_ref().unchecked_ref(), &source.direct_config()?, &source.renewal())
                .map_err(|e| format!("Could not initialize browser audio: {e:?}"))?;
        Ok(Self { browser, _save: save, _source: source })
    }
    pub fn play(&self) {
        self.browser.play();
    }
    pub fn pause(&self) {
        self.browser.pause();
    }
    pub fn toggle(&self) -> Result<(), String> {
        self.browser.toggle();
        Ok(())
    }
    pub fn seek_to(&self, position: Duration) -> Result<(), String> {
        self.browser.seek(position.as_secs_f64());
        Ok(())
    }
    pub fn set_tempo(&mut self, tempo: f32) -> Result<(), String> {
        self.browser.set_rate(tempo as f64);
        Ok(())
    }
    pub fn set_volume(&self, volume: f32) {
        self.browser.set_volume(volume);
    }
    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.browser.position().max(0.0))
    }
    pub fn is_playing(&self) -> bool {
        self.browser.playing()
    }
    pub fn confirmed_position(&self) -> Option<Duration> {
        self.browser.confirmed_position().and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
    }
    pub fn last_confirmed_position(&self) -> Duration {
        Duration::try_from_secs_f64(self.browser.last_confirmed_position()).unwrap_or_default()
    }
    pub fn is_buffering(&self) -> bool {
        self.browser.buffering()
    }
    pub fn rate(&self) -> f64 {
        self.browser.rate()
    }
    pub fn take_error(&self) -> Option<String> {
        self.browser.take_error()
    }
    pub fn has_failed(&self) -> bool {
        self.browser.failed()
    }
    pub fn stop(&self) {
        self.browser.dispose();
    }
}
impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.browser.dispose();
    }
}
