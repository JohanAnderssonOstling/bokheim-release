//! Android owns playback in a foreground service. This module owns the seekable
//! playback source and forwards service position events to library persistence,
//! independently of the GPUI activity's rendering/lifetime.
use crate::{AudiobookBook, PlaybackPosition, ReadSeek, playback_progress, position_save_required};
use serde_json::{Value, json};
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
    sleep_reset: Option<(Option<web_time::Instant>, Option<Duration>)>,
}
struct Session {
    read_generation: AtomicU64,
    control: Option<app::AudioStreamControl>,
    release: (async_channel::Sender<()>, async_channel::Receiver<()>),
    stop_error: Mutex<Option<String>>,
    reader: Mutex<Option<Box<dyn ReadSeek>>>,
    _file: Option<std::fs::File>,
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
    pub fn last_position(&self) -> Duration {
        self.0.lock().unwrap().position
    }
    pub fn confirmed_position(&self) -> Option<Duration> {
        let status = self.0.lock().unwrap();
        status.confirmed.then_some(status.position)
    }
}
/// Called by Media3 on its loader thread; reads only the requested audio bytes.
pub fn read_source(id: u64, offset: u64, length: usize) -> Result<Vec<u8>, String> {
    if length > 256 * 1024 {
        return Err("audio read is too large".into());
    }
    let session = sessions().lock().unwrap().get(&id).cloned().ok_or("audio session ended")?;
    let mut guard = session.reader.lock().unwrap();
    let generation = session.read_generation.load(Ordering::Acquire);
    if let Some(control) = &session.control {
        if !control.begin_seek(generation) {
            return Err("audio read superseded".into());
        }
    }
    let reader = guard.as_mut().ok_or("audio session has no stream")?;
    reader.seek(std::io::SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
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
    let session = sessions().lock().unwrap().get(&id).cloned();
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
        mut reader: Box<dyn ReadSeek>, file: Option<std::fs::File>, control: Option<app::AudioStreamControl>, book: &AudiobookBook, track: Option<&book_model::AudiobookTrack>, initial: Duration, updates: async_channel::Sender<PlaybackPosition>,
    ) -> Result<Self, String> {
        BRIDGE.get().ok_or("Android audio bridge is not initialized")?;
        #[cfg(any(target_os = "android", target_os = "linux"))]
        let path = file.as_ref().map(|file| {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
        });
        #[cfg(not(any(target_os = "android", target_os = "linux")))]
        let path: Option<PathBuf> = None;
        let length = if path.is_none() { reader.seek(std::io::SeekFrom::End(0)).map_err(|e| e.to_string())? } else { 0 };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let global_offset = track.map(|track| Duration::from_millis(track.start_ms)).unwrap_or_default();
        let duration = track.map(|track| Duration::from_millis(track.end_ms - track.start_ms)).unwrap_or(book.duration);
        let chapter_ends = book.chapters.iter().filter_map(|chapter| (chapter.end > global_offset && chapter.start < global_offset + duration).then_some(chapter.end.saturating_sub(global_offset).min(duration).as_millis() as u64)).collect::<Vec<_>>();
        let load = json!({"command":"load", "id":id, "path":path, "length":length, "title":book.title, "author":book.author, "position":initial.min(duration).as_millis() as u64, "chapterEnds":chapter_ends});
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
    pub fn play(&self) {
        if let Some(control) = &self.session.control {
            control.set_paused(false);
        }
        self.started.store(true, Ordering::Release);
        sessions().lock().unwrap().insert(self.id, self.session.clone());
        if let Err(error) = dispatch(self.load.clone()) {
            self.session.status.lock().unwrap().error = Some(error);
            sessions().lock().unwrap().remove(&self.id);
            self.session.release.0.close();
        }
    }
    pub fn position(&self) -> Duration {
        self.session.status.lock().unwrap().position
    }
    pub fn position_snapshot(&self) -> PositionSnapshot {
        PositionSnapshot(self.session.status.clone())
    }
    pub fn confirmed_position(&self) -> Option<Duration> {
        self.position_snapshot().confirmed_position()
    }
    pub fn rate(&self) -> f64 {
        self.session.status.lock().unwrap().rate.unwrap_or_else(|| self.load["speed"].as_f64().unwrap_or(1.0))
    }
    pub fn is_playing(&self) -> bool {
        self.session.status.lock().unwrap().playing
    }
    pub fn finished(&self) -> bool {
        let status = self.session.status.lock().unwrap();
        status.confirmed && status.ended && !status.playing && !status.buffering
    }
    pub fn is_buffering(&self) -> bool {
        self.session.status.lock().unwrap().buffering
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
        let sequence = {
            let mut s = self.session.status.lock().unwrap();
            s.sequence += 1;
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
            let mut status = self.session.status.lock().unwrap();
            status.sleep_finished = false;
            status.sleep_reset = None;
        }
        dispatch(json!({"command":"sleep", "id":self.id,
            "remaining":remaining.map(|time| time.as_millis() as u64),
            "chapterEnd":chapter_end.map(|time| time.saturating_sub(self.session.global_offset).as_millis() as u64)}))
    }
    pub fn take_sleep_reset(&self) -> Option<(Option<web_time::Instant>, Option<Duration>)> {
        self.session.status.lock().unwrap().sleep_reset.take().map(|(deadline, chapter_end)| (deadline, chapter_end.map(|end| self.session.global_offset.saturating_add(end))))
    }
    pub fn take_sleep_finished(&self) -> bool {
        std::mem::take(&mut self.session.status.lock().unwrap().sleep_finished)
    }
    pub fn take_error(&self) -> Option<String> {
        self.session.status.lock().unwrap().error.take()
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
            *self.session.stop_error.lock().unwrap() = Some(error);
            self.session.release.0.close();
        }
    }

    pub fn release_completion(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>> {
        self.release_completion_with_timeout(Duration::from_secs(30))
    }

    fn release_completion_with_timeout(&self, timeout: Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>> {
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
            session.stop_error.lock().unwrap().clone().map_or(Ok(()), Err)
        })
    }
}

/// Called on the Java service thread, including while the screen is off.
pub fn playback_state(event: &str) {
    let Ok(event) = serde_json::from_str::<Value>(event) else { return };
    let Some(id) = event["id"].as_u64() else { return };
    let session = sessions().lock().unwrap().get(&id).cloned();
    let Some(session) = session else { return };
    let mut status = session.status.lock().unwrap();
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
        status.ended = event["ended"].as_bool().unwrap_or(false);
        if let Some(control) = &session.control {
            let requested = event["requested"].as_bool().unwrap_or(playing || status.buffering);
            control.set_paused(!requested);
        }
        status.rate = event["speed"].as_f64().filter(|rate| rate.is_finite() && *rate > 0.0);
        if status.confirmed && position_save_required(status.last_saved, status.position, force) {
            let position = session.global_offset.saturating_add(status.position);
            let _ = session.updates.try_send((position, playback_progress(position, session.global_duration)));
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
        sessions().lock().unwrap().remove(&id);
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
            reader: Mutex::new(Some(Box::new(std::io::Cursor::new(Vec::<u8>::new())))),
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
    fn loader_acknowledges_latest_seek_generation_and_keeps_byte_offsets() {
        let (id, session, _) = session();
        *session.reader.lock().unwrap() = Some(Box::new(std::io::Cursor::new(vec![0, 1, 2, 3, 4, 5])));
        interrupt_source(id);
        interrupt_source(id);
        let control = session.control.as_ref().unwrap();
        assert!(control.is_interrupted());
        assert_eq!(read_source(id, 3, 2).unwrap(), vec![3, 4]);
        assert!(!control.is_interrupted());
        assert_eq!(session.read_generation.load(Ordering::Acquire), 2);
        event(id, 0, 0, false, false, true);
    }

    #[test]
    fn seek_during_read_cannot_deliver_old_generation_bytes() {
        struct InterruptedReader {
            id: u64,
            interrupted: bool,
            bytes: std::io::Cursor<Vec<u8>>,
        }
        impl std::io::Read for InterruptedReader {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    interrupt_source(self.id);
                }
                std::io::Read::read(&mut self.bytes, out)
            }
        }
        impl std::io::Seek for InterruptedReader {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                std::io::Seek::seek(&mut self.bytes, position)
            }
        }
        let (id, session, _) = session();
        *session.reader.lock().unwrap() = Some(Box::new(InterruptedReader { id, interrupted: false, bytes: std::io::Cursor::new(vec![0, 1, 2, 3]) }));
        assert_eq!(read_source(id, 0, 2), Err("audio read superseded".into()));
        assert_eq!(read_source(id, 2, 2).unwrap(), vec![2, 3]);
        event(id, 0, 0, false, false, true);
    }

    #[test]
    fn service_intent_gates_source_retries_without_confusing_loading_with_pause() {
        let (id, session, _) = session();
        let control = session.control.as_ref().unwrap().clone();
        playback_state(&json!({"id": id, "requested": true, "playing": false, "ready": false}).to_string());
        assert!(!control.is_paused(), "loading with play requested must permit retry");
        playback_state(&json!({"id": id, "requested": false, "playing": false, "ready": false}).to_string());
        assert!(control.is_paused());
        session.status.lock().unwrap().sequence = 1;
        playback_state(&json!({"id": id, "sequence": 0, "requested": true}).to_string());
        assert!(control.is_paused(), "stale service state must not resume retries");
        event(id, 1, 0, false, false, true);
        assert!(control.is_cancelled());
    }

    #[test]
    fn stop_cancels_source_even_before_service_start() {
        let (id, session, _) = session();
        let control = session.control.as_ref().unwrap().clone();
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(false) };
        engine.stop();
        assert!(control.is_cancelled());
        sessions().lock().unwrap().remove(&id);
    }

    #[test]
    fn buffering_is_distinct_from_pause_and_never_advances_position() {
        let (id, session, _) = session();
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(true) };
        event(id, 0, 40_000, true, true, false);
        playback_state(
            &json!({"id": id, "sequence": 0, "position": 45_000,
            "ready": false, "playing": false, "buffering": true})
            .to_string(),
        );
        assert!(engine.is_buffering());
        assert!(!engine.is_playing());
        assert_eq!(engine.position(), Duration::from_secs(40));
        // A pause while loading clears buffering; old sequence events cannot
        // overwrite the state of a newer seek.
        event(id, 0, 45_000, false, false, false);
        assert!(!engine.is_buffering());
        engine.session.status.lock().unwrap().sequence = 1;
        playback_state(&json!({"id": id, "sequence": 0, "buffering": true}).to_string());
        assert!(!engine.is_buffering());
        playback_state(&json!({"id": id, "sequence": 1, "buffering": true, "released": true}).to_string());
        assert!(!engine.is_buffering());
    }

    #[test]
    fn final_position_snapshot_survives_source_release() {
        let (id, session, updates) = session();
        let source = Arc::downgrade(&session);
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(true) };
        let snapshot = engine.position_snapshot();
        assert_eq!(snapshot.confirmed_position(), None);
        event(id, 0, 40_000, true, true, false);
        assert_eq!(snapshot.confirmed_position(), Some(Duration::from_secs(40)));
        drop(engine);
        event(id, 0, 42_000, false, true, true);
        assert_eq!(snapshot.confirmed_position(), Some(Duration::from_secs(42)));
        assert!(source.upgrade().is_none(), "snapshot must not retain the playback source");
        assert_eq!(updates.try_recv().unwrap().0, Duration::from_secs(40));
        assert_eq!(updates.try_recv().unwrap().0, Duration::from_secs(42));
    }

    #[test]
    fn unready_service_event_does_not_reset_resume_position() {
        let (id, session, updates) = session();
        session.status.lock().unwrap().position = Duration::from_secs(120);
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(true) };
        event(id, 0, 0, false, false, false);
        assert_eq!(engine.position(), Duration::from_secs(120));
        assert_eq!(engine.confirmed_position(), None);
        assert!(updates.is_empty());
        event(id, 0, 120_000, false, true, true);
        assert_eq!(engine.confirmed_position(), Some(Duration::from_secs(120)));
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
    fn release_completion_follows_the_final_progress_event() {
        let (id, session, updates) = session();
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(true) };
        let mut released = engine.release_completion();
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(released.as_mut().poll(&mut poll).is_pending());
        event(id, 0, 42_000, false, true, true);
        assert!(matches!(released.as_mut().poll(&mut poll), std::task::Poll::Ready(Ok(()))));
        assert_eq!(updates.try_recv().unwrap().0, Duration::from_secs(42));
    }

    #[test]
    fn release_completion_reports_stop_dispatch_failure() {
        let (id, session, _) = session();
        let engine = AudioEngine { id, session: session.clone(), load: Value::Null, started: AtomicBool::new(true) };
        let mut released = engine.release_completion();
        *session.stop_error.lock().unwrap() = Some("service unavailable".to_owned());
        session.release.0.close();
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(matches!(released.as_mut().poll(&mut poll), std::task::Poll::Ready(Err(error)) if error == "service unavailable"));
        sessions().lock().unwrap().remove(&id);
    }

    #[test]
    fn missing_service_acknowledgement_fails_instead_of_allowing_removal() {
        let (id, session, _) = session();
        let engine = AudioEngine { id, session, load: Value::Null, started: AtomicBool::new(true) };
        let mut released = engine.release_completion_with_timeout(Duration::ZERO);
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
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
            cursor: std::io::Cursor<&'static [u8]>,
            reads: Arc<AtomicU64>,
        }
        impl std::io::Read for TrackedReader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.reads.fetch_add(1, Ordering::Relaxed);
                std::io::Read::read(&mut self.cursor, bytes)
            }
        }
        impl std::io::Seek for TrackedReader {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                std::io::Seek::seek(&mut self.cursor, position)
            }
        }
        let reads = Arc::new(AtomicU64::new(0));
        let reader = TrackedReader { cursor: std::io::Cursor::new(b"seekable M4B bytes"), reads: reads.clone() };
        let mut audio = AudioEngine::open(Box::new(reader), None, None, &book, None, Duration::from_secs(90), updates).unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 0, "opening must not copy the stream");
        assert!(audio.load["path"].is_null());
        assert_eq!(audio.load["length"], 18);
        assert!(commands.lock().unwrap().is_empty(), "Loading must not start abandoned reader requests");
        audio.set_tempo(1.5).unwrap();
        audio.stop();
        assert!(commands.lock().unwrap().is_empty(), "Preparing speed and stopping an unstarted reader must not contact a missing service");
        audio.play();
        assert_eq!(read_source(audio.id, 9, 3).unwrap(), b"M4B");
        assert_eq!(read_source(audio.id, 0, 8).unwrap(), b"seekable");
        assert!(read_source(audio.id, 18, 8).unwrap().is_empty());
        assert!(read_source(audio.id, 0, 256 * 1024 + 1).is_err());
        let command = commands.lock().unwrap()[0].clone();
        assert_eq!(command["speed"], 1.5);
        assert_eq!(command["position"], 60_000);
        assert_eq!(command["title"], "Example");
        event(audio.id, 0, 60_000, false, true, true);
        let id = audio.id;
        drop(audio);
        assert!(read_source(id, 0, 1).is_err());
        #[cfg(target_os = "linux")]
        {
            let original = directory.path().join("original.m4b");
            std::fs::write(&original, b"original audiobook").unwrap();
            let file = std::fs::File::open(&original).unwrap();
            let (updates, _) = async_channel::unbounded();
            // An empty reader proves playback did not copy its bytes.
            let audio = AudioEngine::open(Box::new(std::io::Cursor::new(Vec::<u8>::new())), Some(file), None, &book, None, Duration::ZERO, updates).unwrap();
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
    fn background_session_keeps_source_and_saves_without_ui() {
        let (id, session, receiver) = session();
        let retained = Arc::downgrade(&session);
        drop(session);
        event(id, 0, 12_000, true, true, false);
        assert_eq!(receiver.try_recv().unwrap().0, Duration::from_secs(12));
        assert!(retained.upgrade().is_some());
        event(id, 0, 13_000, false, true, true);
        assert_eq!(receiver.try_recv().unwrap().0, Duration::from_secs(13));
        assert!(retained.upgrade().is_none());
    }
    #[test]
    fn unprepared_and_stale_seek_events_cannot_overwrite_resume_position() {
        let (id, session, receiver) = session();
        event(id, 0, 0, false, false, false);
        assert!(receiver.try_recv().is_err());
        session.status.lock().unwrap().sequence = 2;
        event(id, 1, 20_000, true, true, false);
        assert!(receiver.try_recv().is_err());
        event(id, 2, 120_000, true, true, false);
        assert_eq!(receiver.try_recv().unwrap().0, Duration::from_secs(120));
        event(id, 2, 120_000, false, true, true);
    }
    #[test]
    fn pause_saves_short_progress_and_periodic_updates_are_throttled() {
        let (id, _, receiver) = session();
        event(id, 0, 10_000, true, true, false);
        receiver.try_recv().unwrap();
        event(id, 0, 11_000, true, true, false);
        assert!(receiver.try_recv().is_err());
        event(id, 0, 11_000, false, true, false);
        assert_eq!(receiver.try_recv().unwrap().0, Duration::from_secs(11));
        event(id, 0, 11_000, false, true, true);
    }
    #[test]
    fn small_seek_from_system_controls_is_saved_while_paused() {
        let (id, _, receiver) = session();
        event(id, 0, 10_000, false, true, false);
        receiver.try_recv().unwrap();
        playback_state(&json!({"id":id,"position":11_000,"ready":true,"force":true}).to_string());
        assert_eq!(receiver.try_recv().unwrap().0, Duration::from_secs(11));
        event(id, 0, 11_000, false, true, true);
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
