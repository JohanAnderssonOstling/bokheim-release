//! Android owns playback in a foreground service. This module owns the seekable
//! playback source and forwards service position events to library persistence,
//! independently of the GPUI activity's rendering/lifetime.

use crate::{AudiobookBook, PlaybackPosition, ReadSeek, lock_state, playback_progress, position_save_required};
#[cfg(not(target_arch = "wasm32"))]
use library_backend::library::AudioStreamControl;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::task::{Context, Waker};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

type Dispatch = dyn Fn(&str) -> Result<(), String> + Send + Sync;
struct Bridge {
    dispatch: Box<Dispatch>,
}
static BRIDGE: OnceLock<Bridge> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SESSIONS: OnceLock<Mutex<HashMap<u64, Arc<Session>>>> = OnceLock::new();
fn sessions() -> &'static Mutex<HashMap<u64, Arc<Session>>> {
    SESSIONS.get_or_init(Default::default)
}

pub fn initialize(directory: PathBuf, dispatch: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) {
    if BRIDGE.get().is_none() {
        // Sources belong to a process-local session. Remove staging left by a
        // killed process, but never remove files when reattaching an activity.
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("audiobook-") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    let _ = BRIDGE.set(Bridge { dispatch: Box::new(dispatch) });
}
fn dispatch(command: Value) -> Result<(), String> {
    (BRIDGE.get().ok_or("Android audio bridge is not initialized")?.dispatch)(&command.to_string())
}
/// A file handoff leaves an idle service ready for its replacement. Closing
/// or failing that handoff must also retire the idle service; a newer book's
/// nonzero ID makes this cancellation harmless to replacement sessions.
pub(crate) fn stop_detached_service() {
    let _ = dispatch(json!({"command":"stop", "id":0}));
}
#[derive(Default)]
struct Status {
    position: Duration,
    playing: bool,
    buffering: bool,
    ended: bool,
    confirmed: bool,
    error: Option<String>,
    last_saved: Option<Duration>,
    sequence: u64,
    rate: Option<f64>,
    sleep_finished: bool,
    remote_seek: Option<Duration>,
    initial_seek: Option<Duration>,
    requested: bool,
    sleep_reset: Option<(Option<web_time::Instant>, Option<Duration>)>,
}
struct Session {
    read_generation: AtomicU64,
    control: Option<AudioStreamControl>,
    release: (async_channel::Sender<()>, async_channel::Receiver<()>),
    stop_error: Mutex<Option<String>>,
    reader: Mutex<Option<Box<dyn ReadSeek>>>,
    _file: Option<File>,
    duration: Duration,
    global_offset: Duration,
    global_duration: Duration,
    updates: async_channel::Sender<PlaybackPosition>,
    status: Arc<Mutex<Status>>,
}
/// Retains only service observations, not the source, file, or progress sender.
#[derive(Clone)]
pub(crate) struct PositionSnapshot(Arc<Mutex<Status>>);
impl PositionSnapshot {
    pub fn take_seek_request(&self) -> Option<Duration> {
        lock_state(&self.0).remote_seek.take()
    }
    pub fn last_position(&self) -> Duration {
        lock_state(&self.0).position
    }
    pub fn confirmed_position(&self) -> Option<Duration> {
        let status = lock_state(&self.0);
        status.confirmed.then_some(status.position)
    }
}
/// Called by Media3 on its loader thread; reads only the requested audio bytes.
pub fn read_source(id: u64, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    if length > 256 * 1024 {
        return Err("audio read is too large".into());
    }
    let session = lock_state(sessions()).get(&id).cloned().ok_or("audio session ended")?;
    let mut guard = session.reader.lock().map_err(|_| "audio source reader failed; reopen the audiobook".to_owned())?;
    let generation = session.read_generation.load(Ordering::Acquire);
    if let Some(control) = &session.control {
        if !control.begin_seek(generation) {
            return Err("audio read superseded".into());
        }
    }
    let reader = guard.as_mut().ok_or("audio session has no stream")?;
    reader.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut bytes = vec![0; length];
    let count = reader.read(&mut bytes).map_err(|e| e.to_string())?;
    if session.read_generation.load(Ordering::Acquire) != generation {
        return Err("audio read superseded".into());
    }
    bytes.truncate(count);
    Ok(bytes)
}
/// Media3 reports seek discontinuities from both application and media controls.
/// Invalidate the blocked range; the next loader read acknowledges the generation.
pub fn interrupt_source(id: u64) {
    let session = lock_state(sessions()).get(&id).cloned();
    if let Some(session) = session {
        if let Some(control) = &session.control {
            let generation = session.read_generation.fetch_add(1, Ordering::AcqRel) + 1;
            control.request_seek(generation);
        }
    }
}
pub(crate) struct AudioEngine {
    id: u64,
    session: Arc<Session>,
    load: Value,
    started: AtomicBool,
}
impl AudioEngine {
    pub(crate) fn open(
        mut reader: Box<dyn ReadSeek>, file: Option<File>, control: Option<AudioStreamControl>, book: &AudiobookBook, track: Option<&book_model::AudiobookTrack>, initial: Duration, updates: async_channel::Sender<PlaybackPosition>,
    ) -> Result<Self, String> {
        BRIDGE.get().ok_or("Android audio bridge is not initialized")?;
        #[cfg(any(target_os = "android", target_os = "linux"))]
        let path = file.as_ref().map(|file| {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
        });
        #[cfg(not(any(target_os = "android", target_os = "linux")))]
        let path: Option<PathBuf> = None;
        let length = if path.is_none() { reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? } else { 0 };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let global_offset = track.map(|track| Duration::from_millis(track.start_ms)).unwrap_or_default();
        let duration = track.map(|track| Duration::from_millis(track.end_ms - track.start_ms)).unwrap_or(book.duration);
        let chapter_ends = book
            .chapters
            .iter()
            .filter_map(|chapter| (chapter.end() > global_offset && chapter.start() < global_offset + duration).then_some(chapter.end().saturating_sub(global_offset).min(duration).as_millis() as u64))
            .collect::<Vec<_>>();
        let chapters = book.chapters.iter().map(|chapter| {
            json!({"start":chapter.start().as_millis() as u64, "end":chapter.end().as_millis() as u64, "title":crate::chapter_display_title(&chapter.title)})
        }).collect::<Vec<_>>();
        let cover = book.cover.as_ref().map(|cover| cover.bytes());
        let load = json!({
            "command":"load", "id":id, "path":path, "length":length,
            "title":book.title, "author":book.author, "cover":cover, "chapters":chapters,
            "offset":global_offset.as_millis() as u64, "duration":duration.as_millis() as u64,
            "position":initial.min(duration).as_millis() as u64, "chapterEnds":chapter_ends,
        });
        let session = Arc::new(Session {
            read_generation: AtomicU64::new(0),
            control,
            release: async_channel::bounded(1),
            stop_error: Mutex::new(None),
            reader: Mutex::new(path.is_none().then_some(reader)),
            _file: file,
            duration,
            global_offset,
            global_duration: book.duration,
            updates,
            status: Arc::new(Mutex::new(Status { position: initial.min(duration), ..Status::default() })),
        });
        Ok(Self { id, session, load, started: AtomicBool::new(false) })
    }
    pub fn play(&self) { self.start(true); }
    pub fn prepare_paused(&self) { self.start(false); }
    fn start(&self, playing: bool) {
        if let Some(control) = &self.session.control {
            control.set_paused(!playing);
        }
        let mut load = self.load.clone();
        {
            let mut status = lock_state(&self.session.status);
            if let Some(position) = status.initial_seek.take() { status.position = position; }
            status.requested = playing;
            load["position"] = json!(status.position.as_millis() as u64);
            load["sequence"] = json!(status.sequence);
        }
        load["playWhenReady"] = json!(playing);
        self.started.store(true, Ordering::Release);
        lock_state(sessions()).insert(self.id, self.session.clone());
        if let Err(error) = dispatch(load) {
            lock_state(&self.session.status).error = Some(error);
            lock_state(sessions()).remove(&self.id);
            self.session.release.0.close();
        }
    }
    pub fn take_seek_request(&self) -> Option<Duration> {
        self.position_snapshot().take_seek_request()
    }
    pub fn playback_requested(&self) -> bool {
        lock_state(&self.session.status).requested
    }
    pub fn set_artwork(&self, cover: &[u8]) -> Result<(), String> {
        dispatch(json!({"command":"artwork", "id":self.id, "cover":cover}))
    }
    pub fn position(&self) -> Duration {
        lock_state(&self.session.status).position
    }
    pub fn position_snapshot(&self) -> PositionSnapshot {
        PositionSnapshot(self.session.status.clone())
    }
    pub fn confirmed_position(&self) -> Option<Duration> {
        self.position_snapshot().confirmed_position()
    }
    pub fn rate(&self) -> f64 {
        lock_state(&self.session.status).rate.unwrap_or_else(|| self.load["speed"].as_f64().unwrap_or(1.0))
    }
    pub fn is_playing(&self) -> bool {
        lock_state(&self.session.status).playing
    }
    pub fn finished(&self) -> bool {
        let status = lock_state(&self.session.status);
        status.confirmed && status.ended && !status.playing && !status.buffering
    }
    pub fn is_buffering(&self) -> bool {
        lock_state(&self.session.status).buffering
    }
    pub fn toggle(&self) -> Result<(), String> {
        dispatch(json!({"command":"toggle", "id":self.id}))
    }
    pub fn pause(&self) -> Result<(), String> {
        dispatch(json!({"command":"pause", "id":self.id}))?;
        if let Some(control) = &self.session.control {
            control.set_paused(true);
        }
        Ok(())
    }
    pub fn seek_to(&self, position: Duration) -> Result<(), String> {
        if !self.started.load(Ordering::Acquire) {
            lock_state(&self.session.status).initial_seek = Some(position.min(self.session.duration));
            return Ok(());
        }
        let sequence = {
            let mut s = lock_state(&self.session.status);
            s.sequence += 1;
            s.remote_seek = None;
            s.confirmed = false;
            s.sequence
        };
        dispatch(json!({"command":"seek", "id":self.id, "position":position.min(self.session.duration).as_millis() as u64, "sequence":sequence}))
    }
    pub fn set_tempo(&mut self, tempo: f32) -> Result<(), String> {
        if !tempo.is_finite() || tempo <= 0.0 {
            return Err("Playback speed must be positive and finite".into());
        }
        self.load["speed"] = json!(tempo);
        if !self.started.load(Ordering::Acquire) {
            return Ok(());
        }
        dispatch(json!({"command":"speed", "id":self.id, "speed":tempo}))
    }
    pub fn set_sleep(&self, remaining: Option<Duration>, chapter_end: Option<Duration>) -> Result<(), String> {
        {
            let mut status = lock_state(&self.session.status);
            status.sleep_finished = false;
            status.sleep_reset = None;
        }
        dispatch(json!({"command":"sleep", "id":self.id,
            "remaining":remaining.map(|time| time.as_millis() as u64),
            "chapterEnd":chapter_end.map(|time| time.saturating_sub(self.session.global_offset).as_millis() as u64)}))
    }
    pub fn take_sleep_reset(&self) -> Option<(Option<web_time::Instant>, Option<Duration>)> {
        lock_state(&self.session.status).sleep_reset.take().map(|(deadline, chapter_end)| (deadline, chapter_end.map(|end| self.session.global_offset.saturating_add(end))))
    }
    pub fn take_sleep_finished(&self) -> bool {
        std::mem::take(&mut lock_state(&self.session.status).sleep_finished)
    }
    pub fn take_error(&self) -> Option<String> {
        lock_state(&self.session.status).error.take()
    }
    /// Retire this file while keeping the Android service available for the
    /// replacement, including a paused seek initiated with the screen locked.
    pub fn stop_for_seek(&self) {
        if self.started.load(Ordering::Acquire) {
            let _ = dispatch(json!({"command":"detach", "id":self.id}));
        }
    }
    pub fn stop(&self) {
        // Release can wait for the Media3 loader; unblock its native read first.
        if let Some(control) = &self.session.control {
            control.cancel();
        }
        if !self.started.load(Ordering::Acquire) || self.session.release.0.is_closed() {
            return;
        }
        if let Err(error) = dispatch(json!({"command":"stop", "id":self.id})) {
            log::warn!("Could not stop Android audio: {error}");
            *lock_state(&self.session.stop_error) = Some(error);
            self.session.release.0.close();
        }
    }

    pub fn release_completion(&self) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>>>> {
        self.release_completion_with_timeout(Duration::from_secs(30))
    }

    fn release_completion_with_timeout(&self, timeout: Duration) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>>>> {
        let started = self.started.load(Ordering::Acquire);
        let session = self.session.clone();
        Box::pin(async move {
            if !started {
                return Ok(());
            }
            // Closing this channel broadcasts release, after the final position
            // event has been enqueued for persistence by playback_state.
            use std::{future::Future, task::Poll};
            let mut released = std::pin::pin!(session.release.1.recv());
            let mut deadline = std::pin::pin!(async_io::Timer::after(timeout));
            std::future::poll_fn(|cx| {
                if released.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Ok(()));
                }
                if deadline.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Err("Android audio service did not acknowledge stop".to_owned()));
                }
                Poll::Pending
            })
            .await?;
            lock_state(&session.stop_error).clone().map_or(Ok(()), Err)
        })
    }
}

/// Called on the Java service thread, including while the screen is off.
pub fn playback_state(event: &str) {
    let Ok(event) = serde_json::from_str::<Value>(event) else { return };
    let Some(id) = event["id"].as_u64() else { return };
    let session = lock_state(sessions()).get(&id).cloned();
    let Some(session) = session else { return };
    let mut status = lock_state(&session.status);
    if let Some(target) = event["seekTarget"].as_u64() {
        if event["sequence"].as_u64().unwrap_or(0) >= status.sequence {
            status.remote_seek = Some(Duration::from_millis(target).min(session.global_duration));
        }
        return;
    }
    if let Some(reset) = event["sleepReset"].as_object() {
        status.sleep_reset = Some((reset.get("remaining").and_then(Value::as_u64).map(|millis| web_time::Instant::now() + Duration::from_millis(millis)), reset.get("chapterEnd").and_then(Value::as_u64).map(Duration::from_millis)));
    }
    status.sleep_finished |= event["sleepFinished"].as_bool().unwrap_or(false);
    let released = event["released"].as_bool().unwrap_or(false);
    if let Some(error) = event["error"].as_str() {
        status.error = Some(error.to_owned());
    }
    if event["sequence"].as_u64().unwrap_or(0) >= status.sequence {
        let playing = event["playing"].as_bool().unwrap_or(false);
        let force = released || event["force"].as_bool().unwrap_or(false) || playing != status.playing || !status.confirmed;
        status.confirmed = event["ready"].as_bool().unwrap_or(false);
        if status.confirmed {
            status.position = Duration::from_millis(event["position"].as_u64().unwrap_or(0)).min(session.duration);
        }
        status.playing = playing;
        status.buffering = event["buffering"].as_bool().unwrap_or(false);
        status.requested = event["requested"].as_bool().unwrap_or(playing || status.buffering);
        status.ended = event["ended"].as_bool().unwrap_or(false);
        if let Some(control) = &session.control {
            let requested = event["requested"].as_bool().unwrap_or(playing || status.buffering);
            control.set_paused(!requested);
        }
        status.rate = event["speed"].as_f64().filter(|rate| rate.is_finite() && *rate > 0.0);
        if status.confirmed && position_save_required(status.last_saved, status.position, force) {
            let position = session.global_offset.saturating_add(status.position);
            let _ = session.updates.try_send(PlaybackPosition::new(position, playback_progress(position, session.global_duration)));
            status.last_saved = Some(status.position);
        }
    }
    if released {
        if let Some(control) = &session.control {
            control.cancel();
        }
        status.playing = false;
        status.buffering = false;
        session.release.0.close();
        lock_state(sessions()).remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> (u64, Arc<Session>, async_channel::Receiver<PlaybackPosition>) {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (updates, receiver) = async_channel::unbounded();
        let session = Arc::new(Session {
            read_generation: AtomicU64::new(0),
            control: Some(Default::default()),
            release: async_channel::bounded(1),
            stop_error: Mutex::new(None),
            reader: Mutex::new(Some(Box::new(Cursor::new(Vec::<u8>::new())))),
            _file: None,
            duration: Duration::from_secs(600),
            global_offset: Duration::ZERO,
            global_duration: Duration::from_secs(600),
            updates,
            status: Arc::new(Mutex::new(Status::default())),
        });
        sessions().lock().unwrap().insert(id, session.clone());
        (id, session, receiver)
    }
    fn event(id: u64, sequence: u64, position: u64, playing: bool, ready: bool, released: bool) {
        playback_state(&json!({"id":id,"sequence":sequence,"position":position,"playing":playing,"ready":ready,"released":released}).to_string());
    }

    #[test]
    fn remote_chapter_seek_does_not_overwrite_confirmed_file_position() {
        let (id, session, _) = session();
        event(id, 0, 15_000, true, true, false);
        playback_state(&json!({"id":id,"seekTarget":60_000}).to_string());
        let mut state = session.status.lock().unwrap();
        assert_eq!(state.remote_seek.take(), Some(Duration::from_secs(60)));
        assert_eq!(state.position, Duration::from_secs(15));
        assert!(state.confirmed);
        assert!(state.playing);
        drop(state);
        sessions().lock().unwrap().remove(&id);
    }

    #[test]
    fn late_remote_seek_cannot_override_a_newer_application_seek() {
        let (id, session, _) = session();
        session.status.lock().unwrap().sequence = 2;
        playback_state(&json!({"id":id,"sequence":1,"seekTarget":60_000}).to_string());
        assert!(session.status.lock().unwrap().remote_seek.is_none());
        sessions().lock().unwrap().remove(&id);
    }

    #[test]
    fn a_poisoned_source_reader_returns_an_error_without_reusing_the_stream() {
        let (id, session, _) = session();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = session.reader.lock().unwrap();
            panic!("source reader failed");
        }));
        assert_eq!(read_source(id, 0, 2), Err("audio source reader failed; reopen the audiobook".into()));
        sessions().lock().unwrap().remove(&id);
    }

    #[test]
    fn seek_during_read_cannot_deliver_old_generation_bytes() {
        struct InterruptedReader {
            id: u64,
            interrupted: bool,
            bytes: Cursor<Vec<u8>>,
        }
        impl Read for InterruptedReader {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    interrupt_source(self.id);
                }
                Read::read(&mut self.bytes, out)
            }
        }
        impl Seek for InterruptedReader {
            fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
                Seek::seek(&mut self.bytes, position)
            }
        }
        let (id, session, _) = session();
        *session.reader.lock().unwrap() = Some(Box::new(InterruptedReader { id, interrupted: false, bytes: Cursor::new(vec![0, 1, 2, 3]) }));
        assert_eq!(read_source(id, 0, 2), Err("audio read superseded".into()));
        assert_eq!(read_source(id, 2, 2).unwrap(), vec![2, 3]);
        event(id, 0, 0, false, false, true);
    }

    #[test]
    fn release_during_pending_seek_retains_last_accepted_position() {
        let (id, session, _) = session();
        let snapshot = PositionSnapshot(session.status.clone());
        event(id, 0, 40_000, true, true, false);
        {
            let mut status = session.status.lock().unwrap();
            status.sequence = 1;
            status.confirmed = false;
        }
        // The seek has not been acknowledged; neither a stale event nor an
        // unready release may turn the requested target into a resume point.
        event(id, 0, 180_000, true, true, false);
        assert_eq!(snapshot.confirmed_position(), None);
        event(id, 1, 180_000, false, false, true);
        assert_eq!(snapshot.last_position(), Duration::from_secs(40));
        assert_eq!(snapshot.confirmed_position(), None);
    }

    #[test]
    fn missing_service_acknowledgement_fails_instead_of_allowing_removal() {
        let (id, session, _) = session();
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(true) };
        let mut released = engine.release_completion_with_timeout(Duration::ZERO);
        let mut poll = Context::from_waker(Waker::noop());
        assert!(matches!(released.as_mut().poll(&mut poll), std::task::Poll::Ready(Err(error)) if error.contains("did not acknowledge")));
        sessions().lock().unwrap().remove(&id);
    }
    #[test]
    fn streams_seekable_source_and_clamps_initial_position_before_starting() {
        let directory = tempfile::tempdir().unwrap();
        let commands = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorded = commands.clone();
        initialize(directory.path().to_owned(), move |command| {
            recorded.lock().unwrap().push(serde_json::from_str(command).unwrap());
            Ok(())
        });
        let book = AudiobookBook { title: "Example".into(), author: Some("Author".into()), narrator: None, duration: Duration::from_secs(60), chapters: Vec::new(), cover: None };
        let (updates, _) = async_channel::unbounded();
        struct TrackedReader {
            cursor: Cursor<&'static [u8]>,
            reads: Arc<AtomicU64>,
        }
        impl Read for TrackedReader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                Read::read(&mut self.cursor, bytes)
            }
        }
        impl Seek for TrackedReader {
            fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
                Seek::seek(&mut self.cursor, position)
            }
        }
        let reads = Arc::new(AtomicU64::new(0));
        let reader = TrackedReader { cursor: Cursor::new(b"seekable M4B bytes"), reads: reads.clone() };
        let mut audio = AudioEngine::open(Box::new(reader), None, None, &book, None, Duration::from_secs(90), updates).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 0, "opening must not copy the stream");
        assert!(audio.load["path"].is_null());
        assert_eq!(audio.load["length"], 18);
        assert!(commands.lock().unwrap().is_empty(), "Loading must not start abandoned reader requests");
        audio.set_tempo(1.5).unwrap();
        audio.stop();
        assert!(commands.lock().unwrap().is_empty(), "Preparing speed and stopping an unstarted reader must not contact a missing service");
        audio.seek_to(Duration::from_secs(30)).unwrap();
        assert!(commands.lock().unwrap().is_empty(), "A seek before loading must be included in the load command");
        audio.prepare_paused();
        assert_eq!(read_source(audio.id, 9, 3).unwrap(), b"M4B");
        assert_eq!(read_source(audio.id, 0, 8).unwrap(), b"seekable");
        assert!(read_source(audio.id, 18, 8).unwrap().is_empty());
        assert!(read_source(audio.id, 0, 256 * 1024 + 1).is_err());
        let command = commands.lock().unwrap()[0].clone();
        assert_eq!(command["speed"], 1.5);
        assert_eq!(command["position"], 30_000);
        assert_eq!(command["playWhenReady"], false);
        assert_eq!(command["title"], "Example");
        event(audio.id, 0, 60_000, false, true, true);
        let id = audio.id;
        drop(audio);
        assert!(read_source(id, 0, 1).is_err());
        #[cfg(target_os = "linux")]
        {
            let original = directory.path().join("original.m4b");
            std::fs::write(&original, b"original audiobook").unwrap();
            let file = File::open(&original).unwrap();
            let (updates, _) = async_channel::unbounded();
            // An empty reader proves playback did not copy its bytes.
            let audio = AudioEngine::open(Box::new(Cursor::new(Vec::<u8>::new())), Some(file), None, &book, None, Duration::ZERO, updates).unwrap();
            assert!(audio.session.reader.lock().unwrap().is_none());
            let descriptor_path = PathBuf::from(audio.load["path"].as_str().unwrap());
            audio.play();
            let id = audio.id;
            drop(audio);
            let renamed = directory.path().join("renamed.m4b");
            std::fs::rename(&original, &renamed).unwrap();
            assert_eq!(std::fs::read(&descriptor_path).unwrap(), b"original audiobook");
            use std::os::unix::fs::MetadataExt;
            let original_metadata = std::fs::metadata(&descriptor_path).unwrap();
            event(id, 0, 0, false, true, true);
            // Parallel audio/subprocess tests may reuse the numeric descriptor
            // immediately after release. Verify it no longer owns our file,
            // rather than requiring the descriptor number to remain unused.
            assert!(std::fs::metadata(&descriptor_path).map_or(true, |metadata| (metadata.dev(), metadata.ino()) != (original_metadata.dev(), original_metadata.ino())));
            assert_eq!(std::fs::read(renamed).unwrap(), b"original audiobook");
        }
    }
    #[test]
    fn terminal_start_failure_releases_source_without_saving_zero() {
        let (id, session, receiver) = session();
        playback_state(&json!({"id":id,"error":"Cannot start service","released":true}).to_string());
        assert_eq!(session.status.lock().unwrap().error.as_deref(), Some("Cannot start service"));
        assert!(receiver.try_recv().is_err());
        assert!(!sessions().lock().unwrap().contains_key(&id));
    }
}
