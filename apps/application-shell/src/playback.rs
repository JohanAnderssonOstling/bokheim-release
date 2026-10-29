//! Playback owners with separate ordered restoration writers.

use app::AppClient;
use browser_ui::{LibraryRemovalHook, LibraryRemovalPreparation, LibraryRemovalService};
use futures_util::{
    FutureExt,
    future::{LocalBoxFuture, Shared},
};
use gpui::{App, AppContext, Context, Entity, Global, Subscription, WeakEntity};
use library_model::BookLocator;
use reader_ui::ActiveAudiobook;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use sync_common::LibraryId;
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
        #[cfg(not(target_arch = "wasm32"))]
        if self.path.is_none() {
            return Ok(true);
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
    removed_through: std::collections::HashMap<LibraryId, u64>,
}
impl OpenOrder {
    fn reserve(&mut self) -> u64 {
        // Exhaustion rejects subsequent opens through the existing order check.
        self.next = self.next.saturating_add(1);
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
    fn cancel_library(&mut self, library: LibraryId) {
        self.removed_through.insert(library, self.next);
    }
    fn cancelled_for(&self, request: u64, library: LibraryId) -> bool {
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
    pending_positions: Vec<(LibraryId, PositionFlush)>,
    saves: async_channel::Sender<Save>,
    observation: Option<Subscription>,
}

#[derive(Default)]
struct PlaybackControllers(Vec<WeakEntity<PlaybackController>>);
impl Global for PlaybackControllers {}

#[cfg(feature = "kobo")]
pub(super) fn kobo_idle_sleep_allowed(cx: &App) -> bool {
    cx.try_global::<PlaybackControllers>().is_none_or(|registry| {
        !registry.0.iter().filter_map(WeakEntity::upgrade).any(|controller| {
            controller.read(cx).current.as_ref().is_some_and(|active| active.session.read(cx).prevents_idle_sleep())
        })
    })
}

#[cfg(feature = "kobo")]
pub(super) fn prepare_kobo_sleep(cx: &mut App) {
    let controllers = cx.try_global::<PlaybackControllers>().map(|registry| registry.0.clone()).unwrap_or_default();
    for controller in controllers.into_iter().filter_map(|controller| controller.upgrade()) {
        controller.update(cx, |controller, cx| {
            controller.opens.cancel_pending();
            controller.opening = None;
            controller.generation = controller.generation.wrapping_add(1);
            if let Some(active) = &controller.current {
                active.session.update(cx, |session, cx| session.prepare_sleep(cx));
            }
        });
    }
}

struct PlaybackLibraryRemoval {
    controllers: Vec<WeakEntity<PlaybackController>>,
}

impl LibraryRemovalService for PlaybackLibraryRemoval {
    fn prepare_removal(&self, library: LibraryId, cx: &mut App) -> LibraryRemovalPreparation {
        let preparations = self.controllers.iter().filter_map(|controller| controller.upgrade()).map(|controller| controller.update(cx, |controller, cx| controller.prepare_removal(library, cx))).collect::<Vec<_>>();
        Box::pin(async move {
            for preparation in preparations {
                preparation.await?;
            }
            Ok(())
        })
    }
}

impl PlaybackController {
    pub fn shared(backend: AppClient, restore_path: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        let controller = Self::shared_with(cx, |cx| Self::new(backend, RestoreStore::new(restore_path), cx));
        Self::register(&controller, cx);
        controller
    }

    pub fn independent(backend: AppClient, restore_path: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        let controller = cx.new(|cx| Self::new(backend, RestoreStore::new(restore_path), cx));
        Self::register(&controller, cx);
        controller
    }

    pub fn has_controllers(cx: &App) -> bool {
        cx.try_global::<PlaybackControllers>().is_some_and(|registry| registry.0.iter().any(|controller| controller.upgrade().is_some()))
    }

    fn register(controller: &Entity<Self>, cx: &mut App) {
        let mut controllers = cx.try_global::<PlaybackControllers>().map(|registry| registry.0.clone()).unwrap_or_default();
        controllers.retain(|existing| existing.upgrade().is_some());
        if !controllers.iter().any(|existing| existing.entity_id() == controller.entity_id()) {
            controllers.push(controller.downgrade());
        }
        cx.set_global(PlaybackControllers(controllers.clone()));
        cx.set_global(LibraryRemovalHook(Rc::new(PlaybackLibraryRemoval { controllers })));
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
                        let error = "Audiobook restore record changed outside this playback controller".to_owned();
                        log::warn!("{error}");
                        write_error = Some(error);
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
                if let Some(flush) = position_flush
                    && let Err(error) = flush.await
                {
                    log::warn!("could not save final audiobook position: {error}");
                }
                if let Some(snapshot) = snapshot {
                    let _ = saves.try_send(Save::Replace(Some(snapshot())));
                }
                let (done, finished) = async_channel::bounded(1);
                if saves.try_send(Save::Flush(done)).is_ok()
                    && let Ok(Err(error)) = finished.recv().await
                {
                    log::warn!("could not flush active audiobook state: {error}");
                }
            }
        })
        .detach();
        Self { current: None, generation: 0, opens: OpenOrder::default(), opening: None, user_changed: false, quitting: false, pending_positions: vec![], saves, observation: None }
    }

    fn begin_shutdown(&mut self, cx: &mut Context<Self>) -> Option<LibraryRemovalPreparation> {
        if self.quitting {
            return None;
        }
        self.quitting = true;
        self.user_changed = true;
        self.opens.cancel_pending();
        self.opening = None;
        self.generation = self.generation.wrapping_add(1);
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
            if let Some(flush) = current
                && let Err(error) = flush.await
            {
                result = Err(error);
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
        self.generation = self.generation.wrapping_add(1);
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
        let metadata = library_model::PlaybackMetadata { format: book_model::BookFormat::M4b, title, author: String::new(), narrator: None, duration_ms: 0, chapters: vec![], tracks: vec![], cover: vec![] };
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

    pub fn close_pane(&mut self, cx: &mut Context<Self>) -> LibraryRemovalPreparation {
        self.close(cx);
        let positions = self.pending_positions.iter().map(|(_, flush)| flush.clone()).collect::<Vec<_>>();
        let saves = self.flush_saves();
        Box::pin(async move {
            for position in positions {
                position.await?;
            }
            saves.await
        })
    }

    fn flush_saves(&self) -> LibraryRemovalPreparation {
        let (done, finished) = async_channel::bounded(1);
        let queued = self.saves.try_send(Save::Flush(done)).is_ok();
        Box::pin(async move {
            if !queued {
                return Err("Audiobook state writer stopped".into());
            }
            finished.recv().await.map_err(|_| "Audiobook state writer stopped".to_owned())?
        })
    }

    fn prepare_removal(&mut self, library: LibraryId, cx: &mut Context<Self>) -> LibraryRemovalPreparation {
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

    fn finish_active_removal(&mut self, positions: LibraryRemovalPreparation, cx: &mut Context<Self>) -> LibraryRemovalPreparation {
        let Some(active) = &self.current else {
            return Box::pin(async { Err("Playback session ended while preparing library removal; try again".into()) });
        };
        let library = *active.record.locator.library_id();
        let positions = self.track_position_flush(library, positions, cx);
        self.opens.cancel_pending();
        self.opening = None;
        self.user_changed = true;
        self.observation = None;
        self.generation = self.generation.wrapping_add(1);
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
                    controller.generation = controller.generation.wrapping_add(1);
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

    fn track_position_flush(&mut self, library: LibraryId, positions: LibraryRemovalPreparation, cx: &mut Context<Self>) -> PositionFlush {
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
        self.generation = self.generation.wrapping_add(1);
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

#[cfg(test)]
mod order_tests {
    use super::OpenOrder;

    #[test]
    fn exhausting_request_ids_never_reaccepts_an_old_open() {
        let mut order = OpenOrder { next: u64::MAX - 1, ..OpenOrder::default() };
        let last = order.reserve();
        assert_eq!(last, u64::MAX);
        assert!(order.accept(last));
        let exhausted = order.reserve();
        assert!(!order.accept(exhausted));
        assert!(!order.accept(1));
    }
}
