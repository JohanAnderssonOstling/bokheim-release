//! Offline synthesis and playback on desktop and Android.
#[cfg(any(target_os = "android", test))]
#[path = "android.rs"]
pub mod android;
#[path = "audio_cache.rs"]
mod audio_cache;
#[path = "model.rs"]
mod model;
#[path = "output.rs"]
mod output;
#[path = "prefetch.rs"]
mod prefetch;
#[path = "speech_text.rs"]
mod speech_text;

use crate::{FIRST_CHUNK_CHARS, MAX_CHUNK_CHARS, Pauses, SpeechChunk, SpeechControl, SpeechEvent, SpeechPassage, next_chunk_with_limit};
pub use model::{Voice, load_model, load_model_with_threads, load_voice, load_voice_with_threads, model_directory, set_model_preparer, voice_directory};

use async_channel::{Receiver, Sender};
use audio_cache::{AudioCache, AudioKey};
use output::OutputControl;
use prefetch::TextPrefetch;
use rodio::{Player, Source};
use sherpa_onnx::{GenerationConfig, OfflineTts};
use speech_text::prepare_speech_text;
use std::mem::take;
use std::num::{NonZeroU16, NonZeroU32};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, Weak,
    atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

const VOICE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const AUDIO_BUFFER_SECONDS: usize = 60;

/// The installed speech model supports English. A missing language declaration defaults to English.
pub fn supports_languages(languages: &[String]) -> bool {
    let mut declared = languages.iter().map(|language| language.trim()).filter(|language| !language.is_empty()).peekable();
    if declared.peek().is_none() {
        return true;
    }
    declared.any(|language| {
        let base = language.split(['-', '_']).next().unwrap_or(language);
        base.eq_ignore_ascii_case("en") || base.eq_ignore_ascii_case("eng")
    })
}

enum Command {
    Replace {
        generation: u64,
        text: Vec<SpeechPassage>,
        start: usize,
    },
    Read {
        generation: u64,
        count: usize,
        start: usize,
        glyph_start: u32,
    },
    Chunk {
        generation: u64,
        index: usize,
        glyph_start: u32,
        chunk: Option<SpeechChunk>,
    },
    Skip {
        generation: u64,
        index: usize,
    },
    Stop {
        generation: u64,
    },
    Release {
        generation: u64,
    },
    OutputFailed {
        generation: u64,
        message: String,
    },
    Wake,
    #[cfg(test)]
    Shutdown,
    #[cfg(test)]
    Synchronize(mpsc::Sender<()>),
    #[cfg(test)]
    Hold {
        ready: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
}

struct Shared {
    generation: AtomicU64,
    paused: AtomicBool,
    // Logical reading item; pause/resume position belongs to the retained PCM source.
    cursor: AtomicUsize,
    count: AtomicUsize,
    rate: AtomicU32,
    end: AtomicUsize,
    pauses: Mutex<Pauses>,
    voice: Mutex<Voice>,
    player: Mutex<Option<Arc<Player>>>,
    output: Mutex<Option<Arc<dyn OutputControl>>>,
    owner: Mutex<u64>,
    route: Mutex<Option<Arc<SpeechClient>>>,
}

impl Shared {
    /// Cancel stale PCM before recovering a lock. Each field still contains a
    /// valid Rust value, but a partially completed handoff must not keep playing.
    fn lock<'a, T>(&self, mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
        mutex.lock().unwrap_or_else(|error| {
            self.fail("Speech state was interrupted. Start reading aloud again to retry.");
            mutex.clear_poison();
            error.into_inner()
        })
    }

    fn fail(&self, message: &str) {
        self.paused.store(true, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        // This can run while another speech lock is held, including route.
        // Never wait for that lock while reporting a failure.
        if let Ok(route) = self.route.try_lock() {
            if let Some(client) = route.as_ref() {
                let _ = client.events.try_send(SpeechEvent::Failed { generation: client.generation.load(Ordering::Acquire), message: message.into() });
            }
        }
        eprintln!("{message}");
    }

    fn update_output(&self, commands: &mpsc::Sender<Command>) {
        let output = self.lock(&self.output).clone();
        if let Some(output) = output {
            if self.paused.load(Ordering::Acquire) {
                output.pause();
            } else if let Err(message) = output.resume() {
                self.paused.store(true, Ordering::Release);
                let _ = commands.send(Command::OutputFailed { generation: self.generation.load(Ordering::Acquire), message });
            }
        }
    }
    fn publish_state(&self) {
        #[cfg(target_os = "android")]
        android::changed_for(self);
    }
}

/// Registry entries and settings snapshots can be retained after unwinding;
/// they do not contain an in-progress audio ownership transition.
fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| {
        eprintln!("Recovering interrupted speech registry state");
        mutex.clear_poison();
        error.into_inner()
    })
}

#[derive(Clone)]
enum SpeechEvents {
    Routed(Weak<Shared>),
    #[cfg(test)]
    Direct(Sender<SpeechEvent>),
}

impl SpeechEvents {
    fn try_send(&self, event: SpeechEvent) -> Result<(), async_channel::TrySendError<SpeechEvent>> {
        match self {
            #[cfg(test)]
            Self::Direct(sender) => sender.try_send(event),
            Self::Routed(shared) => {
                let Some(shared) = shared.upgrade() else { return Ok(()) };
                let route = match shared.route.lock() {
                    Ok(route) => route,
                    Err(error) => {
                        let message = "Speech event routing was interrupted. Start reading aloud again to retry.";
                        shared.fail(message);
                        let route = error.into_inner();
                        shared.route.clear_poison();
                        if let Some(client) = route.as_ref() {
                            let _ = client.events.try_send(SpeechEvent::Failed { generation: client.generation.load(Ordering::Acquire), message: message.into() });
                        }
                        return Ok(());
                    }
                };
                let Some(client) = route.as_ref() else { return Ok(()) };
                let generation = match &event {
                    SpeechEvent::Requested { generation, .. } | SpeechEvent::Started { generation, .. } | SpeechEvent::Finished { generation } | SpeechEvent::Failed { generation, .. } | SpeechEvent::Remote { generation, .. } => *generation,
                };
                if generation != client.generation.load(Ordering::Acquire) {
                    return Ok(());
                }
                if let SpeechEvent::Remote { command: SpeechControl::Item { generation, .. }, .. } = &event {
                    client.generation.store(*generation, Ordering::Release);
                }
                client.events.try_send(event)
            }
        }
    }
}

#[cfg(test)]
impl From<Sender<SpeechEvent>> for SpeechEvents {
    fn from(sender: Sender<SpeechEvent>) -> Self {
        Self::Direct(sender)
    }
}

struct ClientSettings {
    title: String,
    #[cfg(target_os = "android")]
    metadata: android::MediaMetadata,
    rate: f32,
    pauses: Pauses,
    voice: Voice,
}

struct SpeechClient {
    id: u64,
    generation: AtomicU64,
    settings: Mutex<ClientSettings>,
    events: Sender<SpeechEvent>,
    #[cfg(target_os = "android")]
    android_id: u64,
}

struct SpeechRuntime {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}
static RUNTIME: OnceLock<Mutex<Option<SpeechRuntime>>> = OnceLock::new();
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

impl SpeechRuntime {
    fn new() -> Result<Self, String> {
        let (commands, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            paused: AtomicBool::new(true),
            cursor: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
            rate: AtomicU32::new(1.0_f32.to_bits()),
            end: AtomicUsize::new(usize::MAX),
            pauses: Mutex::new(Pauses::NATURAL),
            voice: Mutex::new(Voice::Alba),
            player: Mutex::new(None),
            output: Mutex::new(None),
            owner: Mutex::new(0),
            route: Mutex::new(None),
        });
        let events = SpeechEvents::Routed(Arc::downgrade(&shared));
        let worker_shared = shared.clone();
        let wake = commands.clone();
        thread::Builder::new()
            .name("reader-tts".into())
            .spawn(move || {
                if catch_unwind(AssertUnwindSafe(|| worker(receiver, wake, events, worker_shared.clone()))).is_err() {
                    worker_shared.fail("The speech worker stopped unexpectedly. Reopen the reader to retry.");
                    let player = recover_lock(&worker_shared.player).take();
                    let output = recover_lock(&worker_shared.output).take();
                    if let Some(player) = player {
                        player.stop();
                    }
                    drop(output);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { commands, shared })
    }
}

/// A reader's control and event endpoint for the one process-wide speech worker.
/// Starting a different reader stops the previous session without loading another voice.
pub struct DesktopSpeech {
    commands: mpsc::Sender<Command>,
    events: Receiver<SpeechEvent>,
    shared: Arc<Shared>,
    client: Arc<SpeechClient>,
}

impl DesktopSpeech {
    pub fn new() -> Result<Self, String> {
        let mut runtime = recover_lock(RUNTIME.get_or_init(|| Mutex::new(None)));
        if let Some(current) = runtime.as_ref() {
            if current.commands.send(Command::Wake).is_ok() {
                return Ok(Self::for_worker(current.commands.clone(), current.shared.clone()));
            }
        }
        let next = SpeechRuntime::new()?;
        let speech = Self::for_worker(next.commands.clone(), next.shared.clone());
        *runtime = Some(next);
        Ok(speech)
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            self.shared.fail("The speech worker is unavailable. Reopen the reader to retry.");
        }
    }

    fn for_worker(commands: mpsc::Sender<Command>, shared: Arc<Shared>) -> Self {
        let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
        let (sender, events) = async_channel::unbounded();
        #[cfg(target_os = "android")]
        let android_id = android::register(&shared, commands.clone(), SpeechEvents::Routed(Arc::downgrade(&shared)), id);
        let client = Arc::new(SpeechClient {
            id,
            generation: AtomicU64::new(0),
            settings: Mutex::new(ClientSettings {
                #[cfg(target_os = "android")]
                metadata: android::MediaMetadata::default(),
                title: "Read aloud".into(),
                rate: 1.0,
                pauses: Pauses::NATURAL,
                voice: Voice::Alba,
            }),
            events: sender,
            #[cfg(target_os = "android")]
            android_id,
        });
        Self { commands, events, shared, client }
    }

    fn owned<T>(&self, action: impl FnOnce() -> T) -> Option<T> {
        let owner = self.shared.lock(&self.shared.owner);
        (*owner == self.client.id).then(action)
    }

    pub fn set_title(&self, title: &str) {
        let owner = self.shared.lock(&self.shared.owner);
        self.shared.lock(&self.client.settings).title = title.into();
        #[cfg(target_os = "android")]
        if *owner == self.client.id {
            android::set_title(self.client.android_id, title);
        }
        drop(owner);
    }

    /// The reader supplies library artwork/credits and its current chapter.
    pub fn set_media_metadata(&self, author: &str, chapter: &str, cover: &[u8]) {
        #[cfg(target_os = "android")]
        {
            let owner = self.shared.lock(&self.shared.owner);
            let mut settings = self.shared.lock(&self.client.settings);
            if settings.metadata.author == author && settings.metadata.chapter == chapter && settings.metadata.cover == cover {
                return;
            }
            settings.metadata = android::MediaMetadata { author: author.into(), chapter: chapter.into(), cover: cover.to_vec() };
            if *owner == self.client.id {
                android::set_metadata(self.client.android_id, settings.metadata.clone());
            }
        }
        #[cfg(not(target_os = "android"))]
        let _ = (author, chapter, cover);
    }

    pub fn stop(&self) {
        let mut owner = self.shared.lock(&self.shared.owner);
        if *owner != self.client.id {
            return;
        }
        self.shared.count.store(0, Ordering::Release);
        let generation = self.invalidate_audio();
        self.send(Command::Stop { generation });
        #[cfg(target_os = "android")]
        android::stop(self.client.android_id);
        self.shared.lock(&self.shared.route).take();
        *owner = 0;
    }

    pub fn events(&self) -> Receiver<SpeechEvent> {
        self.events.clone()
    }

    /// End this endpoint's session and unload its audio device and voice.
    /// An endpoint superseded by another reader cannot release that reader's
    /// resources; the worker also rejects a release overtaken by a new start.
    pub fn release_resources(&self) {
        self.stop();
        let owner = self.shared.lock(&self.shared.owner);
        let generation = self.client.generation.load(Ordering::Acquire);
        if *owner == 0 && generation == self.shared.generation.load(Ordering::Acquire) {
            self.send(Command::Release { generation });
        }
    }

    pub fn is_current(&self) -> bool {
        *self.shared.lock(&self.shared.owner) == self.client.id
    }

    pub fn replace(&self, text: Vec<String>) -> u64 {
        self.replace_from(text, 0)
    }
    pub fn replace_from(&self, text: Vec<String>, start: usize) -> u64 {
        self.replace_passages_from(text.into_iter().map(|text| SpeechPassage::new(text, false)).collect(), start)
    }
    pub fn replace_passages_from(&self, text: Vec<SpeechPassage>, start: usize) -> u64 {
        self.start(text.len(), start, |generation| Command::Replace { generation, text, start })
    }
    pub fn read_items_from(&self, count: usize, start: usize, glyph_start: u32) -> u64 {
        self.start(count, start, |generation| Command::Read { generation, count, start, glyph_start })
    }

    fn start(&self, count: usize, start: usize, command: impl FnOnce(u64) -> Command) -> u64 {
        let mut owner = self.shared.lock(&self.shared.owner);
        if *owner != self.client.id {
            let previous = self.shared.lock(&self.shared.route).take();
            if let Some(previous) = previous {
                #[cfg(target_os = "android")]
                android::stop(previous.android_id);
                let _ = previous.events.try_send(SpeechEvent::Remote { generation: previous.generation.load(Ordering::Acquire), command: SpeechControl::Stop });
            }
            *owner = self.client.id;
            *self.shared.lock(&self.shared.route) = Some(self.client.clone());
        }
        let generation = self.invalidate_audio();
        let settings = self.shared.lock(&self.client.settings);
        self.shared.rate.store(settings.rate.to_bits(), Ordering::Release);
        *self.shared.lock(&self.shared.pauses) = settings.pauses;
        *self.shared.lock(&self.shared.voice) = settings.voice;
        self.shared.end.store(usize::MAX, Ordering::Release);
        self.shared.cursor.store(start, Ordering::Release);
        self.shared.count.store(count, Ordering::Release);
        self.shared.paused.store(false, Ordering::Release);
        #[cfg(target_os = "android")]
        {
            android::set_title(self.client.android_id, &settings.title);
            android::set_metadata(self.client.android_id, settings.metadata.clone());
            if let Err(message) = android::start(self.client.android_id) {
                self.shared.paused.store(true, Ordering::Release);
                android::failed(self.client.android_id, generation, message);
                return generation;
            }
        }
        drop(settings);
        self.send(command(generation));
        generation
    }

    pub fn provide_chunk(&self, generation: u64, index: usize, glyph_start: u32, chunk: Option<SpeechChunk>) {
        self.owned(|| {
            if generation == self.client.generation.load(Ordering::Acquire) {
                self.send(Command::Chunk { generation, index, glyph_start, chunk });
            }
        });
    }

    pub fn skip_to(&self, index: usize) -> u64 {
        self.owned(|| {
            let paused = self.shared.paused.load(Ordering::Acquire);
            let generation = self.invalidate_audio();
            self.shared.cursor.store(index, Ordering::Release);
            self.shared.paused.store(paused, Ordering::Release);
            self.send(Command::Skip { generation, index });
            generation
        })
        .unwrap_or_else(|| self.client.generation.load(Ordering::Acquire))
    }

    pub fn set_end_fragment(&self, end: Option<usize>) -> u64 {
        self.owned(|| {
            self.shared.end.store(end.unwrap_or(usize::MAX), Ordering::Release);
            self.send(Command::Wake);
        });
        self.client.generation.load(Ordering::Acquire)
    }

    pub fn cancel(&self) -> u64 {
        self.owned(|| {
            self.shared.count.store(0, Ordering::Release);
            self.invalidate_audio()
        })
        .unwrap_or_else(|| self.client.generation.load(Ordering::Acquire))
    }

    fn invalidate_audio(&self) -> u64 {
        let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.client.generation.store(generation, Ordering::Release);
        self.shared.paused.store(true, Ordering::Release);
        if let Some(player) = self.shared.lock(&self.shared.player).as_ref() {
            player.stop();
            player.pause();
        }
        generation
    }

    pub fn set_rate(&self, rate: f32) -> u64 {
        let owner = self.shared.lock(&self.shared.owner);
        let rate = rate.clamp(0.5, 3.0);
        let mut settings = self.shared.lock(&self.client.settings);
        if settings.rate == rate {
            return self.client.generation.load(Ordering::Acquire);
        }
        settings.rate = rate;
        if *owner == self.client.id {
            self.shared.rate.store(rate.to_bits(), Ordering::Release);
            self.restart_current();
            self.shared.publish_state();
        }
        self.client.generation.load(Ordering::Acquire)
    }

    // The owner lock serializes settings changes and reader handoffs. A longer
    // queue must not keep playing old settings after the user changes them.
    fn restart_current(&self) {
        if self.shared.count.load(Ordering::Acquire) > 0 {
            let paused = self.shared.paused.load(Ordering::Acquire);
            let index = self.shared.cursor.load(Ordering::Acquire);
            let generation = self.invalidate_audio();
            self.shared.paused.store(paused, Ordering::Release);
            self.send(Command::Skip { generation, index });
        }
    }

    /// Replace the active voice at the current logical passage, preserving pause state.
    pub fn set_voice(&self, voice: Voice) -> u64 {
        let owner = self.shared.lock(&self.shared.owner);
        let mut settings = self.shared.lock(&self.client.settings);
        if settings.voice == voice {
            return self.client.generation.load(Ordering::Acquire);
        }
        settings.voice = voice;
        if *owner == self.client.id {
            *self.shared.lock(&self.shared.voice) = voice;
            self.restart_current();
        }
        self.client.generation.load(Ordering::Acquire)
    }

    pub fn set_pauses(&self, pauses: Pauses) -> u64 {
        let owner = self.shared.lock(&self.shared.owner);
        let mut settings = self.shared.lock(&self.client.settings);
        if settings.pauses == pauses {
            return self.client.generation.load(Ordering::Acquire);
        }
        settings.pauses = pauses;
        if *owner == self.client.id {
            *self.shared.lock(&self.shared.pauses) = pauses;
            self.restart_current();
        }
        self.client.generation.load(Ordering::Acquire)
    }

    pub fn pause(&self) {
        self.owned(|| {
            self.shared.paused.store(true, Ordering::Release);
            if let Some(player) = self.shared.lock(&self.shared.player).as_ref() {
                player.pause();
            }
            self.shared.update_output(&self.commands);
            self.shared.publish_state();
        });
    }

    pub fn resume(&self) {
        self.owned(|| {
            self.shared.paused.store(false, Ordering::Release);
            self.send(Command::Wake);
            if let Some(player) = self.shared.lock(&self.shared.player).as_ref() {
                player.play();
            }
            self.shared.update_output(&self.commands);
            self.shared.publish_state();
        });
    }

    #[cfg(test)]
    fn test_handle(commands: mpsc::Sender<Command>, sender: Sender<SpeechEvent>, events: Receiver<SpeechEvent>, shared: Arc<Shared>) -> Self {
        let mut speech = Self::for_worker(commands, shared);
        speech.events = events;
        Arc::get_mut(&mut speech.client).unwrap().events = sender;
        speech.client.generation.store(speech.shared.generation.load(Ordering::Acquire), Ordering::Release);
        *speech.shared.owner.lock().unwrap() = speech.client.id;
        *speech.shared.route.lock().unwrap() = Some(speech.client.clone());
        speech
    }
}

impl Drop for DesktopSpeech {
    fn drop(&mut self) {
        self.release_resources();
        #[cfg(target_os = "android")]
        android::remove(self.client.android_id);
    }
}

/// Set timing during synthesis. sherpa-onnx multiplies the length of each
/// pause in the generated audio by `silence_scale`; 1.0 keeps the voice's own.
pub fn narration_config(speed: f32, pauses: Pauses) -> GenerationConfig {
    GenerationConfig { speed: speed.clamp(0.5, 3.0), silence_scale: pauses.sentence_scale, ..Default::default() }
}

#[derive(Clone)]
struct AudioChunk {
    samples: Arc<Vec<f32>>,
    rate: NonZeroU32,
    last: bool,
}

/// Share synthesis allocations with the reuse cache. Passage gaps occupy no PCM memory.
struct OwnedPcm {
    samples: Arc<Vec<f32>>,
    offset: usize,
    silence: usize,
    rate: NonZeroU32,
    duration: Duration,
}

impl OwnedPcm {
    fn new(samples: impl Into<Arc<Vec<f32>>>, rate: NonZeroU32, silence: usize) -> Self {
        let samples = samples.into();
        let duration = Duration::from_secs_f64((samples.len() + silence) as f64 / f64::from(rate.get()));
        Self { samples, offset: 0, silence, rate, duration }
    }
}

impl Iterator for OwnedPcm {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if let Some(&sample) = self.samples.get(self.offset) {
            self.offset += 1;
            if self.offset == self.samples.len() {
                // Release playback ownership even during a long gap. The cache
                // may retain the same allocation for subsequent navigation.
                self.samples = Arc::new(Vec::new());
                self.offset = 0;
            }
            Some(sample)
        } else if self.silence > 0 {
            self.silence -= 1;
            Some(0.0)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.samples.len() - self.offset + self.silence;
        (remaining, Some(remaining))
    }
}

impl Source for OwnedPcm {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> NonZeroU16 {
        NonZeroU16::MIN
    }
    fn sample_rate(&self) -> NonZeroU32 {
        self.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        Some(self.duration)
    }
}

trait SpeechEngine {
    fn generate(&self, text: &str, config: &GenerationConfig, shared: &Arc<Shared>, generation: u64, emit: impl FnMut(AudioChunk) -> bool + 'static) -> Result<(), String>;
}

impl SpeechEngine for OfflineTts {
    fn generate(&self, text: &str, config: &GenerationConfig, shared: &Arc<Shared>, generation: u64, mut emit: impl FnMut(AudioChunk) -> bool + 'static) -> Result<(), String> {
        let current = shared.clone();
        let rate = u32::try_from(self.sample_rate()).ok().and_then(NonZeroU32::new).ok_or("Invalid speech sample rate.")?;
        let emitted = Arc::new(AtomicUsize::new(0));
        let emitted_count = emitted.clone();
        let audio = self
            .generate_with_config(
                text,
                config,
                Some(move |samples: &[f32], progress: f32| {
                    if generation != current.generation.load(Ordering::Acquire) {
                        return false;
                    }
                    if samples.is_empty() {
                        return true;
                    }
                    // Sherpa supplies each newly completed segment, not a
                    // cumulative waveform. Its memory expires on callback return.
                    emitted_count.fetch_add(samples.len(), Ordering::Relaxed);
                    emit(AudioChunk { samples: Arc::new(samples.to_vec()), rate, last: progress >= 1.0 })
                }),
            )
            .ok_or("Speech generation failed.")?;
        if generation != shared.generation.load(Ordering::Acquire) {
            return Err("Cancelled".into());
        }
        if audio.samples().is_empty() || emitted.load(Ordering::Relaxed) != audio.samples().len() {
            return Err("Speech generation failed.".into());
        }
        // The final waveform contains the same samples already queued by the
        // callbacks. Drop it without copying or replaying them.
        Ok(())
    }
}

/// Feeds completed synthesis segments to playback while inference continues.
/// Only the first segment announces the reader chunk's highlight. Passage-gap
/// silence is appended once, after the final segment has been generated.
struct AudioStream {
    player: Arc<Player>,
    shared: Arc<Shared>,
    events: SpeechEvents,
    wake: mpsc::Sender<Command>,
    buffered: Arc<AtomicUsize>,
    generation: u64,
    index: usize,
    glyph_ranges: Vec<Range<u32>>,
    gap: f32,
    rate: Option<NonZeroU32>,
    queued: bool,
    complete: bool,
    error: Option<String>,
    chunks: Vec<AudioChunk>,
}

impl AudioStream {
    fn push(&mut self, audio: AudioChunk) -> bool {
        if self.rate.is_some_and(|rate| rate != audio.rate) {
            self.error = Some("Speech sample rate changed during synthesis.".into());
            return false;
        }
        if audio.samples.is_empty() {
            return true;
        }
        self.chunks.push(audio.clone());
        self.rate = Some(audio.rate);
        let announce = !self.queued;
        self.complete = audio.last;
        let silence = if audio.last { (self.gap * audio.rate.get() as f32).round() as usize } else { 0 };
        self.enqueue(audio.samples, silence, announce)
    }

    fn enqueue(&mut self, samples: Arc<Vec<f32>>, silence: usize, announce: bool) -> bool {
        if self.generation != self.shared.generation.load(Ordering::Acquire) {
            return false;
        }
        let rate = self.rate.expect("audio rate is set before queuing samples");
        let remaining = samples.len() + silence;
        self.buffered.fetch_add(remaining, Ordering::AcqRel);
        let source = FragmentSource {
            samples: OwnedPcm::new(samples, rate, silence),
            events: self.events.clone(),
            shared: self.shared.clone(),
            wake: self.wake.clone(),
            buffered: self.buffered.clone(),
            remaining,
            refill_at: rate.get() as usize * AUDIO_BUFFER_SECONDS,
            generation: self.generation,
            index: self.index,
            glyph_ranges: if announce { take(&mut self.glyph_ranges) } else { Vec::new() },
            started: !announce,
            pending_consumed: 0,
        };
        self.player.append(source);
        self.queued = true;
        let guard = self.shared.lock(&self.shared.player);
        if self.generation == self.shared.generation.load(Ordering::Acquire) && !self.shared.paused.load(Ordering::Acquire) {
            self.player.play();
        }
        drop(guard);
        self.shared.update_output(&self.wake);
        self.shared.publish_state();
        true
    }

    fn finish(&mut self) -> Result<NonZeroU32, String> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if !self.complete {
            return Err("Speech generation did not finish.".into());
        }
        self.rate.ok_or_else(|| "Speech generation failed.".into())
    }
}

/// Break source ownership cycles before closing the device. The loaded voice is
/// deliberately independent of the output, so a quick subsequent start stays warm.
fn release_output<O>(shared: &Shared, player: &mut Option<Arc<Player>>, output: &mut Option<O>) {
    let retired = shared.lock(&shared.player).take();
    let device = shared.lock(&shared.output).take();
    drop(retired);
    drop(device);
    drop(player.take());
    drop(output.take());
}

fn worker(commands: mpsc::Receiver<Command>, wake: mpsc::Sender<Command>, events: SpeechEvents, shared: Arc<Shared>) {
    worker_with(commands, wake, events, shared, load_voice, output::open_default, VOICE_IDLE_TIMEOUT);
}

fn unload_voice<E>(engine: &mut Option<E>) {
    if let Some(voice) = engine.take() {
        drop(voice);
        // glibc can retain freed inference buffers in the synthesis thread's
        // heap. Return those pages when the voice is explicitly retired, while
        // leaving the hot synthesis path and paused sessions alone.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

fn worker_with<E: SpeechEngine, O: OutputControl + 'static>(
    commands: mpsc::Receiver<Command>, wake: mpsc::Sender<Command>, events: SpeechEvents, shared: Arc<Shared>, mut load: impl FnMut(Voice) -> Result<E, String>, mut open: impl FnMut() -> Result<(Arc<Player>, O, Duration), String>,
    voice_idle_timeout: Duration,
) {
    let mut engine = None;
    let mut engine_voice = None;
    let mut output = None;
    let mut player: Option<Arc<Player>> = None;
    let mut supplied = None::<Vec<SpeechPassage>>;
    let mut passage = None::<SpeechPassage>;
    let mut prefetch = TextPrefetch::default();
    // Playback generations invalidate queued output, never reusable synthesis.
    let mut cache = AudioCache::default();
    let mut opening_chunk = true;
    let mut count = 0;
    let mut next_index = 0;
    let mut next_offset = 0;
    let mut next_glyph = 0;
    let mut generation = 0;
    let mut active = false;
    let mut finished = false;
    let mut drain_time = Duration::ZERO;
    let mut retire_at = None::<Instant>;
    let mut unload_at = None::<Instant>;
    let mut buffered = Arc::new(AtomicUsize::new(0));
    let mut sample_rate = 22050;
    loop {
        if retire_at.is_some_and(|deadline| Instant::now() >= deadline) {
            release_output(&shared, &mut player, &mut output);
            retire_at = None;
        }
        if unload_at.is_some_and(|deadline| Instant::now() >= deadline) {
            // Starting publishes the item count before sending its command.
            // Keep the warm model if a new start already raced this deadline.
            if shared.count.load(Ordering::Acquire) == 0 {
                unload_voice(&mut engine);
            }
            unload_at = None;
        }
        // Pausing suspends output, not preparation. Keep filling the bounded
        // audio and text queues while the narration session remains active.
        // When full, wait for a control command, requested text, or freed capacity.
        let paused = shared.paused.load(Ordering::Acquire);
        let empty = player.as_ref().is_none_or(|player| player.empty());
        let end = count.min(shared.end.load(Ordering::Acquire));
        let can_generate = next_index < end && (supplied.is_some() || passage.is_some() || prefetch.has_ready()) && buffered.load(Ordering::Acquire) < sample_rate * AUDIO_BUFFER_SECONDS;
        let can_prefetch = supplied.is_none() && prefetch.can_request(end, f32::from_bits(shared.rate.load(Ordering::Acquire)));
        let can_finish = next_index >= end && empty && !paused;
        let command = if active && generation == shared.generation.load(Ordering::Acquire) && (can_generate || can_prefetch || can_finish) {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        } else if let Some(deadline) = retire_at.into_iter().chain(unload_at).min() {
            // One-shot resource deadlines. Commands interrupt this wait;
            // playback and pauses never require a periodic poll.
            match commands.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };
        // A later navigation/start may have invalidated queued work while the
        // current synthesis ran. Skip its resource and playback changes.
        if command.as_ref().is_some_and(|command| match command {
            Command::Replace { generation, .. }
            | Command::Read { generation, .. }
            | Command::Skip { generation, .. }
            | Command::Stop { generation }
            | Command::Release { generation }
            | Command::Chunk { generation, .. }
            | Command::OutputFailed { generation, .. } => *generation != shared.generation.load(Ordering::Acquire),
            _ => false,
        }) {
            // Starts carry the document definition used by a subsequent Skip.
            // Retain just that metadata, so start-then-skip works even when the
            // worker has not processed the original start yet.
            match command {
                Some(Command::Replace { text, .. }) if shared.count.load(Ordering::Acquire) > 0 => {
                    count = text.len();
                    supplied = Some(text);
                }
                Some(Command::Read { count: items, .. }) if shared.count.load(Ordering::Acquire) > 0 => {
                    count = items;
                    supplied = None;
                }
                _ => {}
            }
            continue;
        }
        match command {
            #[cfg(test)]
            Some(Command::Shutdown) => break,
            Some(Command::OutputFailed { generation: failed, message }) => {
                if failed == shared.generation.load(Ordering::Acquire) {
                    active = false;
                    if let Some(player) = &player {
                        player.stop();
                        player.pause();
                    }
                    let _ = events.try_send(SpeechEvent::Failed { generation: failed, message });
                }
            }
            #[cfg(test)]
            Some(Command::Synchronize(ready)) => {
                let _ = ready.send(());
            }
            #[cfg(test)]
            Some(Command::Hold { ready, release }) => {
                let _ = ready.send(());
                let _ = release.recv_timeout(Duration::from_secs(3));
            }
            Some(Command::Stop { generation: stopped }) => {
                // A quick new start may already have superseded this stop.
                if stopped == shared.generation.load(Ordering::Acquire) {
                    generation = stopped;
                    active = false;
                    count = 0;
                    supplied = None;
                    passage = None;
                    prefetch.reset(0, 0);
                    if finished && output.is_some() && !drain_time.is_zero() {
                        retire_at = Some(Instant::now() + drain_time);
                    } else {
                        release_output(&shared, &mut player, &mut output);
                        retire_at = None;
                    }
                    finished = false;
                    unload_at = engine.as_ref().map(|_| Instant::now() + voice_idle_timeout);
                    buffered = Arc::new(AtomicUsize::new(0));
                }
            }
            Some(Command::Release { generation: released }) => {
                generation = released;
                active = false;
                count = 0;
                supplied = None;
                passage = None;
                prefetch.reset(0, 0);
                finished = false;
                retire_at = None;
                unload_at = None;
                release_output(&shared, &mut player, &mut output);
                unload_voice(&mut engine);
                buffered = Arc::new(AtomicUsize::new(0));
            }
            Some(Command::Replace { generation: next, text, start }) => {
                if player.as_ref().is_some_and(|player| !player.empty()) {
                    // Rodio's append waits for stopped sources to drain. A
                    // paused device cannot drain them, so retire pending audio
                    // before a new batch while retaining the loaded voice.
                    release_output(&shared, &mut player, &mut output);
                }
                finished = false;
                retire_at = None;
                unload_at = None;
                generation = next;
                count = text.len();
                supplied = Some(text);
                next_index = start.min(count);
                next_offset = 0;
                next_glyph = 0;
                passage = None;
                prefetch.reset(next_index, next_glyph);
                opening_chunk = true;
                buffered = Arc::new(AtomicUsize::new(0));
                active = true;
            }
            Some(Command::Read { generation: next, count: items, start, glyph_start }) => {
                if player.as_ref().is_some_and(|player| !player.empty()) {
                    release_output(&shared, &mut player, &mut output);
                }
                finished = false;
                retire_at = None;
                unload_at = None;
                generation = next;
                count = items;
                supplied = None;
                next_index = start.min(count);
                next_offset = 0;
                next_glyph = glyph_start;
                passage = None;
                prefetch.reset(next_index, next_glyph);
                opening_chunk = true;
                buffered = Arc::new(AtomicUsize::new(0));
                active = true;
            }
            Some(Command::Skip { generation: next, index }) => {
                if player.as_ref().is_some_and(|player| !player.empty()) {
                    release_output(&shared, &mut player, &mut output);
                }
                finished = false;
                retire_at = None;
                if count > 0 {
                    unload_at = None;
                }
                generation = next;
                next_index = index.min(count);
                next_offset = 0;
                next_glyph = 0;
                passage = None;
                prefetch.reset(next_index, next_glyph);
                opening_chunk = true;
                buffered = Arc::new(AtomicUsize::new(0));
                active = true;
            }
            Some(Command::Chunk { generation: supplied_generation, index, glyph_start, chunk }) => {
                if supplied_generation == generation {
                    prefetch.provide(index, glyph_start, chunk);
                }
            }
            Some(Command::Wake) => {
                if generation == shared.generation.load(Ordering::Acquire) {
                    // Retain an exhausted but paused batch until resume can emit
                    // Finished. Only newly available items revive a finished batch.
                    let available = next_index < count.min(shared.end.load(Ordering::Acquire)) || player.as_ref().is_some_and(|player| !player.empty());
                    if available {
                        finished = false;
                    }
                    active |= available;
                }
            }
            None => {}
        }
        shared.publish_state();
        if generation != shared.generation.load(Ordering::Acquire) {
            active = false;
        }
        if !active {
            continue;
        }
        let end = count.min(shared.end.load(Ordering::Acquire));
        if supplied.is_none() {
            // Request the following text before entering blocking inference, so
            // extraction on the UI thread overlaps synthesis on this worker.
            prefetch.request(generation, end, f32::from_bits(shared.rate.load(Ordering::Acquire)), &events);
        }
        if buffered.load(Ordering::Acquire) >= sample_rate * AUDIO_BUFFER_SECONDS {
            continue;
        }
        if next_index < end && passage.is_none() {
            if let Some(text) = &supplied {
                passage = Some(text[next_index].clone());
            } else if let Some(prepared) = prefetch.take(next_index, next_glyph) {
                passage = prepared;
                if passage.is_none() {
                    next_index += 1;
                    next_glyph = 0;
                    next_offset = 0;
                    continue;
                }
            }
        }
        if let Some(item) = passage.as_mut().filter(|_| next_index < end) {
            let span = if item.position.is_some() { (!item.text().is_empty()).then_some(0..item.text().len()) } else { next_chunk_with_limit(item.text(), next_offset, if opening_chunk { FIRST_CHUNK_CHARS } else { MAX_CHUNK_CHARS }) };
            let Some(span) = span else {
                next_index += 1;
                next_offset = 0;
                next_glyph = 0;
                passage = None;
                continue;
            };
            let voice = *shared.lock(&shared.voice);
            if generation != shared.generation.load(Ordering::Acquire) {
                continue;
            }
            let speed = f32::from_bits(shared.rate.load(Ordering::Acquire));
            let pauses = *shared.lock(&shared.pauses);
            let final_chunk = item.position.as_ref().map_or_else(|| item.text()[span.end..].trim().is_empty(), |position| position.next_glyph.is_none());
            let glyph_ranges = item.position.as_mut().map_or_else(Vec::new, |position| take(&mut position.glyph_ranges));
            let text = prepare_speech_text(&item.text()[span.clone()]);
            let mut config = narration_config(speed, pauses);
            config.sid = voice.speaker_id();
            let key = AudioKey::new(&text, voice, &config);
            let cached = cache.get(&key);
            if cached.is_none() {
                if engine_voice != Some(voice) {
                    // Drop the previous session before allocating the next model.
                    unload_voice(&mut engine);
                    engine_voice = Some(voice);
                }
                if engine.is_none() {
                    match load(voice) {
                        Ok(model) => engine = Some(model),
                        Err(message) => {
                            let _ = events.try_send(SpeechEvent::Failed { generation, message });
                            active = false;
                            continue;
                        }
                    }
                }
            }
            if generation != shared.generation.load(Ordering::Acquire) {
                continue;
            }
            if player.is_none() {
                match open() {
                    Ok((next, sink, delay)) => {
                        drain_time = delay;
                        next.pause();
                        let sink = Arc::new(sink);
                        *shared.lock(&shared.output) = Some(sink.clone());
                        *shared.lock(&shared.player) = Some(next.clone());
                        player = Some(next);
                        output = Some(sink);
                    }
                    Err(message) => {
                        let _ = events.try_send(SpeechEvent::Failed { generation, message });
                        active = false;
                        continue;
                    }
                }
            }
            let block_gap = if final_chunk && item.ends_block() { 0.25 * pauses.sentence_scale.clamp(0.2, 3.0) } else { 0.0 };
            let gap = if final_chunk { pauses.passage_gap.max(block_gap) / speed } else { 0.0 };
            let stream = Arc::new(Mutex::new(AudioStream {
                player: player.as_ref().expect("speech output was opened").clone(),
                shared: shared.clone(),
                events: events.clone(),
                wake: wake.clone(),
                buffered: buffered.clone(),
                generation,
                index: next_index,
                glyph_ranges,
                gap,
                rate: None,
                queued: false,
                complete: false,
                error: None,
                chunks: Vec::new(),
            }));
            let reused = cached.is_some();
            let result = if let Some(chunks) = cached {
                let mut target = shared.lock(&stream);
                if chunks.into_iter().all(|audio| target.push(audio)) { Ok(()) } else { Err("Cancelled".into()) }
            } else {
                let target = stream.clone();
                let current = shared.clone();
                engine.as_ref().expect("speech model was loaded").generate(&text, &config, &shared, generation, move |audio| current.lock(&target).push(audio))
            };
            let result = result.and_then(|()| shared.lock(&stream).finish());
            if result.is_ok() && !reused {
                cache.insert(key, take(&mut shared.lock(&stream).chunks));
            }
            if generation != shared.generation.load(Ordering::Acquire) {
                continue;
            }
            let result = result.map(|rate| {
                sample_rate = rate.get() as usize;
                opening_chunk = false;
            });
            if let Err(message) = result {
                active = false;
                if let Some(player) = &player {
                    player.stop();
                }
                let _ = events.try_send(SpeechEvent::Failed { generation, message });
            } else if final_chunk {
                next_index += 1;
                next_offset = 0;
                next_glyph = 0;
                passage = None;
            } else if let Some(position) = &item.position {
                next_glyph = position.next_glyph.expect("continuing reading chunk");
                passage = None;
            } else {
                next_offset = span.end;
            }
        }
        if next_index >= end && player.as_ref().is_none_or(|player| player.empty()) && active && !shared.paused.load(Ordering::Acquire) {
            active = false;
            finished = true;
            let _ = events.try_send(SpeechEvent::Finished { generation });
        }
    }
    release_output(&shared, &mut player, &mut output);
    unload_voice(&mut engine);
}

struct FragmentSource {
    samples: OwnedPcm,
    events: SpeechEvents,
    shared: Arc<Shared>,
    wake: mpsc::Sender<Command>,
    buffered: Arc<AtomicUsize>,
    remaining: usize,
    refill_at: usize,
    pending_consumed: usize,
    generation: u64,
    index: usize,
    glyph_ranges: Vec<Range<u32>>,
    started: bool,
}

impl Iterator for FragmentSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.generation != self.shared.generation.load(Ordering::Acquire) || self.index >= self.shared.end.load(Ordering::Acquire) {
            return None;
        }
        let sample = self.samples.next()?;
        if !self.started {
            self.started = true;
            self.shared.cursor.store(self.index, Ordering::Release);
            let _ = self.events.try_send(SpeechEvent::Started { generation: self.generation, index: self.index, glyph_ranges: take(&mut self.glyph_ranges) });
        }
        self.pending_consumed += 1;
        if self.pending_consumed == 128 {
            let previous = self.buffered.fetch_sub(self.pending_consumed, Ordering::AcqRel);
            if previous >= self.refill_at && previous - self.pending_consumed < self.refill_at {
                // One wake at the duration boundary, rather than polling or
                // sending a command for every group of consumed samples.
                let _ = self.wake.send(Command::Wake);
            }
            self.remaining -= self.pending_consumed;
            self.pending_consumed = 0;
        }
        Some(sample)
    }
}

impl Drop for FragmentSource {
    fn drop(&mut self) {
        self.buffered.fetch_sub(self.remaining, Ordering::AcqRel);
        let _ = self.wake.send(Command::Wake);
    }
}

impl Source for FragmentSource {
    fn current_span_len(&self) -> Option<usize> {
        self.samples.current_span_len()
    }
    fn channels(&self) -> NonZeroU16 {
        self.samples.channels()
    }
    fn sample_rate(&self) -> NonZeroU32 {
        self.samples.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.samples.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::{buffer::SamplesBuffer, queue::SourcesQueueOutput};

    type TestSamples = Arc<Mutex<SourcesQueueOutput>>;

    struct TestEngine {
        generated: mpsc::Sender<String>,
        speaker_ids: mpsc::Sender<i32>,
        samples: usize,
        unloaded: mpsc::Sender<()>,
        gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
        stream_gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>,
    }

    impl Drop for TestEngine {
        fn drop(&mut self) {
            let _ = self.unloaded.send(());
        }
    }

    impl SpeechEngine for TestEngine {
        fn generate(&self, text: &str, config: &GenerationConfig, _: &Arc<Shared>, _: u64, mut emit: impl FnMut(AudioChunk) -> bool + 'static) -> Result<(), String> {
            self.speaker_ids.send(config.sid).unwrap();
            self.generated.send(text.to_owned()).unwrap();
            if let Some(gate) = &self.gate {
                gate.lock().unwrap().recv_timeout(Duration::from_secs(3)).expect("release test synthesis");
            }
            let rate = NonZeroU32::new(1024).unwrap();
            if let Some(gate) = &self.stream_gate {
                if !emit(AudioChunk { samples: Arc::new(vec![0.25; self.samples / 2]), rate, last: false }) {
                    return Err("Cancelled".into());
                }
                gate.lock().unwrap().recv_timeout(Duration::from_secs(3)).expect("release remaining synthesis");
                emit(AudioChunk { samples: Arc::new(vec![0.25; self.samples - self.samples / 2]), rate, last: true });
            } else {
                emit(AudioChunk { samples: Arc::new(vec![0.25; self.samples]), rate, last: true });
            }
            Ok(())
        }
    }

    struct TestOutput {
        _samples: TestSamples,
        retired: mpsc::Sender<()>,
        paused: AtomicBool,
        states: mpsc::Sender<bool>,
    }

    impl OutputControl for TestOutput {
        fn pause(&self) {
            if !self.paused.swap(true, Ordering::AcqRel) {
                let _ = self.states.send(true);
            }
        }
        fn resume(&self) -> Result<(), String> {
            if self.paused.swap(false, Ordering::AcqRel) {
                let _ = self.states.send(false);
            }
            Ok(())
        }
    }

    impl Drop for TestOutput {
        fn drop(&mut self) {
            let _ = self.retired.send(());
        }
    }

    struct TestWorker {
        speech: DesktopSpeech,
        generated: mpsc::Receiver<String>,
        loaded_voices: mpsc::Receiver<Voice>,
        speaker_ids: mpsc::Receiver<i32>,
        opened: mpsc::Receiver<TestSamples>,
        retired: mpsc::Receiver<()>,
        unloaded: mpsc::Receiver<()>,
        device_states: mpsc::Receiver<bool>,
        loads: Arc<AtomicUsize>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl TestWorker {
        fn new(samples: usize) -> Self {
            Self::with_drain_time(samples, Duration::ZERO)
        }

        fn with_drain_time(samples: usize, drain_time: Duration) -> Self {
            Self::with_timeouts(samples, drain_time, VOICE_IDLE_TIMEOUT)
        }

        fn with_timeouts(samples: usize, drain_time: Duration, idle_timeout: Duration) -> Self {
            Self::with_controls(samples, drain_time, idle_timeout, None)
        }

        fn with_controls(samples: usize, drain_time: Duration, idle_timeout: Duration, gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>) -> Self {
            Self::with_streaming_controls(samples, drain_time, idle_timeout, gate, None)
        }

        fn with_streaming_controls(samples: usize, drain_time: Duration, idle_timeout: Duration, gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>, stream_gate: Option<Arc<Mutex<mpsc::Receiver<()>>>>) -> Self {
            let shared = shared();
            let (commands, receiver) = mpsc::channel();
            let (sender, received) = async_channel::unbounded();
            let speech = DesktopSpeech::test_handle(commands.clone(), sender, received, shared.clone());
            let events = SpeechEvents::Routed(Arc::downgrade(&shared));
            let (generated, generated_receiver) = mpsc::channel();
            let (loaded_voices, loaded_voice_receiver) = mpsc::channel();
            let (speaker_ids, speaker_id_receiver) = mpsc::channel();
            let (opened, opened_receiver) = mpsc::channel();
            let (retired, retired_receiver) = mpsc::channel();
            let (unloaded, unloaded_receiver) = mpsc::channel();
            let (states, state_receiver) = mpsc::channel();
            let loads = Arc::new(AtomicUsize::new(0));
            let load_count = loads.clone();
            let worker_shared = shared.clone();
            let wake = commands.clone();
            let worker = thread::spawn(move || {
                worker_with(
                    receiver,
                    wake,
                    events,
                    worker_shared,
                    move |voice| {
                        load_count.fetch_add(1, Ordering::Relaxed);
                        loaded_voices.send(voice).unwrap();
                        Ok(TestEngine { generated: generated.clone(), speaker_ids: speaker_ids.clone(), samples, unloaded: unloaded.clone(), gate: gate.clone(), stream_gate: stream_gate.clone() })
                    },
                    move || {
                        let (player, source) = Player::new();
                        let samples = Arc::new(Mutex::new(source));
                        opened.send(samples.clone()).unwrap();
                        Ok((Arc::new(player), TestOutput { _samples: samples, retired: retired.clone(), paused: AtomicBool::new(true), states: states.clone() }, drain_time))
                    },
                    idle_timeout,
                )
            });
            Self {
                speech,
                generated: generated_receiver,
                loaded_voices: loaded_voice_receiver,
                speaker_ids: speaker_id_receiver,
                opened: opened_receiver,
                retired: retired_receiver,
                unloaded: unloaded_receiver,
                device_states: state_receiver,
                loads,
                worker: Some(worker),
            }
        }

        fn player(&self) -> Arc<Player> {
            wait_until(|| self.speech.shared.player.lock().unwrap().as_ref().is_some_and(|player| !player.empty()));
            self.speech.shared.player.lock().unwrap().as_ref().unwrap().clone()
        }

        fn synchronize(&self) {
            let (ready, received) = mpsc::channel();
            self.speech.commands.send(Command::Synchronize(ready)).unwrap();
            receive(&received);
        }

        fn hold(&self) -> mpsc::Sender<()> {
            let (ready, received) = mpsc::channel();
            let (release, waiting) = mpsc::channel();
            self.speech.commands.send(Command::Hold { ready, release: waiting }).unwrap();
            receive(&received);
            release
        }
    }

    impl Drop for TestWorker {
        fn drop(&mut self) {
            self.speech.stop();
            let _ = self.speech.commands.send(Command::Shutdown);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready() {
            assert!(Instant::now() < deadline, "worker did not reach the expected state");
            thread::yield_now();
        }
    }

    fn receive<T>(receiver: &mpsc::Receiver<T>) -> T {
        receiver.recv_timeout(Duration::from_secs(3)).expect("worker notification")
    }

    fn next_event(worker: &TestWorker) -> SpeechEvent {
        reader_event(&worker.speech.events)
    }

    fn reader_event(receiver: &Receiver<SpeechEvent>) -> SpeechEvent {
        let mut event = None;
        wait_until(|| {
            event = receiver.try_recv().ok();
            event.is_some()
        });
        event.unwrap()
    }

    fn consume_audio(samples: &TestSamples, count: usize) {
        for _ in 0..count {
            wait_until(|| samples.lock().unwrap().next().is_some_and(|sample| sample == 0.25));
        }
    }

    fn finished_generation(worker: &TestWorker) -> u64 {
        let mut finished = None;
        wait_until(|| {
            match worker.speech.events.try_recv() {
                Ok(SpeechEvent::Finished { generation }) => finished = Some(generation),
                Ok(SpeechEvent::Failed { message, .. }) => panic!("speech failed: {message}"),
                Ok(_) | Err(async_channel::TryRecvError::Empty) => {}
                Err(error) => panic!("worker event channel: {error}"),
            }
            finished.is_some()
        });
        finished.unwrap()
    }

    #[test]
    fn audio_lookahead_is_limited_by_seconds_instead_of_passages() {
        for samples_per_passage in [1024, 8192] {
            let worker = TestWorker::new(samples_per_passage);
            let passage = |index| format!("Passage {}.", "a".repeat(index + 1));
            worker.speech.replace_passages_from((0..200).map(|index| SpeechPassage::new(passage(index), false)).collect(), 0);
            let samples = receive(&worker.opened);
            let expected = (1024 * AUDIO_BUFFER_SECONDS).div_ceil(samples_per_passage);
            for index in 0..expected {
                assert_eq!(receive(&worker.generated), passage(index));
            }
            worker.synchronize();
            assert!(worker.generated.try_recv().is_err(), "stop generating once a minute of audio is buffered");
            assert_eq!(worker.player().len(), expected);
            consume_audio(&samples, samples_per_passage);
            assert_eq!(receive(&worker.generated), passage(expected), "consuming audio must refill the time budget");
        }
    }

    #[test]
    fn pausing_during_synthesis_keeps_filling_the_audio_budget_without_playing() {
        let (release, gate) = mpsc::channel();
        let worker = TestWorker::with_controls(20 * 1024, Duration::ZERO, VOICE_IDLE_TIMEOUT, Some(Arc::new(Mutex::new(gate))));
        let generation = worker.speech.replace_passages_from((0..5).map(|index| SpeechPassage::new(format!("Passage {}.", "a".repeat(index + 1)), false)).collect(), 0);
        let _samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Passage a.");
        release.send(()).unwrap();
        assert_eq!(receive(&worker.generated), "Passage aa.");
        worker.speech.pause();
        assert_eq!(worker.player().len(), 1);
        release.send(()).unwrap();
        assert_eq!(receive(&worker.generated), "Passage aaa.");
        release.send(()).unwrap();
        worker.synchronize();
        assert_eq!(worker.player().len(), 3, "paused narration fills the same one-minute audio budget");
        assert!(worker.player().is_paused());
        assert!(worker.speech.shared.paused.load(Ordering::Acquire));
        assert_eq!(worker.speech.shared.generation.load(Ordering::Acquire), generation);
        assert_eq!(worker.speech.shared.cursor.load(Ordering::Acquire), 0);
        assert!(worker.speech.events.try_recv().is_err(), "preparation must not advance highlighting or finish playback");
        assert!(worker.generated.try_recv().is_err(), "generation sleeps once the buffer is full");
        worker.speech.stop();
        worker.synchronize();
        assert!(worker.generated.try_recv().is_err(), "stopping must not continue filling the remaining document");
    }

    #[test]
    fn paused_reader_continues_requesting_text_and_refills_after_resume() {
        let worker = TestWorker::new(20 * 1024);
        let generation = worker.speech.read_items_from(4, 0, 0);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, .. }));
        worker.speech.provide_chunk(generation, 0, 0, Some(SpeechChunk { text: "First.".into(), glyph_ranges: vec![0..6], next_glyph: None }));
        let samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "First.");
        worker.synchronize();
        worker.speech.pause();
        for (index, text) in [(1, "Second."), (2, "Third."), (3, "Fourth.")] {
            assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: requested, .. } if requested == index));
            worker.speech.provide_chunk(generation, index, 0, Some(SpeechChunk { text: text.into(), glyph_ranges: vec![index as u32 * 10..index as u32 * 10 + 6], next_glyph: None }));
            if index < 3 {
                assert_eq!(receive(&worker.generated), text);
            }
        }
        worker.synchronize();
        assert_eq!(worker.player().len(), 3);
        assert!(worker.player().is_paused());
        assert!(worker.generated.try_recv().is_err(), "ready text waits when the audio budget is full");
        assert!(worker.speech.events.try_recv().is_err(), "paused preparation must not announce future passages");
        worker.speech.resume();
        // Include the queued paragraph gaps when freeing audio capacity.
        consume_audio(&samples, 1024);
        assert_eq!(receive(&worker.generated), "Fourth.");
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: current, index: 0, glyph_ranges } if current == generation && glyph_ranges == [0..6]));
    }

    #[test]
    fn playback_and_highlighting_start_before_synthesis_finishes() {
        let (release, gate) = mpsc::channel();
        let worker = TestWorker::with_streaming_controls(512, Duration::ZERO, VOICE_IDLE_TIMEOUT, None, Some(Arc::new(Mutex::new(gate))));
        let generation = worker.speech.read_items_from(1, 0, 0);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, .. }));
        worker.speech.provide_chunk(generation, 0, 0, Some(SpeechChunk { text: "First sentence. Second sentence.".into(), glyph_ranges: vec![0..31], next_glyph: None }));
        let samples = receive(&worker.opened);
        receive(&worker.generated);
        let player = worker.player();
        consume_audio(&samples, 256);
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: started, index: 0, glyph_ranges } if started == generation && glyph_ranges == [0..31]));
        wait_until(|| {
            samples.lock().unwrap().next();
            player.empty()
        });
        assert!(worker.speech.events.try_recv().is_err(), "draining a partial segment must not finish the passage");
        release.send(()).unwrap();
        worker.synchronize();
        consume_audio(&samples, 256);
        assert!(worker.speech.events.try_recv().is_err(), "later segments must not repeat or clear the highlight");
        wait_until(|| {
            samples.lock().unwrap().next();
            player.empty()
        });
        assert_eq!(finished_generation(&worker), generation);
    }

    #[test]
    fn navigating_during_streaming_discards_unfinished_and_buffered_audio() {
        let (release, gate) = mpsc::channel();
        let worker = TestWorker::with_streaming_controls(512, Duration::ZERO, VOICE_IDLE_TIMEOUT, None, Some(Arc::new(Mutex::new(gate))));
        let old_generation = worker.speech.read_items_from(2, 0, 0);
        next_event(&worker);
        worker.speech.provide_chunk(old_generation, 0, 0, Some(SpeechChunk { text: "Old first. Old second.".into(), glyph_ranges: vec![0..22], next_glyph: None }));
        let old_samples = receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { generation, index: 1, .. } if generation == old_generation));
        let generation = worker.speech.skip_to(1);
        for _ in 0..128 {
            assert_ne!(old_samples.lock().unwrap().next(), Some(0.25), "cancellation must silence already queued segments");
        }
        release.send(()).unwrap();
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { generation: requested, index: 1, .. } if requested == generation));
        worker.speech.provide_chunk(generation, 1, 0, Some(SpeechChunk { text: "New first. New second.".into(), glyph_ranges: vec![100..122], next_glyph: None }));
        assert_eq!(receive(&worker.generated), "New first. New second.");
        worker.player();
        let samples = worker.opened.try_recv().unwrap_or_else(|_| old_samples.clone());
        consume_audio(&samples, 1);
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: started, index: 1, glyph_ranges } if started == generation && glyph_ranges == [100..122]));
        release.send(()).unwrap();
        worker.synchronize();
        assert!(worker.speech.events.try_recv().is_err());
    }

    #[test]
    #[ignore = "requires installed Alba and George voice assets"]
    fn installed_voices_deliver_incremental_audio_without_repeating_the_final_waveform() {
        for voice in [Voice::Alba, Voice::George] {
            let model = load_voice(voice).expect("load installed voice");
            let shared = shared();
            let segments = Arc::new(Mutex::new(Vec::new()));
            let output = segments.clone();
            let started = Instant::now();
            let mut config = narration_config(1.0, Pauses::NATURAL);
            config.sid = voice.speaker_id();
            SpeechEngine::generate(&model, "The first sentence is ready. The second sentence follows.", &config, &shared, 4, move |audio| {
                output.lock().unwrap().push((audio.samples.len(), audio.last, started.elapsed()));
                true
            })
            .expect("stream synthesis segments");
            let segments = segments.lock().unwrap();
            assert!(segments.len() >= 2, "{voice:?} must deliver completed sentences separately");
            assert!(segments.iter().all(|(samples, _, _)| *samples > 0));
            assert!(segments[..segments.len() - 1].iter().all(|(_, last, _)| !last));
            assert!(segments.last().unwrap().1);
            assert!(segments[0].2 < segments.last().unwrap().2);
            println!("{voice:?}: {} segments; first ready at {:.2}s, finished at {:.2}s; playback speed 1×", segments.len(), segments[0].2.as_secs_f64(), segments.last().unwrap().2.as_secs_f64());
        }
    }

    #[test]
    fn changing_voice_restarts_the_current_passage_and_keeps_pause_state() {
        let worker = TestWorker::new(8192);
        worker.speech.replace(vec!["Current passage.".into()]);
        let _old_audio = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Current passage.");
        assert_eq!(receive(&worker.loaded_voices), Voice::Alba);
        assert_eq!(receive(&worker.speaker_ids), 0);
        worker.player();
        worker.speech.pause();
        let generation = worker.speech.set_voice(Voice::George);
        receive(&worker.unloaded);
        receive(&worker.opened);
        assert_eq!(receive(&worker.loaded_voices), Voice::George);
        assert_eq!(receive(&worker.generated), "Current passage.");
        assert_eq!(receive(&worker.speaker_ids), 26);
        assert_eq!(worker.speech.shared.generation.load(Ordering::Acquire), generation);
        assert!(worker.speech.shared.paused.load(Ordering::Acquire));
        assert!(worker.player().is_paused());
        assert_eq!(worker.loads.load(Ordering::Acquire), 2);
    }

    #[test]
    fn inactive_reader_voice_choice_does_not_change_the_active_reader() {
        let worker = TestWorker::new(8192);
        worker.speech.replace(vec!["Active reader.".into()]);
        receive(&worker.opened);
        receive(&worker.generated);
        assert_eq!(receive(&worker.loaded_voices), Voice::Alba);
        let inactive = DesktopSpeech::for_worker(worker.speech.commands.clone(), worker.speech.shared.clone());
        inactive.set_voice(Voice::George);
        assert_eq!(*worker.speech.shared.voice.lock().unwrap(), Voice::Alba);
        assert_eq!(worker.loads.load(Ordering::Acquire), 1);
        inactive.replace(vec!["New reader.".into()]);
        receive(&worker.unloaded);
        assert_eq!(receive(&worker.loaded_voices), Voice::George);
        assert_eq!(receive(&worker.generated), "New reader.");
        assert_eq!(worker.loads.load(Ordering::Acquire), 2);
    }

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            generation: AtomicU64::new(4),
            paused: AtomicBool::new(false),
            cursor: AtomicUsize::new(0),
            count: AtomicUsize::new(2),
            rate: AtomicU32::new(1.0_f32.to_bits()),
            end: AtomicUsize::new(usize::MAX),
            pauses: Mutex::new(Pauses::NATURAL),
            voice: Mutex::new(Voice::Alba),
            player: Mutex::new(None),
            output: Mutex::new(None),
            owner: Mutex::new(0),
            route: Mutex::new(None),
        })
    }

    fn source(shared: &Arc<Shared>, events: &Sender<SpeechEvent>, index: usize, samples: Vec<f32>) -> FragmentSource {
        let (wake, _) = mpsc::channel();
        let remaining = samples.len();
        FragmentSource {
            samples: OwnedPcm::new(samples, NonZeroU32::new(22050).unwrap(), 0),
            shared: shared.clone(),
            events: events.clone().into(),
            wake,
            buffered: Arc::new(AtomicUsize::new(remaining)),
            remaining,
            refill_at: 22050 * 8,
            pending_consumed: 0,
            generation: 4,
            index,
            glyph_ranges: vec![0..20],
            started: false,
        }
    }

    #[test]
    fn owned_pcm_keeps_the_original_allocation_and_generates_gap_silence() {
        let samples = vec![0.1, 0.2, 0.3];
        let allocation = samples.as_ptr();
        let mut pcm = OwnedPcm::new(samples, NonZeroU32::new(5).unwrap(), 2);
        assert_eq!(pcm.samples.as_slice().as_ptr(), allocation, "no Vec-to-Arc PCM copy");
        assert_eq!(pcm.samples.len(), 3, "silence must not extend the allocation");
        assert_eq!(pcm.size_hint(), (5, Some(5)));
        assert_eq!(pcm.channels().get(), 1);
        assert_eq!(pcm.sample_rate().get(), 5);
        assert_eq!(pcm.total_duration(), Some(Duration::from_secs(1)));
        assert_eq!(pcm.next(), Some(0.1));
        assert_eq!(pcm.next(), Some(0.2));
        assert_eq!(pcm.next(), Some(0.3));
        assert_eq!(pcm.size_hint(), (2, Some(2)));
        let empty = Vec::<f32>::new();
        assert_eq!(pcm.samples.as_slice().as_ptr(), empty.as_ptr(), "release PCM before emitting the gap");
        assert_eq!(pcm.collect::<Vec<_>>(), [0.0, 0.0]);
    }

    #[test]
    fn reader_endpoints_share_the_single_process_worker() {
        let first = DesktopSpeech::new().unwrap();
        let second = DesktopSpeech::new().unwrap();
        assert!(Arc::ptr_eq(&first.shared, &second.shared));
        assert_ne!(first.client.id, second.client.id);
        assert!(!first.is_current());
        assert!(!second.is_current());
    }

    #[test]
    fn opening_a_speech_session_and_changing_settings_does_not_load_the_voice() {
        let worker = TestWorker::new(8192);
        worker.speech.set_title("A book");
        worker.speech.set_rate(1.25);
        worker.speech.set_pauses(Pauses::NATURAL);
        worker.synchronize();
        assert_eq!(worker.loads.load(Ordering::Acquire), 0);
        assert!(worker.opened.try_recv().is_err());

        worker.speech.replace(vec!["Start reading only when requested.".into()]);
        assert_eq!(receive(&worker.generated), "Start reading only when requested.");
        assert_eq!(worker.loads.load(Ordering::Acquire), 1);
    }

    #[test]
    fn skipping_before_a_supplied_batch_starts_retains_its_text() {
        let worker = TestWorker::new(8192);
        let hold = worker.hold();
        worker.speech.replace(vec!["First item.".into(), "Second item.".into()]);
        let generation = worker.speech.skip_to(1);
        drop(hold);
        assert_eq!(receive(&worker.generated), "Second item.");
        let samples = receive(&worker.opened);
        worker.player();
        consume_audio(&samples, 1);
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: started, index: 1, .. } if started == generation));
        assert_eq!(worker.loads.load(Ordering::Acquire), 1);
    }

    #[test]
    fn skipping_before_a_lazy_batch_starts_retains_its_item_count() {
        let worker = TestWorker::new(8192);
        let hold = worker.hold();
        worker.speech.read_items_from(12, 3, 50);
        let generation = worker.speech.skip_to(7);
        drop(hold);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { generation: requested, index: 7, glyph_start: 0, .. } if requested == generation));
        assert_eq!(worker.loads.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancelled_queued_controls_do_not_retire_the_existing_output() {
        let worker = TestWorker::new(8192);
        worker.speech.replace(vec!["Original item.".into()]);
        let _samples = receive(&worker.opened);
        receive(&worker.generated);
        let player = worker.player();
        worker.speech.pause();
        let hold = worker.hold();
        worker.speech.read_items_from(10, 0, 0);
        worker.speech.skip_to(1);
        let passage = SpeechPassage::new("Cancelled replacement.", true);
        let text = Arc::downgrade(&passage.text);
        worker.speech.replace_passages_from(vec![passage], 0);
        worker.speech.cancel();
        drop(hold);
        worker.synchronize();
        assert!(Arc::ptr_eq(&player, worker.speech.shared.player.lock().unwrap().as_ref().unwrap()));
        assert!(worker.retired.try_recv().is_err());
        assert!(worker.generated.try_recv().is_err());
        assert!(worker.speech.events.try_recv().is_err());
        assert!(text.upgrade().is_none(), "cancelled queued text must be released");
    }

    #[test]
    fn switching_readers_reuses_the_voice_and_routes_events_only_to_the_active_reader() {
        let worker = TestWorker::new(8192);
        let former = DesktopSpeech::for_worker(worker.speech.commands.clone(), worker.speech.shared.clone());
        let first_generation = former.replace(vec!["First reader.".into()]);
        let _old_samples = receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        let current = DesktopSpeech::for_worker(worker.speech.commands.clone(), worker.speech.shared.clone());
        current.set_rate(1.5);
        assert_eq!(f32::from_bits(current.shared.rate.load(Ordering::Acquire)), 1.0, "inactive settings stay local");
        let generation = current.read_items_from(2, 1, 20);
        assert!(matches!(reader_event(&former.events), SpeechEvent::Remote { generation: stopped, command: SpeechControl::Stop } if stopped == first_generation));
        assert!(matches!(reader_event(&current.events), SpeechEvent::Requested { generation: requested, index: 1, glyph_start: 20, .. } if requested == generation));
        former.pause();
        former.resume();
        former.stop();
        former.cancel();
        former.skip_to(0);
        former.set_end_fragment(Some(0));
        former.set_rate(0.7);
        former.set_pauses(Pauses { sentence_scale: 2.0, passage_gap: 1.0 });
        former.provide_chunk(first_generation, 0, 0, Some(SpeechChunk { text: "Old text.".into(), glyph_ranges: vec![0..9], next_glyph: None }));
        assert_eq!(current.shared.generation.load(Ordering::Acquire), generation);
        assert!(!current.shared.paused.load(Ordering::Acquire));
        assert_eq!(f32::from_bits(current.shared.rate.load(Ordering::Acquire)), 1.5);
        assert_eq!(*current.shared.pauses.lock().unwrap(), Pauses::NATURAL);
        assert_eq!(current.shared.count.load(Ordering::Acquire), 2);
        assert_eq!(current.shared.end.load(Ordering::Acquire), usize::MAX);
        drop(former);
        current.provide_chunk(generation, 1, 20, Some(SpeechChunk { text: "Current reader.".into(), glyph_ranges: vec![20..35], next_glyph: None }));
        assert_eq!(receive(&worker.generated), "Current reader.");
        worker.synchronize();
        let samples = receive(&worker.opened);
        worker.player();
        receive(&worker.retired);
        assert_eq!(worker.loads.load(Ordering::Acquire), 1);
        consume_audio(&samples, 1);
        assert!(matches!(reader_event(&current.events), SpeechEvent::Started { generation: started, index: 1, .. } if started == generation));
        assert!(worker.speech.events.try_recv().is_ok(), "the original fixture reader received its takeover stop");
        assert!(worker.speech.events.try_recv().is_err(), "inactive readers must not consume the current reader's events");
    }

    #[test]
    fn skipping_paused_audio_prepares_the_new_item_without_waiting_for_a_callback() {
        let worker = TestWorker::new(25600);
        worker.speech.replace(vec!["First item.".into(), "Second item.".into()]);
        let _old_samples = receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        worker.speech.pause();
        let generation = worker.speech.skip_to(1);
        assert_eq!(receive(&worker.generated), "Second item.");
        let samples = receive(&worker.opened);
        receive(&worker.retired);
        let player = worker.player();
        assert!(player.is_paused());
        assert!(worker.speech.shared.paused.load(Ordering::Acquire));
        assert_eq!(worker.loads.load(Ordering::Acquire), 1);
        worker.speech.resume();
        consume_audio(&samples, 1);
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: started, index: 1, .. } if started == generation));
    }

    #[test]
    fn virtual_silence_counts_towards_buffering_and_drop_reclaims_the_tail() {
        let shared = shared();
        let (events, _) = async_channel::unbounded();
        let mut audio = source(&shared, &events, 0, vec![0.25; 64]);
        audio.samples = OwnedPcm::new(vec![0.25; 64], NonZeroU32::new(22050).unwrap(), 256);
        audio.remaining += 256;
        audio.buffered.fetch_add(256, Ordering::AcqRel);
        audio.refill_at = 256;
        let (wake, received) = mpsc::channel();
        audio.wake = wake;
        let buffered = audio.buffered.clone();
        assert_eq!(audio.by_ref().take(64).collect::<Vec<_>>(), vec![0.25; 64]);
        assert_eq!(audio.by_ref().take(65).collect::<Vec<_>>(), vec![0.0; 65]);
        assert_eq!(buffered.load(Ordering::Acquire), 192);
        assert!(matches!(received.try_recv(), Ok(Command::Wake)), "silence consumption crosses the refill threshold too");
        assert!(received.try_recv().is_err());
        drop(audio);
        assert_eq!(buffered.load(Ordering::Acquire), 0, "drop includes silence and the uncredited final sample");
        assert!(matches!(received.try_recv(), Ok(Command::Wake)));
    }

    #[test]
    fn pause_resume_keeps_exact_samples_and_queued_item() {
        let shared = shared();
        let (events, received) = async_channel::unbounded();
        let (player, mut output) = Player::new();
        let player = Arc::new(player);
        *shared.player.lock().unwrap() = Some(player.clone());
        let (commands, _receiver) = mpsc::channel();
        let speech = DesktopSpeech::test_handle(commands, events.clone(), received.clone(), shared.clone());
        let (baseline, mut expected) = Player::new();
        for index in 0..2 {
            let samples: Vec<_> = (0..4096).map(|sample| (index * 4096 + sample + 1) as f32 / 10000.0).collect();
            baseline.append(SamplesBuffer::new(NonZeroU16::new(1).unwrap(), NonZeroU32::new(22050).unwrap(), samples.clone()));
            player.append(source(&shared, &events, index, samples));
        }
        for _ in 0..1000 {
            assert_eq!(output.next(), expected.next());
        }
        speech.pause();
        // Rodio applies control changes at its next control interval. Account
        // for those samples before checking the fully paused source.
        let mut paused = false;
        for _ in 0..1000 {
            let sample = output.next().unwrap();
            if sample == 0.0 {
                paused = true;
                break;
            }
            assert_eq!(Some(sample), expected.next());
        }
        assert!(paused);
        for _ in 0..4096 {
            assert_eq!(output.next(), Some(0.0));
        }
        assert_eq!(shared.generation.load(Ordering::Acquire), 4);
        assert_eq!(shared.cursor.load(Ordering::Acquire), 0);
        assert!(matches!(received.try_recv(), Ok(SpeechEvent::Started { index: 0, .. })));
        assert!(received.try_recv().is_err());
        speech.resume();
        let first = (0..1000).find_map(|_| output.next().filter(|sample| *sample != 0.0)).expect("resume");
        assert_eq!(Some(first), expected.next());
        for _ in 0..6000 {
            assert_eq!(output.next(), expected.next());
        }
        assert!(matches!(received.try_recv(), Ok(SpeechEvent::Started { index: 1, .. })));
        speech.stop();
        *shared.player.lock().unwrap() = None;
    }

    #[test]
    fn navigating_back_reuses_completed_audio_and_announces_the_current_position() {
        let worker = TestWorker::new(1024 * AUDIO_BUFFER_SECONDS);
        worker.speech.replace(vec!["First item.".into(), "Second item.".into()]);
        let _original = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "First item.");
        worker.synchronize();
        worker.speech.pause();
        worker.speech.skip_to(1);
        let _second = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Second item.");
        worker.synchronize();
        let generation = worker.speech.skip_to(0);
        let replay = receive(&worker.opened);
        worker.synchronize();
        assert!(worker.generated.try_recv().is_err(), "navigation must reuse completed synthesis");
        assert!(worker.player().is_paused(), "navigation preserves pause state");
        worker.speech.resume();
        consume_audio(&replay, 1);
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: current, index: 0, .. } if current == generation));
    }

    #[test]
    fn restarting_narration_reuses_audio_with_fresh_reader_highlights() {
        let worker = TestWorker::new(1024 * AUDIO_BUFFER_SECONDS);
        let generation = worker.speech.read_items_from(1, 0, 0);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { .. }));
        worker.speech.provide_chunk(generation, 0, 0, Some(SpeechChunk { text: "Same words.".into(), glyph_ranges: vec![0..11], next_glyph: None }));
        let _original = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Same words.");
        worker.synchronize();
        worker.speech.cancel();
        let generation = worker.speech.read_items_from(3, 2, 40);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 2, glyph_start: 40, .. }));
        worker.speech.provide_chunk(generation, 2, 40, Some(SpeechChunk { text: "Same words.".into(), glyph_ranges: vec![40..51], next_glyph: None }));
        let replay = receive(&worker.opened);
        worker.synchronize();
        assert!(worker.generated.try_recv().is_err());
        consume_audio(&replay, 1);
        assert!(matches!(next_event(&worker), SpeechEvent::Started { generation: current, index: 2, glyph_ranges } if current == generation && glyph_ranges == vec![40..51]));
    }

    #[test]
    fn low_buffer_signal_fires_once_before_the_chunk_ends() {
        let shared = shared();
        let (events, _) = async_channel::unbounded();
        let mut audio = source(&shared, &events, 0, vec![0.25; 1000]);
        let (wake, received) = mpsc::channel();
        audio.wake = wake;
        audio.refill_at = 256;
        let buffered = audio.buffered.clone();
        for _ in 0..768 {
            assert_eq!(audio.next(), Some(0.25));
        }
        assert!(matches!(received.try_recv(), Ok(Command::Wake)));
        assert!(received.try_recv().is_err());
        for _ in 0..200 {
            assert_eq!(audio.next(), Some(0.25));
        }
        assert!(received.try_recv().is_err(), "consumption below the threshold must not keep waking the worker");
        drop(audio);
        assert_eq!(buffered.load(Ordering::Acquire), 0);
        assert!(matches!(received.try_recv(), Ok(Command::Wake)), "retirement frees a queue slot");
    }

    #[test]
    fn worker_refills_before_a_long_chunk_ends_and_ignores_stale_stops() {
        let worker = TestWorker::new(1024 * AUDIO_BUFFER_SECONDS);
        let generation = worker.speech.replace(vec!["First. Second. Third.".into()]);
        let samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "First.");
        let player = worker.player();
        assert!(worker.generated.try_recv().is_err(), "a minute of audio is enough to sleep");
        worker.speech.commands.send(Command::Stop { generation: generation - 1 }).unwrap();
        consume_audio(&samples, 128);
        assert_eq!(receive(&worker.generated), "Second.");
        wait_until(|| player.len() == 2);
        assert!(worker.retired.try_recv().is_err(), "an old stop must not close the new session");
        assert!(worker.generated.try_recv().is_err(), "the worker waits again once refilled");
    }

    #[test]
    fn audio_queue_fills_by_duration_instead_of_stopping_at_three_chunks() {
        let worker = TestWorker::new(1024);
        worker.speech.replace(vec![(0..AUDIO_BUFFER_SECONDS + 10).map(|index| format!("Short {}. ", "a".repeat(index + 1))).collect()]);
        let samples = receive(&worker.opened);
        for index in 0..AUDIO_BUFFER_SECONDS {
            assert_eq!(receive(&worker.generated), format!("Short {}.", "a".repeat(index + 1)));
        }
        worker.synchronize();
        assert_eq!(worker.player().len(), AUDIO_BUFFER_SECONDS);
        assert!(worker.generated.try_recv().is_err(), "stop generating when a minute of audio is queued");
        consume_audio(&samples, 128);
        assert_eq!(receive(&worker.generated), format!("Short {}.", "a".repeat(AUDIO_BUFFER_SECONDS + 1)));
        worker.synchronize();
        assert!(worker.generated.try_recv().is_err(), "refill only the consumed duration");
    }

    #[test]
    fn supplied_text_starts_with_a_short_chunk_and_continues_without_losing_words() {
        let worker = TestWorker::new(1024);
        let text = format!("{} until the chapter ends.", "Ångström measurements continue ".repeat(25));
        worker.speech.replace(vec![text.clone()]);
        let first = receive(&worker.generated);
        assert!(first.chars().count() <= FIRST_CHUNK_CHARS);
        let second = receive(&worker.generated);
        assert!(second.chars().count() > FIRST_CHUNK_CHARS);
        let mut spoken = first + &second;
        let expected = text.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        while spoken.chars().filter(|c| !c.is_whitespace()).count() < expected.chars().count() {
            spoken.push_str(&receive(&worker.generated));
        }
        assert_eq!(spoken.chars().filter(|c| !c.is_whitespace()).collect::<String>(), expected);
    }

    #[test]
    fn settings_changes_discard_buffered_audio_and_preserve_pause() {
        let worker = TestWorker::new(1024);
        worker.speech.replace(vec![(0..AUDIO_BUFFER_SECONDS + 10).map(|index| format!("Current {}. ", "a".repeat(index + 1))).collect()]);
        let old_samples = receive(&worker.opened);
        for _ in 0..AUDIO_BUFFER_SECONDS {
            receive(&worker.generated);
        }
        worker.player();
        worker.speech.pause();
        let old_generation = worker.speech.client.generation.load(Ordering::Acquire);
        let generation = worker.speech.set_rate(1.5);
        assert!(generation > old_generation);
        for index in 0..AUDIO_BUFFER_SECONDS {
            assert_eq!(receive(&worker.generated), format!("Current {}.", "a".repeat(index + 1)));
        }
        receive(&worker.opened);
        worker.synchronize();
        assert!(worker.player().is_paused());
        assert_eq!(f32::from_bits(worker.speech.shared.rate.load(Ordering::Acquire)), 1.5);
        assert_eq!(worker.loads.load(Ordering::Acquire), 1);
        for _ in 0..256 {
            assert_ne!(old_samples.lock().unwrap().next(), Some(0.25), "old buffered speech must not resume");
        }
        let next = worker.speech.set_pauses(Pauses { sentence_scale: 1.5, passage_gap: 0.4 });
        assert!(next > generation);
        for index in 0..AUDIO_BUFFER_SECONDS {
            assert_eq!(receive(&worker.generated), format!("Current {}.", "a".repeat(index + 1)));
        }
        receive(&worker.opened);
        worker.synchronize();
        assert!(worker.player().is_paused());
    }

    #[test]
    fn reader_chunks_keep_source_ranges_and_request_the_next_glyph() {
        let worker = TestWorker::new(25600);
        let generation = worker.speech.read_items_from(1, 0, 47);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, glyph_start: 47, .. }));
        worker.speech.provide_chunk(generation, 0, 47, Some(SpeechChunk { text: "First. Still first chunk.".into(), glyph_ranges: vec![47..70, 80..90], next_glyph: Some(120) }));
        let samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "First. Still first chunk.", "reader chunks must not be split again");
        worker.player();
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, glyph_start: 120, max_chars: MAX_CHUNK_CHARS, .. }));
        assert!(worker.speech.events.try_recv().is_err(), "highlighting starts with playback, not synthesis");
        consume_audio(&samples, 128);
        match next_event(&worker) {
            SpeechEvent::Started { index: 0, glyph_ranges, .. } => assert_eq!(glyph_ranges, [47..70, 80..90]),
            other => panic!("unexpected event: {other:?}"),
        }
        worker.speech.provide_chunk(generation, 0, 120, Some(SpeechChunk { text: "Last chunk.".into(), glyph_ranges: vec![120..131], next_glyph: None }));
        assert_eq!(receive(&worker.generated), "Last chunk.");
        wait_until(|| worker.player().len() == 2);
        consume_audio(&samples, 25473);
        match next_event(&worker) {
            SpeechEvent::Started { index: 0, glyph_ranges, .. } => assert_eq!(glyph_ranges, [120..131]),
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(worker.speech.events.try_recv().is_err(), "the final chunk does not request more text");
    }

    #[test]
    fn text_prefetch_overlaps_synthesis_and_navigation_discards_its_response() {
        let (release, gate) = mpsc::channel();
        let worker = TestWorker::with_controls(1024, Duration::ZERO, VOICE_IDLE_TIMEOUT, Some(Arc::new(Mutex::new(gate))));
        let generation = worker.speech.read_items_from(3, 0, 0);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, glyph_start: 0, max_chars: FIRST_CHUNK_CHARS, .. }));
        worker.speech.provide_chunk(generation, 0, 0, Some(SpeechChunk { text: "Opening sentence.".into(), glyph_ranges: vec![0..17], next_glyph: Some(100) }));
        assert_eq!(receive(&worker.generated), "Opening sentence.");
        // Inference is still blocked. The reader can already extract the next
        // sentence and deliver it while the worker is occupied.
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, glyph_start: 100, max_chars: MAX_CHUNK_CHARS, .. }));
        worker.speech.provide_chunk(generation, 0, 100, Some(SpeechChunk { text: "Stale prefetched sentence.".into(), glyph_ranges: vec![100..125], next_glyph: None }));
        let next = worker.speech.skip_to(2);
        release.send(()).unwrap();
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { generation, index: 2, glyph_start: 0, max_chars: FIRST_CHUNK_CHARS } if generation == next));
        worker.speech.provide_chunk(next, 2, 0, Some(SpeechChunk { text: "New position.".into(), glyph_ranges: vec![200..213], next_glyph: None }));
        assert_eq!(receive(&worker.generated), "New position.");
        release.send(()).unwrap();
        worker.player();
        worker.synchronize();
        assert!(worker.generated.try_recv().is_err(), "discard stale prefetched text before synthesis");
    }

    #[test]
    fn skipping_discards_old_chunk_responses_and_starts_at_the_items_first_glyph() {
        let worker = TestWorker::new(8192);
        let old = worker.speech.read_items_from(2, 0, 99);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 0, glyph_start: 99, .. }));
        let generation = worker.speech.skip_to(1);
        assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: 1, glyph_start: 0, .. }));
        let chunk = || Some(SpeechChunk { text: "New item.".into(), glyph_ranges: vec![200..209], next_glyph: None });
        worker.speech.provide_chunk(old, 0, 99, chunk());
        worker.speech.provide_chunk(generation, 1, 99, chunk());
        worker.synchronize();
        assert!(worker.generated.try_recv().is_err(), "only the matching request can supply audio");
        worker.speech.provide_chunk(generation, 1, 0, chunk());
        let samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "New item.");
        worker.player();
        consume_audio(&samples, 1);
        match next_event(&worker) {
            SpeechEvent::Started { generation: started, index: 1, glyph_ranges } => {
                assert_eq!(started, generation);
                assert_eq!(glyph_ranges, [200..209]);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn empty_reader_items_advance_without_loading_the_voice() {
        let worker = TestWorker::new(8192);
        let generation = worker.speech.read_items_from(2, 0, 0);
        for index in 0..2 {
            assert!(matches!(next_event(&worker), SpeechEvent::Requested { index: requested, glyph_start: 0, .. } if requested == index));
            worker.speech.provide_chunk(generation, index, 0, None);
        }
        assert_eq!(finished_generation(&worker), generation);
        assert_eq!(worker.loads.load(Ordering::Relaxed), 0);
        assert!(worker.opened.try_recv().is_err());
    }

    #[test]
    fn stop_closes_paused_output_and_restart_reuses_the_loaded_voice() {
        let worker = TestWorker::new(8192);
        worker.speech.replace(vec!["First.".into()]);
        let samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "First.");
        worker.player();
        worker.speech.pause();
        worker.speech.stop();
        receive(&worker.retired);
        assert!(worker.speech.shared.player.lock().unwrap().is_none());
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
        drop(samples);
        worker.speech.replace(vec!["Again.".into()]);
        let _samples = receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Again.");
        worker.player();
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1, "stop releases the device while retaining the voice");
    }

    #[test]
    fn releasing_session_unloads_voice_and_audio_without_waiting_for_idle() {
        let worker = TestWorker::new(8192);
        worker.speech.replace(vec!["First.".into()]);
        receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        worker.speech.pause();
        worker.speech.stop();
        worker.speech.release_resources();
        receive(&worker.retired);
        receive(&worker.unloaded);
        worker.synchronize();
        assert!(worker.speech.shared.player.lock().unwrap().is_none());
        assert!(worker.speech.shared.output.lock().unwrap().is_none());
        assert!(worker.speech.shared.route.lock().unwrap().is_none());
        worker.speech.replace(vec!["Again.".into()]);
        receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Again.");
        assert_eq!(worker.loads.load(Ordering::Relaxed), 2, "starting again must reload the released voice");
    }

    #[test]
    fn superseded_release_and_dropping_an_inactive_endpoint_keep_the_current_reader_alive() {
        let worker = TestWorker::new(8192);
        let former = DesktopSpeech::for_worker(worker.speech.commands.clone(), worker.speech.shared.clone());
        former.replace(vec!["First.".into()]);
        receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        let hold = worker.hold();
        former.release_resources();
        let current = DesktopSpeech::for_worker(worker.speech.commands.clone(), worker.speech.shared.clone());
        let generation = current.replace(vec!["Current reader.".into()]);
        drop(former);
        drop(hold);
        assert_eq!(receive(&worker.generated), "Current reader.");
        worker.synchronize();
        assert!(worker.unloaded.try_recv().is_err(), "a stale release must not unload the active voice");
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
        let inactive = DesktopSpeech::for_worker(worker.speech.commands.clone(), worker.speech.shared.clone());
        drop(inactive);
        worker.synchronize();
        assert!(worker.unloaded.try_recv().is_err());
        assert!(current.shared.player.lock().unwrap().is_some());
        assert!(current.shared.output.lock().unwrap().is_some());
        assert_eq!(current.shared.generation.load(Ordering::Acquire), generation);
        assert!(current.is_current());
    }

    #[test]
    fn stopped_voice_unloads_at_the_idle_deadline_and_reloads_on_restart() {
        let idle = Duration::from_millis(100);
        let worker = TestWorker::with_timeouts(8192, Duration::ZERO, idle);
        worker.speech.replace(vec!["First.".into()]);
        receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        let stopped = Instant::now();
        worker.speech.stop();
        receive(&worker.retired);
        receive(&worker.unloaded);
        assert!(stopped.elapsed() >= idle, "keep the voice warm during the idle grace period");
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
        worker.speech.replace(vec!["Again.".into()]);
        receive(&worker.opened);
        assert_eq!(receive(&worker.generated), "Again.");
        worker.player();
        assert_eq!(worker.loads.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn restarting_cancels_voice_retirement_and_a_stale_stop_cannot_restore_it() {
        let idle = Duration::from_millis(100);
        let worker = TestWorker::with_timeouts(8192, Duration::ZERO, idle);
        worker.speech.replace(vec!["First.".into()]);
        receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        worker.speech.stop();
        receive(&worker.retired);
        let generation = worker.speech.replace(vec!["Again.".into()]);
        receive(&worker.opened);
        receive(&worker.generated);
        worker.player();
        worker.speech.commands.send(Command::Stop { generation: generation - 1 }).unwrap();
        worker.synchronize();
        assert!(worker.unloaded.recv_timeout(idle * 2).is_err(), "active playback must keep its model beyond the old idle deadline");
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
        worker.speech.stop();
        receive(&worker.retired);
        receive(&worker.unloaded);
    }

    #[test]
    fn pausing_keeps_the_voice_and_pcm_beyond_the_idle_timeout() {
        let idle = Duration::from_millis(50);
        let worker = TestWorker::with_timeouts(8192, Duration::ZERO, idle);
        worker.speech.replace(vec!["First.".into()]);
        let samples = receive(&worker.opened);
        receive(&worker.generated);
        let player = worker.player();
        assert!(!receive(&worker.device_states), "start resumes the device only after audio is queued");
        consume_audio(&samples, 128);
        worker.speech.pause();
        assert!(receive(&worker.device_states), "pause suspends device output immediately");
        worker.synchronize();
        assert!(worker.unloaded.recv_timeout(idle * 2).is_err());
        assert!(worker.retired.try_recv().is_err());
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
        assert_eq!(player.len(), 1);
        worker.speech.resume();
        assert!(!receive(&worker.device_states));
        assert!(Arc::ptr_eq(&player, &worker.player()), "resume keeps the same player and its retained sources");
        consume_audio(&samples, 1);
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn exhausted_audio_finishes_after_resume_without_a_polling_timer() {
        let worker = TestWorker::new(16);
        let generation = worker.speech.replace(vec!["First.".into()]);
        let samples = receive(&worker.opened);
        receive(&worker.generated);
        let player = worker.player();
        consume_audio(&samples, 16);
        // Simulate the boundary where pause is published just as the output
        // consumes the final sample, before Rodio applies its pause control.
        worker.speech.shared.paused.store(true, Ordering::Release);
        wait_until(|| {
            samples.lock().unwrap().next();
            player.empty()
        });
        worker.speech.resume();
        assert_eq!(finished_generation(&worker), generation);
    }

    #[test]
    fn natural_completion_drains_device_buffers_before_retiring_output() {
        let delay = Duration::from_millis(100);
        let worker = TestWorker::with_drain_time(16, delay);
        let generation = worker.speech.replace(vec!["First.".into()]);
        let samples = receive(&worker.opened);
        receive(&worker.generated);
        let player = worker.player();
        consume_audio(&samples, 16);
        wait_until(|| {
            samples.lock().unwrap().next();
            player.empty()
        });
        assert_eq!(finished_generation(&worker), generation);
        let stopped = Instant::now();
        worker.speech.stop();
        receive(&worker.retired);
        assert!(stopped.elapsed() >= delay, "closing the device immediately could cut off its final buffers");
        assert!(worker.speech.shared.player.lock().unwrap().is_none());
    }

    #[test]
    fn a_new_batch_reuses_output_during_the_completion_drain() {
        let worker = TestWorker::with_drain_time(16, Duration::from_secs(1));
        let generation = worker.speech.replace(vec!["First.".into()]);
        let samples = receive(&worker.opened);
        receive(&worker.generated);
        let player = worker.player();
        consume_audio(&samples, 16);
        wait_until(|| {
            samples.lock().unwrap().next();
            player.empty()
        });
        assert_eq!(finished_generation(&worker), generation);
        worker.speech.stop();
        worker.synchronize();
        assert!(worker.retired.try_recv().is_err());
        worker.speech.replace(vec!["Again.".into()]);
        assert_eq!(receive(&worker.generated), "Again.");
        worker.player();
        assert!(worker.opened.try_recv().is_err(), "cancel the retirement deadline and reuse the output");
        assert!(worker.retired.try_recv().is_err());
        assert_eq!(worker.loads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn skip_preserves_pause_and_old_audio_cannot_start() {
        let shared = shared();
        shared.paused.store(true, Ordering::Release);
        let (events, received) = async_channel::unbounded();
        let mut old = source(&shared, &events, 0, vec![0.25; 1000]);
        let (commands, receiver) = mpsc::channel();
        let speech = DesktopSpeech::test_handle(commands, events.clone(), received.clone(), shared.clone());
        let generation = speech.skip_to(1);
        assert!(shared.paused.load(Ordering::Acquire));
        assert_eq!(shared.cursor.load(Ordering::Acquire), 1);
        assert!(matches!(receiver.recv().unwrap(), Command::Skip { generation: next, index: 1 } if next == generation));
        assert_eq!(shared.count.load(Ordering::Acquire), 2);
        assert_eq!(old.next(), None);
        assert!(received.try_recv().is_err());
        speech.cancel();
        assert_eq!(shared.count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn chapter_limit_preserves_current_audio_and_discards_later_items() {
        let shared = shared();
        let (events, received) = async_channel::unbounded();
        let mut current = source(&shared, &events, 0, vec![0.1, 0.2, 0.3]);
        let mut later = source(&shared, &events, 1, vec![0.4]);
        let (commands, _receiver) = mpsc::channel();
        let speech = DesktopSpeech::test_handle(commands, events.clone(), received, shared.clone());
        assert_eq!(current.next(), Some(0.1));
        assert_eq!(speech.set_end_fragment(Some(1)), 4);
        assert_eq!(current.next(), Some(0.2));
        assert_eq!(later.next(), None);
        assert_eq!(speech.set_end_fragment(None), 4);
        assert_eq!(current.next(), Some(0.3));
    }

    #[test]
    fn lazy_worker_requests_only_the_current_item_before_loading_the_model() {
        let shared = shared();
        let (commands, receiver) = mpsc::channel();
        let (events, received) = async_channel::unbounded();
        let wake = commands.clone();
        let worker = thread::spawn(move || super::worker(receiver, wake, events.into(), shared));
        commands.send(Command::Read { generation: 4, count: 100000, start: 500, glyph_start: 12 }).unwrap();
        assert!(matches!(received.recv_blocking().unwrap(), SpeechEvent::Requested { generation: 4, index: 500, glyph_start: 12, .. }));
        assert!(received.try_recv().is_err());
        commands.send(Command::Shutdown).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn dropping_speech_cancels_current_and_queued_audio() {
        let shared = shared();
        let (events, received) = async_channel::unbounded();
        let (player, mut output) = Player::new();
        let player = Arc::new(player);
        *shared.player.lock().unwrap() = Some(player.clone());
        let (commands, receiver) = mpsc::channel();
        let speech = DesktopSpeech::test_handle(commands, events.clone(), received.clone(), shared.clone());
        for index in 0..2 {
            player.append(source(&shared, &events, index, vec![0.25; 22050]));
        }
        for _ in 0..1000 {
            output.next();
        }
        assert!(matches!(received.try_recv(), Ok(SpeechEvent::Started { index: 0, .. })));
        drop(speech);
        assert!(matches!(receiver.recv().unwrap(), Command::Stop { .. }));
        assert!(matches!(receiver.recv().unwrap(), Command::Release { .. }), "dropping the last session releases the loaded voice");
        assert!(receiver.try_recv().is_err(), "dropping a reader must not shut down the shared worker");
        for _ in 0..1000 {
            output.next();
        }
        assert!(player.empty());
        assert!(received.try_recv().is_err());
        *shared.player.lock().unwrap() = None;
    }

    #[test]
    fn speech_eligibility_respects_declared_languages() {
        assert!(supports_languages(&[]));
        assert!(supports_languages(&["en-GB".into()]));
        assert!(!supports_languages(&["sv".into()]));
    }
}
