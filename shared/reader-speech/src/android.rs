//! Android notification controls act on the speech worker directly, then notify
//! the reader so its dock and highlighting follow the same playback state.
use super::{Command, Shared, SpeechControl, SpeechEvent, SpeechEvents, recover_lock};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex, OnceLock, Weak,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};

type Bridge = dyn Fn(&str) -> Result<(), String> + Send + Sync;
static BRIDGE: OnceLock<Box<Bridge>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SESSIONS: OnceLock<Mutex<HashMap<u64, Arc<Session>>>> = OnceLock::new();

struct Session {
    shared: Weak<Shared>,
    commands: mpsc::Sender<Command>,
    events: SpeechEvents,
    owner: u64,
    title: Mutex<String>,
    metadata: Mutex<MediaMetadata>,
    active: AtomicBool,
    last_state: Mutex<Option<NotificationState>>,
}

#[derive(Clone, Default, PartialEq)]
pub(super) struct MediaMetadata {
    pub author: String,
    pub chapter: String,
    pub cover: Vec<u8>,
}

#[derive(PartialEq)]
struct NotificationState {
    title: String,
    metadata: MediaMetadata,
    paused: bool,
    preparing: bool,
    rate: u32,
}

impl Session {
    fn state(&self) -> Option<NotificationState> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let shared = self.shared.upgrade()?;
        let preparing = shared.lock(&shared.player).as_ref().is_none_or(|player| player.empty());
        Some(NotificationState { title: recover_lock(&self.title).clone(), metadata: recover_lock(&self.metadata).clone(), paused: shared.paused.load(Ordering::Acquire), preparing, rate: shared.rate.load(Ordering::Acquire) })
    }
}

fn sessions() -> &'static Mutex<HashMap<u64, Arc<Session>>> {
    SESSIONS.get_or_init(Default::default)
}

fn session(id: u64) -> Option<Arc<Session>> {
    recover_lock(sessions()).get(&id).cloned()
}

pub fn initialize(dispatch: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) {
    let _ = BRIDGE.set(Box::new(dispatch));
}

pub(super) fn register(shared: &Arc<Shared>, commands: mpsc::Sender<Command>, events: SpeechEvents, owner: u64) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    recover_lock(sessions()).insert(
        id,
        Arc::new(Session { shared: Arc::downgrade(shared), commands, events, owner, title: Mutex::new("Read aloud".into()), metadata: Mutex::new(MediaMetadata::default()), active: AtomicBool::new(false), last_state: Mutex::new(None) }),
    );
    id
}

pub(super) fn set_title(id: u64, title: &str) {
    if let Some(session) = session(id) {
        *recover_lock(&session.title) = title.into();
        changed(id);
    }
}

pub(super) fn set_metadata(id: u64, metadata: MediaMetadata) {
    if let Some(session) = session(id) {
        *recover_lock(&session.metadata) = metadata;
        changed(id);
    }
}

pub(super) fn start(id: u64) -> Result<(), String> {
    let session = session(id).ok_or("Speech session has ended")?;
    if session.active.swap(true, Ordering::AcqRel) {
        changed(id);
        return Ok(());
    }
    *recover_lock(&session.last_state) = None;
    let result = match BRIDGE.get() {
        Some(dispatch) => dispatch(&json!({"command":"start", "id":id}).to_string()),
        None => Err("Android speech service is unavailable".into()),
    };
    if result.is_err() {
        session.active.store(false, Ordering::Release);
    }
    result
}

pub(super) fn stop(id: u64) {
    if let Some(session) = session(id) {
        session.active.store(false, Ordering::Release);
        if let Some(dispatch) = BRIDGE.get() {
            let _ = dispatch(&json!({"command":"stop", "id":id}).to_string());
        }
    }
}

pub(super) fn remove(id: u64) {
    recover_lock(sessions()).remove(&id);
}

pub fn failed(id: u64, generation: u64, message: String) {
    if let Some(session) = session(id) {
        let _ = session.events.try_send(SpeechEvent::Failed { generation, message });
    }
}

/// Read once when the service starts or receives a state-change signal.
pub fn snapshot(id: u64) -> Option<String> {
    let session = session(id)?;
    let state = session.state()?;
    Some(
        json!({"id":id, "title":state.title, "author":state.metadata.author, "chapter":state.metadata.chapter, "cover":state.metadata.cover, "paused":state.paused, "preparing":state.preparing,
        "rate":f32::from_bits(state.rate)})
        .to_string(),
    )
}

fn changed(id: u64) {
    if let Some(dispatch) = BRIDGE.get() {
        changed_with(id, dispatch.as_ref());
    }
}

pub(super) fn changed_for(shared: &Shared) {
    let id = recover_lock(sessions()).iter().find(|(_, session)| session.active.load(Ordering::Acquire) && std::ptr::eq(session.shared.as_ptr(), shared)).map(|(&id, _)| id);
    if let Some(id) = id {
        changed(id);
    }
}

fn changed_with(id: u64, dispatch: &Bridge) {
    let Some(session) = session(id) else { return };
    let Some(state) = session.state() else { return };
    let mut last = recover_lock(&session.last_state);
    if last.as_ref() == Some(&state) {
        return;
    }
    *last = Some(state);
    drop(last);
    if dispatch(&json!({"command":"update", "id":id}).to_string()).is_err() {
        *recover_lock(&session.last_state) = None;
    }
}

/// Pause/stop take effect immediately even when the reader is in the background.
/// Item changes carry both generations so stale PCM cannot update the reader.
pub fn control(id: u64, action: &str) -> bool {
    let Some(session) = session(id) else { return false };
    if !session.active.load(Ordering::Acquire) {
        return false;
    }
    let Some(shared) = session.shared.upgrade() else { return false };
    let owner = shared.lock(&shared.owner);
    if *owner != session.owner {
        return false;
    }
    let generation = shared.generation.load(Ordering::Acquire);
    let command = match action {
        "pause" => {
            shared.paused.store(true, Ordering::Release);
            if let Some(player) = shared.lock(&shared.player).as_ref() {
                player.pause();
            }
            shared.update_output(&session.commands);
            SpeechControl::Pause
        }
        "resume" => {
            shared.paused.store(false, Ordering::Release);
            let _ = session.commands.send(Command::Wake);
            if let Some(player) = shared.lock(&shared.player).as_ref() {
                player.play();
            }
            shared.update_output(&session.commands);
            SpeechControl::Resume
        }
        "stop" => {
            shared.paused.store(true, Ordering::Release);
            let stopped = shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
            shared.count.store(0, Ordering::Release);
            if let Some(player) = shared.lock(&shared.player).as_ref() {
                player.stop();
                player.pause();
            }
            let _ = session.commands.send(Command::Stop { generation: stopped });
            session.active.store(false, Ordering::Release);
            SpeechControl::Stop
        }
        "back" | "forward" => {
            let current = shared.cursor.load(Ordering::Acquire);
            let count = shared.count.load(Ordering::Acquire).min(shared.end.load(Ordering::Acquire));
            if current >= count {
                return true;
            }
            let index = if action == "back" { current.checked_sub(1) } else { current.checked_add(1).filter(|index| *index < count) };
            let Some(index) = index else { return true };
            let next_generation = shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
            shared.cursor.store(index, Ordering::Release);
            if let Some(player) = shared.lock(&shared.player).as_ref() {
                player.stop();
                player.pause();
            }
            // Publish the item/generation before the worker can emit its next chunk.
            let _ = session.events.try_send(SpeechEvent::Remote { generation, command: SpeechControl::Item { generation: next_generation, index } });
            changed(id);
            return session.commands.send(Command::Skip { generation: next_generation, index }).is_ok();
        }
        _ => return false,
    };
    changed(id);
    session.events.try_send(SpeechEvent::Remote { generation, command }).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pauses, Voice};
    use std::sync::atomic::{AtomicU32, AtomicUsize};

    fn fixture() -> (u64, Arc<Shared>, mpsc::Receiver<Command>, async_channel::Receiver<SpeechEvent>) {
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(7),
            paused: AtomicBool::new(false),
            cursor: AtomicUsize::new(3),
            count: AtomicUsize::new(6),
            rate: AtomicU32::new(1.0_f32.to_bits()),
            end: AtomicUsize::new(usize::MAX),
            pauses: Mutex::new(Pauses::NATURAL),
            voice: Mutex::new(Voice::Alba),
            player: Mutex::new(None),
            output: Mutex::new(None),
            owner: Mutex::new(1),
            route: Mutex::new(None),
        });
        let (commands, receiver) = mpsc::channel();
        let (events, received) = async_channel::unbounded();
        let id = register(&shared, commands, events.into(), 1);
        session(id).unwrap().active.store(true, Ordering::Release);
        (id, shared, receiver, received)
    }

    #[test]
    fn background_controls_change_audio_before_ui_consumes_events() {
        let (id, shared, _, events) = fixture();
        assert!(control(id, "pause"));
        assert!(shared.paused.load(Ordering::Acquire));
        assert!(control(id, "resume"));
        assert!(!shared.paused.load(Ordering::Acquire));
        assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Remote { generation: 7, command: SpeechControl::Pause }));
        assert!(control(id, "stop"));
        assert!(snapshot(id).is_none());
        assert!(!control(id, "resume"));
        remove(id);
    }

    #[test]
    fn controls_from_a_previous_reader_cannot_change_the_active_readers_audio() {
        let (id, shared, commands, events) = fixture();
        *shared.owner.lock().unwrap() = 2;
        for action in ["pause", "resume", "stop", "back", "forward"] {
            assert!(!control(id, action));
        }
        assert!(!shared.paused.load(Ordering::Acquire));
        assert_eq!(shared.generation.load(Ordering::Acquire), 7);
        assert_eq!(shared.cursor.load(Ordering::Acquire), 3);
        assert_eq!(shared.count.load(Ordering::Acquire), 6);
        assert!(commands.try_recv().is_err());
        assert!(events.try_recv().is_err());
        remove(id);
    }

    #[test]
    fn item_skip_invalidates_audio_and_updates_ui_generation_before_new_audio() {
        let (id, shared, commands, events) = fixture();
        assert!(control(id, "forward"));
        assert_eq!(shared.generation.load(Ordering::Acquire), 8);
        assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Remote { generation: 7, command: SpeechControl::Item { generation: 8, index: 4 } }));
        assert!(matches!(commands.try_recv().unwrap(), Command::Skip { generation: 8, index: 4 }));
        remove(id);
        assert!(!control(id, "pause"));
    }

    #[test]
    fn item_navigation_preserves_pause_and_stops_at_boundaries() {
        let (id, shared, commands, events) = fixture();
        shared.paused.store(true, Ordering::Release);
        assert!(control(id, "back"));
        assert!(shared.paused.load(Ordering::Acquire));
        assert_eq!(shared.cursor.load(Ordering::Acquire), 2);
        assert!(matches!(commands.try_recv().unwrap(), Command::Skip { index: 2, .. }));
        assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Remote { command: SpeechControl::Item { index: 2, .. }, .. }));
        shared.cursor.store(0, Ordering::Release);
        let generation = shared.generation.load(Ordering::Acquire);
        assert!(control(id, "back"));
        assert_eq!(shared.generation.load(Ordering::Acquire), generation);
        assert!(commands.try_recv().is_err());
        shared.cursor.store(5, Ordering::Release);
        assert!(control(id, "forward"));
        assert_eq!(shared.generation.load(Ordering::Acquire), generation);
        assert!(commands.try_recv().is_err());
        remove(id);
    }

    #[test]
    fn notification_stop_retires_output_without_waiting_for_the_reader() {
        let (id, shared, commands, events) = fixture();
        assert!(control(id, "stop"));
        assert!(matches!(commands.try_recv().unwrap(), Command::Stop { generation: 8 }));
        assert_eq!(shared.count.load(Ordering::Acquire), 0);
        assert!(shared.paused.load(Ordering::Acquire));
        assert!(snapshot(id).is_none());
        assert!(matches!(events.try_recv().unwrap(), SpeechEvent::Remote { generation: 7, command: SpeechControl::Stop }));
        remove(id);
    }

    #[test]
    fn notification_metadata_survives_pause_and_is_removed_when_stopped() {
        let (id, shared, _commands, _events) = fixture();
        set_metadata(id, MediaMetadata { author: "Author".into(), chapter: "Chapter 2 · Garden".into(), cover: vec![1, 2, 3] });
        shared.paused.store(true, Ordering::Release);
        let state: serde_json::Value = serde_json::from_str(&snapshot(id).unwrap()).unwrap();
        assert_eq!(state["author"], "Author");
        assert_eq!(state["chapter"], "Chapter 2 · Garden");
        assert_eq!(state["cover"], json!([1, 2, 3]));
        assert_eq!(state["paused"], true);
        assert!(control(id, "stop"));
        assert!(snapshot(id).is_none());
        remove(id);
    }

    #[test]
    fn notification_signals_only_when_its_presented_state_changes() {
        let (id, shared, _, _) = fixture();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let messages = sent.clone();
        let dispatch = move |command: &str| {
            messages.lock().unwrap().push(command.to_owned());
            Ok(())
        };
        changed_with(id, &dispatch);
        changed_with(id, &dispatch);
        assert_eq!(sent.lock().unwrap().len(), 1);
        shared.paused.store(true, Ordering::Release);
        changed_with(id, &dispatch);
        assert_eq!(sent.lock().unwrap().len(), 2);
        *session(id).unwrap().title.lock().unwrap() = "Another title".into();
        changed_with(id, &dispatch);
        assert_eq!(sent.lock().unwrap().len(), 3);
        shared.rate.store(1.5f32.to_bits(), Ordering::Release);
        changed_with(id, &dispatch);
        assert_eq!(sent.lock().unwrap().len(), 4);
        session(id).unwrap().active.store(false, Ordering::Release);
        changed_with(id, &dispatch);
        assert_eq!(sent.lock().unwrap().len(), 4);
        remove(id);
    }
}
