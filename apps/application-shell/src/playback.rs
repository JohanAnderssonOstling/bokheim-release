//! One playback owner and one ordered restoration writer per application.
use app::AppClient;
use futures_util::{
    FutureExt,
    future::{LocalBoxFuture, Shared},
};
use gpui::{App, AppContext, Context, Entity, Global, Subscription};
use library_model::BookLocator;
use reader_ui::ActiveAudiobook;
use std::path::PathBuf;
use std::time::Duration;
use web_time::Instant;

type PositionFlush = Shared<LocalBoxFuture<'static, Result<(), String>>>;

#[cfg(target_arch = "wasm32")]
const WEB_RESTORE_KEY: &str = "bokheim.active-audiobook.v1";

/// Playback restoration is UI state. Native shells provide a private file and
/// browser shells use their own local storage; neither involves the backend.
#[derive(Clone)]
struct RestoreStore {
    #[cfg(not(target_arch = "wasm32"))]
    path: Option<PathBuf>,
}

impl RestoreStore {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            path,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load(&self) -> Result<Option<ActiveAudiobook>, String> {
        let Some(path) = &self.path else { return Ok(None) };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let record = serde_json::from_slice::<Option<ActiveAudiobook>>(&bytes).map_err(|error| error.to_string())?;
        if let Some(record) = &record {
            record.validate()?;
        }
        Ok(record)
    }

    #[cfg(target_arch = "wasm32")]
    fn load(&self) -> Result<Option<ActiveAudiobook>, String> {
        let storage = web_sys::window()
            .ok_or_else(|| "browser window is unavailable".to_owned())?
            .local_storage()
            .map_err(|error| format!("could not access browser storage: {error:?}"))?
            .ok_or_else(|| "browser local storage is unavailable".to_owned())?;
        let Some(value) = storage.get_item(WEB_RESTORE_KEY).map_err(|error| format!("could not read browser storage: {error:?}"))? else {
            return Ok(None);
        };
        let record = serde_json::from_str::<ActiveAudiobook>(&value).map_err(|error| error.to_string())?;
        record.validate()?;
        Ok(Some(record))
    }

    fn save(&self, expected: Option<uuid::Uuid>, record: Option<ActiveAudiobook>) -> Result<bool, String> {
        if let Some(record) = &record {
            record.validate()?;
        }
        if self.load()?.as_ref().map(|record| record.session_id) != expected {
            return Ok(false);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(path, serde_json::to_vec(&record).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let storage = web_sys::window()
                .ok_or_else(|| "browser window is unavailable".to_owned())?
                .local_storage()
                .map_err(|error| format!("could not access browser storage: {error:?}"))?
                .ok_or_else(|| "browser local storage is unavailable".to_owned())?;
            match record {
                Some(record) => storage.set_item(WEB_RESTORE_KEY, &serde_json::to_string(&record).map_err(|error| error.to_string())?).map_err(|error| format!("could not write browser storage: {error:?}"))?,
                None => storage.remove_item(WEB_RESTORE_KEY).map_err(|error| format!("could not clear browser storage: {error:?}"))?,
            }
        }
        Ok(true)
    }
}

struct SharedPlayback(Entity<PlaybackController>);
impl Global for SharedPlayback {}

pub(super) struct ActivePlayback {
    pub record: ActiveAudiobook,
    pub session: Entity<reader_ui::PlaybackSession>,
    persisted: bool,
    last_save: Instant,
    playing: bool,
}

enum Save {
    Replace(Option<ActiveAudiobook>),
    Update(ActiveAudiobook),
    Flush(async_channel::Sender<Result<(), String>>),
}

/// User selection advances even when a disk write fails. CAS expectations only
/// advance after success; otherwise later updates could never retry a new session.
struct SaveIdentity<T> {
    stored: Option<T>,
    selected: Option<T>,
}
impl<T: Copy + Eq> SaveIdentity<T> {
    fn new(stored: Option<T>) -> Self {
        Self { stored, selected: stored }
    }
    fn select(&mut self, selected: Option<T>) {
        self.selected = selected;
    }
    fn accepts_update(&self, id: T) -> bool {
        self.selected == Some(id)
    }
    fn committed(&mut self, stored: Option<T>) {
        self.stored = stored;
    }
}

#[derive(Default)]
struct OpenOrder {
    next: u64,
    accepted: u64,
    removed_through: std::collections::HashMap<sync_common::LibraryId, u64>,
}
impl OpenOrder {
    fn reserve(&mut self) -> u64 {
        self.next = self.next.checked_add(1).expect("playback request sequence exhausted");
        self.next
    }
    fn accept(&mut self, request: u64) -> bool {
        if request <= self.accepted || request > self.next {
            return false;
        }
        self.accepted = request;
        true
    }
    fn cancel_pending(&mut self) {
        self.accepted = self.next;
    }
    fn cancel_library(&mut self, library: sync_common::LibraryId) {
        self.removed_through.insert(library, self.next);
    }
    fn cancelled_for(&self, request: u64, library: sync_common::LibraryId) -> bool {
        self.removed_through.get(&library).is_some_and(|through| request <= *through)
    }
}

pub(super) struct PlaybackController {
    pub current: Option<ActivePlayback>,
    pub generation: u64,
    opens: OpenOrder,
    opening: Option<BookLocator>,
    user_changed: bool,
    quitting: bool,
    pending_positions: Vec<(sync_common::LibraryId, PositionFlush)>,
    saves: async_channel::Sender<Save>,
    observation: Option<Subscription>,
}

impl PlaybackController {
    pub fn shared(backend: AppClient, restore_path: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        let controller = Self::shared_with(cx, |cx| Self::new(backend, RestoreStore::new(restore_path), cx));
        let target = controller.downgrade();
        cx.set_global(browser_ui::LibraryRemovalHook(std::rc::Rc::new(move |library, cx| {
            target.update(cx, |controller, cx| controller.prepare_removal(library, cx)).unwrap_or_else(|_| Box::pin(async { Err("Playback controller is unavailable".into()) }))
        })));
        controller
    }

    fn shared_with(cx: &mut App, create: impl FnOnce(&mut Context<Self>) -> Self) -> Entity<Self> {
        if let Some(shared) = cx.try_global::<SharedPlayback>() {
            return shared.0.clone();
        }
        let controller = cx.new(create);
        cx.set_global(SharedPlayback(controller.clone()));
        controller
    }

    fn new(backend: AppClient, restore_store: RestoreStore, cx: &mut Context<Self>) -> Self {
        // Android release has a 30s bound; allow additional time for the final
        // progress and restoration writes. Other GPUI applications keep 200ms.
        #[cfg(not(target_arch = "wasm32"))]
        cx.extend_shutdown_timeout(Duration::from_secs(45));
        let (saves, incoming) = async_channel::unbounded();
        let library_backend = backend.clone();
        cx.spawn(async move |this, cx| {
            let Ok(changes) = library_backend.library_list_changes().await else {
                return;
            };
            loop {
                let expected = this.update(cx, |controller, _| controller.current.as_ref().map(|active| (active.record.session_id, *active.record.locator.library_id()))).ok().flatten();
                let libraries = library_backend.libraries().await;
                if let (Some((session_id, library_id)), Ok(libraries)) = (expected, libraries) {
                    if !libraries.iter().any(|entry| entry.library_id() == &library_id) {
                        let _ = this.update(cx, |controller, cx| {
                            if controller.current.as_ref().is_some_and(|active| active.record.session_id == session_id) {
                                controller.close(cx);
                            }
                        });
                    }
                }
                if changes.recv().await.is_err() {
                    break;
                }
            }
        })
        .detach();
        let restore_backend = backend.clone();
        let (restored_tx, restored_rx) = async_channel::bounded(1);
        let writer = async move {
            let restored = match restore_store.load() {
                Ok(record) => record,
                Err(error) => {
                    log::warn!("could not restore audiobook: {error}");
                    None
                }
            };
            let mut stored_id = restored.as_ref().map(|record| record.session_id);
            if let Some(record) = restored {
                let missing = backend.libraries().await.ok().is_some_and(|libraries| !libraries.iter().any(|entry| entry.library_id() == record.locator.library_id()));
                if missing {
                    if matches!(restore_store.save(stored_id, None), Ok(true)) {
                        stored_id = None;
                    }
                } else {
                    let _ = restored_tx.try_send(record);
                }
            }
            drop(restored_tx);
            let mut write_error = None;
            let mut identity = SaveIdentity::new(stored_id);
            while let Ok(command) = incoming.recv().await {
                let record = match command {
                    Save::Replace(record) => {
                        identity.select(record.as_ref().map(|record| record.session_id));
                        record
                    }
                    Save::Update(record) if identity.accepts_update(record.session_id) => Some(record),
                    Save::Update(_) => continue,
                    Save::Flush(done) => {
                        let _ = done.try_send(write_error.clone().map_or(Ok(()), Err));
                        continue;
                    }
                };
                let next_id = record.as_ref().map(|record| record.session_id);
                match restore_store.save(identity.stored, record) {
                    Ok(true) => {
                        identity.committed(next_id);
                        write_error = None;
                    }
                    Ok(false) => {
                        write_error = Some("Audiobook restore record changed outside this playback controller".to_owned());
                        log::warn!("{}", write_error.as_ref().unwrap());
                    }
                    Err(error) => {
                        log::warn!("could not save active audiobook: {error}");
                        write_error = Some(error);
                    }
                }
            }
        };
        // Native quit blocks after the platform event loop stops. The ordered
        // writer must remain runnable without dispatching foreground tasks.
        #[cfg(not(target_arch = "wasm32"))]
        cx.background_executor().spawn(writer).detach();
        #[cfg(target_arch = "wasm32")]
        cx.spawn(async move |_, _| writer.await).detach();
        cx.spawn(async move |this, cx| {
            if let Ok(record) = restored_rx.recv().await {
                let library = restore_backend.library(*record.locator.library_id());
                let _ = this.update(cx, |controller, cx| {
                    if controller.user_changed {
                        return;
                    }
                    let session = reader_ui::restore_audiobook(&record, library, cx);
                    controller.install(record, session, true, cx);
                });
            }
        })
        .detach();
        cx.on_app_quit(|controller, cx| {
            let position_flush = controller.begin_shutdown(cx);
            let snapshot = controller.current.as_ref().map(|active| active.session.read(cx).final_restoration_snapshot(active.record.clone()));
            let saves = controller.saves.clone();
            // Return the future itself: spawning it on the stopped UI executor
            // deadlocks shutdown. Capture no entity/App access across awaits.
            async move {
                if let Some(flush) = position_flush {
                    if let Err(error) = flush.await {
                        log::warn!("could not save final audiobook position: {error}");
                    }
                }
                if let Some(snapshot) = snapshot {
                    let _ = saves.try_send(Save::Replace(Some(snapshot())));
                }
                let (done, finished) = async_channel::bounded(1);
                if saves.try_send(Save::Flush(done)).is_ok() {
                    if let Ok(Err(error)) = finished.recv().await {
                        log::warn!("could not flush active audiobook state: {error}");
                    }
                }
            }
        })
        .detach();
        Self { current: None, generation: 0, opens: OpenOrder::default(), opening: None, user_changed: false, quitting: false, pending_positions: vec![], saves, observation: None }
    }

    fn begin_shutdown(&mut self, cx: &mut Context<Self>) -> Option<browser_ui::LibraryRemovalPreparation> {
        if self.quitting {
            return None;
        }
        self.quitting = true;
        self.user_changed = true;
        self.opens.cancel_pending();
        self.opening = None;
        self.generation = self.generation.checked_add(1).expect("playback generation exhausted");
        // No intermediate observation may overwrite the final service snapshot
        // while shutdown is waiting for the position producer to finish.
        self.observation = None;
        let pending = std::mem::take(&mut self.pending_positions);
        let current = self.current.as_ref().map(|active| active.session.update(cx, |session, _| session.stop_and_flush()));
        if pending.is_empty() && current.is_none() {
            return None;
        }
        Some(Box::pin(async move {
            let mut result = Ok(());
            // Removal may already have taken the session's producer task. Join
            // its shared completion rather than treating a second stop as done.
            for (_, flush) in pending {
                if let Err(error) = flush.await {
                    result = Err(error);
                }
            }
            if let Some(flush) = current {
                if let Err(error) = flush.await {
                    result = Err(error);
                }
            }
            result
        }))
    }

    pub fn reserve_open(&mut self) -> u64 {
        self.opens.reserve()
    }

    pub fn prepare_open(&mut self, request: u64, locator: &BookLocator, cx: &mut Context<Self>) -> bool {
        if self.quitting || browser_ui::library_is_being_removed(*locator.library_id(), cx) || self.opens.cancelled_for(request, *locator.library_id()) || !self.opens.accept(request) {
            return false;
        }
        self.stop_current(cx);
        self.opening = Some(locator.clone());
        true
    }

    pub fn activate_current(&mut self, request: u64, locator: &BookLocator, target: Option<&str>, cx: &mut Context<Self>) -> bool {
        if self.quitting || browser_ui::library_is_being_removed(*locator.library_id(), cx) || self.opens.cancelled_for(request, *locator.library_id()) {
            return false;
        }
        let Some(active) = self.current.as_ref().filter(|active| &active.record.locator == locator) else {
            return false;
        };
        if !self.opens.accept(request) {
            return false;
        }
        self.generation = self.generation.checked_add(1).expect("playback generation exhausted");
        self.user_changed = true;
        self.opening = None;
        active.session.update(cx, |session, cx| {
            session.activate(target);
            session.ensure_source(cx);
            cx.notify();
        });
        true
    }

    pub fn replace(&mut self, locator: BookLocator, title: String, session: Entity<reader_ui::PlaybackSession>, cx: &mut Context<Self>) {
        if self.quitting || browser_ui::library_is_being_removed(*locator.library_id(), cx) {
            session.update(cx, |session, _| session.stop_playback());
            return;
        }
        self.stop_current(cx);
        let metadata = app::PlaybackMetadata { format: book_model::BookFormat::M4b, title, author: String::new(), narrator: None, duration_ms: 0, chapters: vec![], tracks: vec![], cover: vec![] };
        let record = ActiveAudiobook::new(locator, 0, 1.0, metadata);
        self.install(record, session, false, cx);
        self.save_current(true, cx);
    }

    fn install(&mut self, record: ActiveAudiobook, session: Entity<reader_ui::PlaybackSession>, persisted: bool, cx: &mut Context<Self>) {
        if self.quitting || browser_ui::library_is_being_removed(*record.locator.library_id(), cx) {
            session.update(cx, |session, _| session.stop_playback());
            return;
        }
        self.opening = None;
        self.observation = Some(cx.observe(&session, |controller, _, cx| controller.save_current(false, cx)));
        self.current = Some(ActivePlayback { record, session, persisted, last_save: Instant::now(), playing: false });
        cx.notify();
    }

    fn save_current(&mut self, force: bool, cx: &mut App) {
        let Some(active) = &mut self.current else {
            return;
        };
        let mut record = active.record.clone();
        let session = active.session.read(cx);
        if !session.update_restoration_record(&mut record) {
            return;
        }
        let playing = session.is_playing();
        let changed = record.position_ms != active.record.position_ms || record.speed != active.record.speed;
        let save = force
            || !active.persisted
            || playing != active.playing
            || record.speed != active.record.speed
            || record.metadata.cover != active.record.metadata.cover
            || record.position_ms.abs_diff(active.record.position_ms) > 5_000
            || (changed && active.last_save.elapsed() >= Duration::from_secs(5));
        if !save {
            return;
        }
        let command = if active.persisted { Save::Update(record.clone()) } else { Save::Replace(Some(record.clone())) };
        if self.saves.try_send(command).is_ok() {
            active.record = record;
            active.persisted = true;
            active.last_save = Instant::now();
            active.playing = playing;
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.opens.cancel_pending();
        self.stop_current(cx);
    }

    fn flush_saves(&self) -> browser_ui::LibraryRemovalPreparation {
        let (done, finished) = async_channel::bounded(1);
        let queued = self.saves.try_send(Save::Flush(done)).is_ok();
        Box::pin(async move {
            if !queued {
                return Err("Audiobook state writer stopped".into());
            }
            finished.recv().await.map_err(|_| "Audiobook state writer stopped".to_owned())?
        })
    }

    fn prepare_removal(&mut self, library: sync_common::LibraryId, cx: &mut Context<Self>) -> browser_ui::LibraryRemovalPreparation {
        if self.quitting {
            return Box::pin(async { Err("Application is shutting down".into()) });
        }
        // Format probes may not have installed an opening/session yet. Their
        // completions must remain invalid even after the removal guard drops.
        self.opens.cancel_library(library);
        let active = self.current.as_ref().filter(|active| active.record.locator.library_id() == &library);
        let opening = self.opening.as_ref().is_some_and(|locator| locator.library_id() == &library);
        let pending: Vec<_> = self.pending_positions.iter().filter(|(origin, _)| *origin == library).map(|(_, flush)| flush.clone()).collect();
        let pending = Box::pin(async move {
            for flush in pending {
                flush.await?;
            }
            Ok(())
        });
        if active.is_none() && !opening {
            return pending;
        }
        let Some(active) = active else {
            self.close(cx);
            let saves = self.flush_saves();
            return Box::pin(async move {
                pending.await?;
                saves.await
            });
        };
        let positions = active.session.update(cx, |session, cx| {
            let flush = session.stop_and_flush();
            cx.notify();
            flush
        });
        self.finish_active_removal(
            Box::pin(async move {
                let previous = pending.await;
                let current = positions.await;
                previous.and(current)
            }),
            cx,
        )
    }

    fn finish_active_removal(&mut self, positions: browser_ui::LibraryRemovalPreparation, cx: &mut Context<Self>) -> browser_ui::LibraryRemovalPreparation {
        let library = *self.current.as_ref().expect("active removal requires a session").record.locator.library_id();
        let positions = self.track_position_flush(library, positions, cx);
        self.opens.cancel_pending();
        self.opening = None;
        self.user_changed = true;
        self.observation = None;
        self.generation = self.generation.checked_add(1).expect("playback generation exhausted");
        let generation = self.generation;
        // Retain the stopped session until both saves succeed. In particular,
        // never queue a tombstone before the final position producer has flushed.
        Box::pin(cx.spawn(async move |this, cx| {
            let result = async {
                positions.await?;
                let saved = this
                    .update(cx, |controller, cx| {
                        controller.check_removal_generation(generation)?;
                        controller.save_current(true, cx);
                        Ok::<_, String>(controller.flush_saves())
                    })
                    .map_err(|error| error.to_string())??;
                saved.await?;
                let cleared = this
                    .update(cx, |controller, _| {
                        controller.check_removal_generation(generation)?;
                        controller.saves.try_send(Save::Replace(None)).map_err(|_| "Audiobook state writer stopped".to_owned())?;
                        // Shutdown can overtake this barrier. Its final snapshot
                        // must reselect the retained session after our tombstone.
                        if let Some(active) = &mut controller.current {
                            active.persisted = false;
                        }
                        Ok::<_, String>(controller.flush_saves())
                    })
                    .map_err(|error| error.to_string())??;
                cleared.await?;
                this.update(cx, |controller, cx| {
                    controller.check_removal_generation(generation)?;
                    controller.current = None;
                    controller.generation = controller.generation.checked_add(1).expect("playback generation exhausted");
                    cx.notify();
                    Ok::<_, String>(())
                })
                .map_err(|error| error.to_string())?
            }
            .await;
            if result.is_err() {
                let _ = this.update(cx, |controller, cx| controller.recover_failed_removal(generation, cx));
            }
            result
        }))
    }

    fn check_removal_generation(&self, generation: u64) -> Result<(), String> {
        if self.quitting || self.generation != generation { Err("Playback changed while preparing library removal; try again".into()) } else { Ok(()) }
    }

    fn recover_failed_removal(&mut self, generation: u64, cx: &mut Context<Self>) {
        if self.check_removal_generation(generation).is_err() {
            return;
        }
        let Some(active) = &mut self.current else {
            return;
        };
        active.session.read(cx).update_restoration_record(&mut active.record);
        active.session.update(cx, |session, cx| {
            session.recover_paused(&active.record, cx);
        });
        // A failed clear may have selected None in the ordered writer, even if
        // the disk write failed. Recovery must be a replacement, not an update.
        active.persisted = false;
        self.observation = Some(cx.observe(&active.session, |controller, _, cx| controller.save_current(false, cx)));
        self.save_current(true, cx);
        cx.notify();
    }

    fn track_position_flush(&mut self, library: sync_common::LibraryId, positions: browser_ui::LibraryRemovalPreparation, cx: &mut Context<Self>) -> PositionFlush {
        self.pending_positions.retain(|(_, flush)| flush.peek().is_none());
        let positions = positions.shared();
        self.pending_positions.push((library, positions.clone()));
        let completion = positions.clone();
        // Drain during ordinary navigation too. Shutdown retains a separate
        // waiter, so it never depends on this foreground task continuing to run.
        cx.spawn(async move |_, _| {
            let _ = completion.await;
        })
        .detach();
        positions
    }

    fn stop_current(&mut self, cx: &mut Context<Self>) {
        self.opening = None;
        self.generation = self.generation.checked_add(1).expect("playback generation exhausted");
        self.user_changed = true;
        self.save_current(true, cx);
        if let Some(active) = self.current.take() {
            let positions = active.session.update(cx, |session, cx| {
                let positions = session.stop_and_flush();
                cx.notify();
                positions
            });
            drop(self.track_position_flush(*active.record.locator.library_id(), positions, cx));
        }
        self.observation = None;
        let _ = self.saves.try_send(Save::Replace(None));
        cx.notify();
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn failed_initial_save_does_not_discard_later_updates_for_selected_session() {
        let mut identity = SaveIdentity::new(Some(1));
        identity.select(Some(2));
        // Failed replacement: disk still contains 1, but updates belong to 2.
        assert_eq!(identity.stored, Some(1));
        assert!(identity.accepts_update(2));
        assert!(!identity.accepts_update(1));
        identity.committed(Some(2));
        assert_eq!(identity.stored, Some(2));
        assert!(identity.accepts_update(2));
    }

    #[test]
    fn failed_close_does_not_allow_old_updates_to_resurrect_playback() {
        let mut identity = SaveIdentity::new(Some(1));
        identity.select(None);
        assert!(!identity.accepts_update(1));
        assert_eq!(identity.stored, Some(1));
        identity.select(Some(2));
        assert!(identity.accepts_update(2));
        assert_eq!(identity.stored, Some(1), "new save must still compare against actual disk identity");
        identity.committed(Some(2));
        identity.select(None);
        identity.committed(None);
        assert!(!identity.accepts_update(2));
        assert_eq!(identity.stored, None);
    }

    fn stopped_controller(cx: &mut gpui::TestAppContext) -> (Entity<PlaybackController>, async_channel::Receiver<Save>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let launch = app::host::BackendLaunch::initialize(app::AppDataLocation::native_path(directory.path())).unwrap();
        let (backend, _) = AppClient::start_native(launch).unwrap();
        let (controller, writes, library) = opening_controller(cx);
        let record = ActiveAudiobook::new(
            BookLocator::new(library, "d".repeat(64).parse().unwrap()),
            42_000,
            1.25,
            app::PlaybackMetadata { format: book_model::BookFormat::M4b, title: "Recoverable book".into(), author: "Author".into(), narrator: None, duration_ms: 100_000, chapters: vec![], tracks: vec![], cover: vec![] },
        );
        controller.update(cx, |controller, cx| {
            let session = reader_ui::restore_audiobook(&record, backend.library(library), cx);
            // Cancel the real persistence task before the deterministic executor
            // polls it. Tests below inject the final writer result at the barrier.
            session.update(cx, |session, _| drop(session.stop_and_flush()));
            controller.install(record, session, true, cx);
        });
        (controller, writes, directory)
    }

    fn assert_paused_recovery(controller: &Entity<PlaybackController>, cx: &mut gpui::TestAppContext) {
        controller.read_with(cx, |controller, cx| {
            let active = controller.current.as_ref().expect("failed removal must retain the dock");
            assert_eq!(active.record.position_ms, 42_000);
            assert_eq!(active.record.speed, 1.25);
            assert!(!active.session.read(cx).is_playing());
            assert!(controller.observation.is_some());
        });
        // Do not submit a real backend write during fixture teardown.
        controller.update(cx, |controller, cx| {
            controller.current.as_ref().unwrap().session.update(cx, |session, _| drop(session.stop_and_flush()));
        });
    }

    #[gpui::test]
    async fn failed_position_flush_retains_recovery_and_never_clears_record(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _directory) = stopped_controller(cx);
        let session = controller.read_with(cx, |controller, _| controller.current.as_ref().unwrap().session.clone());
        let result = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async { Err("progress failed".into()) }), cx)).await;
        assert_eq!(result, Err("progress failed".into()));
        assert_paused_recovery(&controller, cx);
        controller.read_with(cx, |controller, _| assert_eq!(controller.current.as_ref().unwrap().session, session));
        let mut replacement = false;
        while let Ok(command) = writes.try_recv() {
            match command {
                Save::Replace(Some(record)) => {
                    replacement = true;
                    assert_eq!(record.position_ms, 42_000);
                }
                Save::Replace(None) => panic!("failed progress save must not clear restoration"),
                _ => {}
            }
        }
        assert!(replacement);
    }

    #[gpui::test]
    async fn failed_clear_restores_paused_session_as_replacement(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _directory) = stopped_controller(cx);
        let preparation = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async { Ok(()) }), cx));
        cx.run_until_parked();
        assert!(matches!(writes.try_recv().unwrap(), Save::Update(_)));
        let Save::Flush(done) = writes.try_recv().unwrap() else {
            panic!("expected snapshot barrier");
        };
        done.try_send(Ok(())).unwrap();
        cx.run_until_parked();
        assert!(matches!(writes.try_recv().unwrap(), Save::Replace(None)));
        let Save::Flush(done) = writes.try_recv().unwrap() else {
            panic!("expected clear barrier");
        };
        done.try_send(Err("disk full".into())).unwrap();
        assert_eq!(preparation.await, Err("disk full".into()));
        assert_paused_recovery(&controller, cx);
        assert!(matches!(writes.try_recv().unwrap(), Save::Replace(Some(_))));
    }

    #[gpui::test]
    async fn failed_snapshot_flush_recovers_without_queuing_clear(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _directory) = stopped_controller(cx);
        let preparation = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async { Ok(()) }), cx));
        cx.run_until_parked();
        assert!(matches!(writes.try_recv().unwrap(), Save::Update(_)));
        let Save::Flush(done) = writes.try_recv().unwrap() else {
            panic!("expected snapshot barrier");
        };
        done.try_send(Err("snapshot failed".into())).unwrap();
        assert_eq!(preparation.await, Err("snapshot failed".into()));
        assert_paused_recovery(&controller, cx);
        assert!(matches!(writes.try_recv().unwrap(), Save::Replace(Some(_))));
        assert!(writes.is_empty(), "failed snapshot must never queue a clear");
    }

    #[gpui::test]
    async fn removal_waits_for_both_state_barriers_before_dropping_session(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _directory) = stopped_controller(cx);
        let preparation = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async { Ok(()) }), cx));
        for clearing in [false, true] {
            cx.run_until_parked();
            controller.read_with(cx, |controller, _| assert!(controller.current.is_some()));
            let command = writes.try_recv().unwrap();
            assert!(if clearing { matches!(command, Save::Replace(None)) } else { matches!(command, Save::Update(_)) });
            let Save::Flush(done) = writes.try_recv().unwrap() else {
                panic!("expected barrier");
            };
            done.try_send(Ok(())).unwrap();
        }
        preparation.await.unwrap();
        controller.read_with(cx, |controller, _| assert!(controller.current.is_none()));
    }

    #[gpui::test]
    async fn late_removal_failure_cannot_undo_explicit_close(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _directory) = stopped_controller(cx);
        let (done, positions) = async_channel::bounded(1);
        let preparation = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async move { positions.recv().await.unwrap() }), cx));
        controller.update(cx, |controller, cx| controller.close(cx));
        done.try_send(Err("progress failed".into())).unwrap();
        assert_eq!(preparation.await, Err("progress failed".into()));
        controller.read_with(cx, |controller, _| assert!(controller.current.is_none()));
        while let Ok(command) = writes.try_recv() {
            assert!(!matches!(command, Save::Replace(Some(_))), "late failure resurrected a closed session");
        }
    }

    #[gpui::test]
    async fn shutdown_during_clear_reselects_the_retained_restore_record(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _directory) = stopped_controller(cx);
        let preparation = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async { Ok(()) }), cx));
        cx.run_until_parked();
        assert!(matches!(writes.try_recv().unwrap(), Save::Update(_)));
        let Save::Flush(done) = writes.try_recv().unwrap() else {
            panic!("expected snapshot barrier");
        };
        done.try_send(Ok(())).unwrap();
        cx.run_until_parked();
        assert!(matches!(writes.try_recv().unwrap(), Save::Replace(None)));
        let Save::Flush(done) = writes.try_recv().unwrap() else {
            panic!("expected clear barrier");
        };
        let shutdown = controller.update(cx, |controller, cx| controller.begin_shutdown(cx)).unwrap();
        shutdown.await.unwrap();
        controller.update(cx, |controller, cx| controller.save_current(true, cx));
        assert!(matches!(writes.try_recv().unwrap(), Save::Replace(Some(_))), "shutdown snapshot must not be an ignored update after clear");
        done.try_send(Ok(())).unwrap();
        assert!(preparation.await.is_err());
        controller.read_with(cx, |controller, _| assert!(controller.current.is_some()));
    }

    #[gpui::test]
    async fn shutdown_joins_inflight_removal_progress_even_after_close(cx: &mut gpui::TestAppContext) {
        for close in [false, true] {
            for outcome in [Ok(()), Err("final producer failed".to_owned())] {
                let (controller, _writes, _directory) = stopped_controller(cx);
                let (done, positions) = async_channel::bounded(1);
                let removal = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async move { positions.recv().await.unwrap() }), cx));
                if close {
                    controller.update(cx, |controller, cx| controller.close(cx));
                }
                let mut shutdown = controller.update(cx, |controller, cx| controller.begin_shutdown(cx)).expect("shutdown must join the outstanding producer");
                let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
                assert!(shutdown.as_mut().poll(&mut poll).is_pending());
                done.try_send(outcome.clone()).unwrap();
                assert_eq!(shutdown.await, outcome);
                assert!(removal.await.is_err(), "shutdown must cancel deletion, not its pending progress flush");
                controller.read_with(cx, |controller, _| assert_eq!(controller.current.is_none(), close));
            }
        }
    }

    #[gpui::test]
    async fn cancelled_removal_waiter_does_not_cancel_shutdown_progress(cx: &mut gpui::TestAppContext) {
        let (controller, _writes, _directory) = stopped_controller(cx);
        let (done, positions) = async_channel::bounded(1);
        let removal = controller.update(cx, |controller, cx| controller.finish_active_removal(Box::pin(async move { positions.recv().await.unwrap() }), cx));
        drop(removal);
        let mut shutdown = controller.update(cx, |controller, cx| controller.begin_shutdown(cx)).unwrap();
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(shutdown.as_mut().poll(&mut poll).is_pending());
        done.try_send(Ok(())).unwrap();
        shutdown.await.unwrap();
    }

    #[gpui::test]
    async fn close_tracks_final_progress_and_completed_flushes_are_pruned(cx: &mut gpui::TestAppContext) {
        let (controller, _writes, _directory) = stopped_controller(cx);
        controller.update(cx, |controller, cx| {
            controller.close(cx);
            assert!(controller.current.is_none());
            assert_eq!(controller.pending_positions.len(), 1, "Close must retain the session's final producer");
        });
        cx.run_until_parked();
        controller.read_with(cx, |controller, _| assert!(controller.pending_positions[0].1.peek().is_some()));
        let (done, saved) = async_channel::bounded(1);
        controller.update(cx, |controller, cx| {
            drop(controller.track_position_flush(sync_common::LibraryId::from_u128(1), Box::pin(async move { saved.recv().await.unwrap() }), cx));
            assert_eq!(controller.pending_positions.len(), 1, "completed producers must not accumulate across replacements");
        });
        let mut shutdown = controller.update(cx, |controller, cx| controller.begin_shutdown(cx)).unwrap();
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(shutdown.as_mut().poll(&mut poll).is_pending());
        done.try_send(Ok(())).unwrap();
        shutdown.await.unwrap();
    }

    #[gpui::test]
    async fn removal_after_close_waits_only_for_origin_progress(cx: &mut gpui::TestAppContext) {
        for outcome in [Ok(()), Err("final save failed".to_owned())] {
            let (controller, _writes, library) = opening_controller(cx);
            let (done, saved) = async_channel::bounded(1);
            controller.update(cx, |controller, cx| {
                controller.close(cx);
                drop(controller.track_position_flush(library, Box::pin(async move { saved.recv().await.unwrap() }), cx));
            });
            let other = sync_common::LibraryId::from_u128(999);
            assert_eq!(controller.update(cx, |controller, cx| controller.prepare_removal(other, cx)).await, Ok(()));
            let mut removal = controller.update(cx, |controller, cx| controller.prepare_removal(library, cx));
            let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(removal.as_mut().poll(&mut poll).is_pending());
            done.try_send(outcome.clone()).unwrap();
            assert_eq!(removal.await, outcome);
        }
    }

    fn opening_controller(cx: &mut gpui::TestAppContext) -> (Entity<PlaybackController>, async_channel::Receiver<Save>, sync_common::LibraryId) {
        let (saves, received) = async_channel::unbounded();
        let library = sync_common::LibraryId::from_u128(1);
        let locator = BookLocator::new(library, "d".repeat(64).parse().unwrap());
        let controller = cx.new(|_| PlaybackController { current: None, generation: 0, opens: OpenOrder::default(), opening: Some(locator), user_changed: false, quitting: false, pending_positions: vec![], saves, observation: None });
        (controller, received, library)
    }

    #[gpui::test]
    async fn shutdown_seals_opening_and_preserves_restore_state(cx: &mut gpui::TestAppContext) {
        let (controller, writes, library) = opening_controller(cx);
        let locator = BookLocator::new(library, "d".repeat(64).parse().unwrap());
        controller.update(cx, |controller, cx| {
            let old_request = controller.reserve_open();
            assert!(controller.begin_shutdown(cx).is_none());
            assert!(controller.opening.is_none());
            assert!(controller.user_changed);
            assert!(!controller.prepare_open(old_request, &locator, cx));
            let late_request = controller.reserve_open();
            assert!(!controller.prepare_open(late_request, &locator, cx));
            assert!(!controller.activate_current(late_request, &locator, None, cx));
            let generation = controller.generation;
            controller.close(cx);
            assert!(controller.begin_shutdown(cx).is_none());
            assert_eq!(controller.generation, generation, "shutdown must be idempotent");
        });
        let error = controller.update(cx, |controller, cx| controller.prepare_removal(library, cx)).await;
        assert_eq!(error, Err("Application is shutting down".into()));
        assert!(writes.is_empty(), "late actions must not clear the restorable session");
        // Shutdown may still enqueue and await the final ordered save barrier.
        let flush = controller.read_with(cx, |controller, _| controller.flush_saves());
        let Save::Flush(done) = writes.recv().await.unwrap() else {
            panic!("expected flush");
        };
        done.send(Ok(())).await.unwrap();
        flush.await.unwrap();
    }

    #[gpui::test]
    async fn removal_cancels_opening_and_waits_for_state_flush(cx: &mut gpui::TestAppContext) {
        let (controller, writes, library) = opening_controller(cx);
        let mut preparation = controller.update(cx, |controller, cx| controller.prepare_removal(library, cx));
        controller.read_with(cx, |controller, _| {
            assert!(controller.opening.is_none());
            assert_eq!(controller.generation, 1);
        });
        let mut poll = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(preparation.as_mut().poll(&mut poll).is_pending());
        assert!(matches!(writes.recv().await.unwrap(), Save::Replace(None)));
        let Save::Flush(done) = writes.recv().await.unwrap() else {
            panic!("expected flush barrier");
        };
        done.send(Ok(())).await.unwrap();
        preparation.await.unwrap();
    }

    #[gpui::test]
    async fn failed_state_flush_rejects_removal(cx: &mut gpui::TestAppContext) {
        let (controller, writes, library) = opening_controller(cx);
        let preparation = controller.update(cx, |controller, cx| controller.prepare_removal(library, cx));
        assert!(matches!(writes.recv().await.unwrap(), Save::Replace(None)));
        let Save::Flush(done) = writes.recv().await.unwrap() else {
            panic!("expected flush barrier");
        };
        done.send(Err("disk full".to_owned())).await.unwrap();
        assert_eq!(preparation.await, Err("disk full".to_owned()));
    }

    #[gpui::test]
    async fn removing_another_library_does_not_cancel_playback_opening(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _) = opening_controller(cx);
        controller.update(cx, |controller, cx| controller.prepare_removal(sync_common::LibraryId::from_u128(2), cx)).await.unwrap();
        assert!(writes.is_empty());
        controller.read_with(cx, |controller, _| assert!(controller.opening.is_some()));
    }

    #[gpui::test]
    fn application_windows_share_one_controller(cx: &mut gpui::TestAppContext) {
        let (saves, _received) = async_channel::unbounded();
        let first = cx.update(|cx| {
            PlaybackController::shared_with(cx, |_| PlaybackController { current: None, generation: 0, opens: OpenOrder::default(), opening: None, user_changed: false, quitting: false, pending_positions: vec![], saves, observation: None })
        });
        let second = cx.update(|cx| PlaybackController::shared_with(cx, |_| panic!("a second controller must not be constructed")));
        assert_eq!(first, second);
        second.update(cx, |controller, cx| controller.close(cx));
        first.read_with(cx, |controller, _| {
            assert!(controller.current.is_none());
            assert!(controller.user_changed);
            assert_eq!(controller.generation, 1);
        });
    }

    #[test]
    fn slow_older_format_probe_cannot_override_newer_audiobook_choice() {
        let mut order = OpenOrder::default();
        let first = order.reserve();
        let second = order.reserve();
        assert!(order.accept(second));
        assert!(!order.accept(first));
        assert!(!order.accept(second));
    }

    #[gpui::test]
    async fn removal_invalidates_unresolved_probes_without_cancelling_other_libraries(cx: &mut gpui::TestAppContext) {
        let (controller, writes, _) = opening_controller(cx);
        let removed = sync_common::LibraryId::from_u128(2);
        let other = sync_common::LibraryId::from_u128(3);
        let locator = |library| BookLocator::new(library, "d".repeat(64).parse().unwrap());
        let (old, unrelated) = controller.update(cx, |controller, _| (controller.reserve_open(), controller.reserve_open()));
        controller.update(cx, |controller, cx| controller.prepare_removal(removed, cx)).await.unwrap();
        assert!(writes.is_empty());
        controller.update(cx, |controller, cx| {
            assert!(!controller.prepare_open(old, &locator(removed), cx));
            assert!(controller.prepare_open(unrelated, &locator(other), cx));
            let retry = controller.reserve_open();
            assert!(controller.prepare_open(retry, &locator(removed), cx), "a new attempt after failed deletion must be allowed");
        });
    }

    #[test]
    fn newer_choice_survives_earlier_completion_and_non_audio_probes() {
        let mut order = OpenOrder::default();
        let first = order.reserve();
        let second = order.reserve();
        let _epub_probe = order.reserve();
        assert!(order.accept(first));
        assert!(order.accept(second));
        let pending = order.reserve();
        order.cancel_pending();
        assert!(!order.accept(pending));
        let after_close = order.reserve();
        assert!(order.accept(after_close));
    }
}
