//! Event delivery owned by one concrete library session.

use async_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::mem::swap;

use crate::reading_positions::ReadingPositionPolicy;
use crate::scheduler::SyncScheduler;
use library_model::{BookDownloadStatus, DownloadState, LibraryUpdate};
use sync_common::BookId;
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub enum LibraryEvent {
    /// Book bytes are now present on the server; lifecycle metadata can publish.
    BooksUploaded,
    /// Selection is reported with the policy revision observed by the scan.
    ReadingPositionsSelected(Vec<(BookId, u64)>),
    /// Metadata acknowledgement may have made covers uploadable.
    AssetWorkAvailable,
    /// File replay finished and admitted uploads; enrichment can look again.
    FilesPrepared,
    ThumbnailGenerated {
        content_hash: BookId,
    },
    DownloadStatusChanged {
        content_hash: BookId,
        state: DownloadState,
    },
    ContentsChanged,
    /// A committed write whose notification is suppressed during a directory import.
    ContentsCommitted,
    /// A presentation update that does not schedule another worker pass.
    Update(LibraryUpdate),
    /// Activity guards report their lifecycle; only the actor applies it.
    DirectoryImportStarted {
        id: uuid::Uuid,
    },
    DirectoryImportFinished {
        id: uuid::Uuid,
        scanning: bool,
    },
    ImportStarted,
    /// `busy`: another import or a scan is still running.
    ImportComplete {
        busy: bool,
    },
    ScanStarted,
    /// `busy`: a folder import is still running.
    ScanComplete {
        busy: bool,
    },
}

/// Send-only capability for workers and callbacks. Mutable coordination stays
/// on the owning library thread; emitting an event never accesses that state.
#[derive(Clone)]
pub struct LibraryEventRouter {
    sender: Sender<LibraryEvent>,
    pub reading_positions: watch::Receiver<ReadingPositionPolicy>,
    pub sync: SyncScheduler,
    pub assets: SyncScheduler,
    pub files: SyncScheduler,
    pub enrichment: SyncScheduler,
}

impl LibraryEventRouter {
    pub fn new(sync: SyncScheduler, assets: SyncScheduler, files: SyncScheduler, enrichment: SyncScheduler) -> (Self, LibraryEventState) {
        let (sender, receiver) = async_channel::unbounded();
        let (position_updates, reading_positions) = watch::channel(ReadingPositionPolicy::default());
        (Self { sender, reading_positions, sync, assets, files, enrichment }, LibraryEventState { receiver, position_updates, updates: Vec::new(), latest_downloads: HashMap::new(), directory_imports: HashSet::new(), closed: false })
    }

    /// Announces that the set of books or folders changed.
    ///
    /// A write site cannot tell whether it is alone or the four-thousandth
    /// commit of an import, so it must not decide how often this is worth
    /// saying. An import in flight brackets itself and announces its result
    /// once when it finishes; every commit inside that bracket is the same
    /// announcement repeated, and each one costs every open view a full reload.
    pub fn notify_contents_changed(&self) {
        self.emit(LibraryEvent::ContentsCommitted);
    }

    pub fn emit(&self, event: LibraryEvent) {
        let _ = self.sender.try_send(event);
    }

    pub fn publish_update(&self, update: LibraryUpdate) {
        self.emit(LibraryEvent::Update(update));
    }

    fn route(&self, event: &LibraryEvent) {
        match event {
            LibraryEvent::BooksUploaded => self.sync.wake(),
            LibraryEvent::AssetWorkAvailable | LibraryEvent::ThumbnailGenerated { .. } => self.assets.wake(),
            LibraryEvent::FilesPrepared => self.enrichment.wake(),
            LibraryEvent::ContentsChanged | LibraryEvent::ImportComplete { .. } | LibraryEvent::ScanComplete { .. } => {
                self.assets.wake();
                self.files.wake();
            }
            LibraryEvent::DownloadStatusChanged { content_hash, state: DownloadState::Downloaded } => {
                self.assets.wake_hashes(vec![*content_hash]);
                self.files.wake();
            }
            _ => {}
        }
    }
}

/// Exclusively owned by the library actor. Download observations and import
/// tracking are operational state, independent of subscriber bookkeeping.
pub struct LibraryEventState {
    position_updates: watch::Sender<ReadingPositionPolicy>,
    receiver: Receiver<LibraryEvent>,
    closed: bool,
    updates: Vec<Sender<LibraryUpdate>>,
    latest_downloads: HashMap<BookId, DownloadState>,
    directory_imports: HashSet<uuid::Uuid>,
}

impl LibraryEventState {
    /// Keep subscribers and pending reading-position policy when folder recovery
    /// rebuilds the session's workers and storage handles.
    pub fn restart(&mut self, mut next: Self) {
        let position_policy = self.position_updates.borrow().clone();
        self.receiver.close();
        swap(&mut self.receiver, &mut next.receiver);
        swap(&mut self.position_updates, &mut next.position_updates);
        self.position_updates.send_replace(position_policy);
        self.latest_downloads.clear();
        self.directory_imports.clear();
        send_update(&mut self.updates, LibraryUpdate::Scanning(false));
        send_update(&mut self.updates, LibraryUpdate::Contents);
    }

    pub fn reading_position_changed(&mut self, hash: BookId, router: &LibraryEventRouter) {
        self.position_updates.send_modify(|policy| policy.changed(hash, web_time::Instant::now()));
        // Every edit schedules its quiet deadline. An earlier maximum-wait
        // wake is retained by the scheduler; unrelated passes re-arm it.
        router.sync.wake_after(std::time::Duration::from_secs(5));
    }

    pub fn receiver(&self) -> Receiver<LibraryEvent> {
        self.receiver.clone()
    }

    /// Apply queued events before reading coordination state or replying to a
    /// command. No borrow of this state may span an await.
    pub fn drain(&mut self, router: &LibraryEventRouter) {
        while let Ok(event) = self.receiver.try_recv() {
            self.apply(event, router);
        }
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
        for sender in self.updates.drain(..) {
            sender.close();
        }
    }

    pub fn subscribe_updates(&mut self) -> Receiver<LibraryUpdate> {
        let (sender, receiver) = async_channel::unbounded();
        if !self.closed {
            self.updates.push(sender);
        }
        receiver
    }

    pub fn directory_imports_active(&self) -> bool {
        !self.directory_imports.is_empty()
    }

    pub fn begin_directory_import(&mut self, id: uuid::Uuid, router: &LibraryEventRouter) {
        if !self.closed && self.directory_imports.insert(id) {
            self.apply(LibraryEvent::ImportStarted, router);
        }
    }

    pub fn finish_directory_import(&mut self, id: uuid::Uuid, scanning: bool, router: &LibraryEventRouter) {
        if !self.closed && self.directory_imports.remove(&id) {
            self.apply(LibraryEvent::ImportComplete { busy: scanning || self.directory_imports_active() }, router);
            router.sync.wake();
        }
    }

    pub fn download_state(&self, hash: &BookId) -> Option<&DownloadState> {
        self.latest_downloads.get(hash)
    }

    pub fn reset_download(&mut self, hash: &BookId) {
        if matches!(self.latest_downloads.get(hash), Some(DownloadState::Downloaded)) {
            self.latest_downloads.remove(hash);
        }
    }

    pub fn apply(&mut self, event: LibraryEvent, router: &LibraryEventRouter) {
        if self.closed {
            return;
        }
        let event = match event {
            LibraryEvent::ReadingPositionsSelected(positions) => {
                self.position_updates.send_modify(|policy| policy.selected(&positions));
                return;
            }
            LibraryEvent::DirectoryImportStarted { id } => {
                self.begin_directory_import(id, router);
                return;
            }
            LibraryEvent::DirectoryImportFinished { id, scanning } => {
                self.finish_directory_import(id, scanning, router);
                return;
            }
            LibraryEvent::ContentsCommitted => {
                if self.directory_imports_active() {
                    return;
                }
                router.sync.wake();
                LibraryEvent::ContentsChanged
            }
            event => event,
        };
        router.route(&event);
        let update = match event {
            LibraryEvent::Update(update) => update,
            LibraryEvent::ThumbnailGenerated { content_hash } => LibraryUpdate::Covers(vec![content_hash]),
            LibraryEvent::DownloadStatusChanged { content_hash, state } => {
                if self.latest_downloads.get(&content_hash) == Some(&state) {
                    return;
                }
                if self.latest_downloads.get(&content_hash).is_some_and(|previous| matches!((previous, &state), (DownloadState::Downloaded, DownloadState::Queued | DownloadState::Downloading(_)))) {
                    return;
                }
                self.latest_downloads.insert(content_hash, state.clone());
                LibraryUpdate::Download(BookDownloadStatus::new(content_hash, state))
            }
            LibraryEvent::AssetWorkAvailable
            | LibraryEvent::FilesPrepared
            | LibraryEvent::ContentsCommitted
            | LibraryEvent::DirectoryImportStarted { .. }
            | LibraryEvent::DirectoryImportFinished { .. }
            | LibraryEvent::ReadingPositionsSelected(_) => return,
            LibraryEvent::ScanStarted | LibraryEvent::ImportStarted => LibraryUpdate::Scanning(true),
            LibraryEvent::ContentsChanged | LibraryEvent::BooksUploaded => LibraryUpdate::Contents,
            LibraryEvent::ScanComplete { busy } | LibraryEvent::ImportComplete { busy } => {
                send_update(&mut self.updates, LibraryUpdate::Scanning(busy));
                LibraryUpdate::Contents
            }
        };
        send_update(&mut self.updates, update);
    }
}

impl Drop for LibraryEventState {
    fn drop(&mut self) {
        self.close();
    }
}

fn send_update(subscribers: &mut Vec<Sender<LibraryUpdate>>, update: LibraryUpdate) {
    subscribers.retain(|sender| sender.try_send(update.clone()).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::SyncRequest;
    use library_model::DownloadProgress;

    fn fixture() -> (LibraryEventRouter, LibraryEventState, Receiver<SyncRequest>) {
        let (sync, requests) = SyncScheduler::new();
        let (assets, _) = SyncScheduler::new();
        let (files, _) = SyncScheduler::new();
        let (enrichment, _) = SyncScheduler::new();
        let (router, state) = LibraryEventRouter::new(sync, assets, files, enrichment);
        (router, state, requests)
    }

    #[test]
    fn sync_status_reaches_all_subscribers_without_scheduling_another_pass() {
        let (sync, sync_requests) = SyncScheduler::new();
        let (assets, asset_requests) = SyncScheduler::new();
        let (files, file_requests) = SyncScheduler::new();
        let (enrichment, enrichment_requests) = SyncScheduler::new();
        let (router, mut state) = LibraryEventRouter::new(sync, assets, files, enrichment);
        let first = state.subscribe_updates();
        let second = state.subscribe_updates();

        router.publish_update(LibraryUpdate::SyncStatusChanged);
        state.drain(&router);

        assert_eq!(first.try_recv().unwrap(), LibraryUpdate::SyncStatusChanged);
        assert_eq!(second.try_recv().unwrap(), LibraryUpdate::SyncStatusChanged);
        assert!(first.try_recv().is_err());
        assert!(second.try_recv().is_err());
        for requests in [sync_requests, asset_requests, file_requests, enrichment_requests] {
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn import_bracket_suppresses_commits_and_duplicate_completions() {
        let (router, mut state, sync) = fixture();
        let updates = state.subscribe_updates();
        let first = uuid::Uuid::from_u128(1);
        let second = uuid::Uuid::from_u128(2);
        state.begin_directory_import(first, &router);
        state.begin_directory_import(second, &router);
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Scanning(true));
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Scanning(true));
        router.notify_contents_changed();
        state.drain(&router);
        assert!(updates.try_recv().is_err());
        assert!(sync.try_recv().is_err());

        state.finish_directory_import(first, false, &router);
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Scanning(true));
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Contents);
        assert!(state.directory_imports_active());
        assert!(sync.try_recv().is_ok());
        state.finish_directory_import(first, false, &router);
        assert!(updates.try_recv().is_err());
        assert!(sync.try_recv().is_err());

        state.finish_directory_import(second, false, &router);
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Scanning(false));
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Contents);
        assert!(!state.directory_imports_active());
        router.notify_contents_changed();
        state.drain(&router);
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Contents);
    }

    #[test]
    fn queued_progress_is_owned_without_subscribers_and_cannot_regress_completion() {
        let (router, mut state, _) = fixture();
        let hash = BookId::from_bytes([7; 32]);
        let downloading = DownloadState::Downloading(DownloadProgress::ZERO);
        router.emit(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: downloading.clone() });
        state.drain(&router);
        assert_eq!(state.download_state(&hash), Some(&downloading));

        let updates = state.subscribe_updates();
        router.emit(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Downloaded });
        router.emit(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: downloading });
        state.drain(&router);
        assert_eq!(state.download_state(&hash), Some(&DownloadState::Downloaded));
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Download(BookDownloadStatus::new(hash, DownloadState::Downloaded)));
        assert!(updates.try_recv().is_err());

        state.reset_download(&hash);
        router.emit(LibraryEvent::DownloadStatusChanged { content_hash: hash, state: DownloadState::Queued });
        state.drain(&router);
        assert_eq!(state.download_state(&hash), Some(&DownloadState::Queued));
    }

    #[test]
    fn sync_selection_uses_its_snapshot_revision_and_preserves_a_newer_edit() {
        let (router, mut state, _) = fixture();
        let hash = BookId::from_bytes([7; 32]);
        state.reading_position_changed(hash, &router);
        let scan_policy = router.reading_positions.borrow().clone();
        let selected = scan_policy.revisions([hash]);
        state.reading_position_changed(hash, &router);
        router.emit(LibraryEvent::ReadingPositionsSelected(selected));
        state.drain(&router);
        let current = router.reading_positions.borrow().clone();
        assert_ne!(scan_policy.revisions([hash]), current.revisions([hash]));
        assert!(current.waiting(web_time::Instant::now()).contains(&hash));
        router.emit(LibraryEvent::ReadingPositionsSelected(current.revisions([hash])));
        state.drain(&router);
        assert!(router.reading_positions.borrow().revisions([hash]).is_empty());
    }

    #[test]
    fn restart_preserves_pending_positions_and_late_selection_cannot_clear_new_edits() {
        let (old_router, mut state, _) = fixture();
        let updates = state.subscribe_updates();
        let first = BookId::from_bytes([7; 32]);
        let second = BookId::from_bytes([8; 32]);
        state.reading_position_changed(first, &old_router);
        state.reading_position_changed(second, &old_router);
        let before_restart = old_router.reading_positions.borrow().clone();
        let selected = before_restart.revisions([first, second]);

        let (router, next, _) = fixture();
        state.restart(next);
        assert!(old_router.sender.is_closed());
        assert!(old_router.reading_positions.has_changed().is_err());
        assert_eq!(router.reading_positions.borrow().revisions([first, second]), selected);
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Scanning(false));
        assert_eq!(updates.try_recv().unwrap(), LibraryUpdate::Contents);

        state.reading_position_changed(first, &router);
        router.emit(LibraryEvent::ReadingPositionsSelected(selected.clone()));
        state.drain(&router);
        let current = router.reading_positions.borrow().clone();
        assert!(current.revisions([second]).is_empty());
        assert_eq!(current.revisions([first]).len(), 1);
        assert_ne!(current.revisions([first]), before_restart.revisions([first]));
        assert_eq!(before_restart.revisions([first, second]), selected);
        assert!(updates.try_recv().is_err());
    }

    #[test]
    fn close_discards_pending_events_and_rejects_stale_emitters() {
        let (router, mut state, sync) = fixture();
        let updates = state.subscribe_updates();
        router.emit(LibraryEvent::BooksUploaded);
        state.close();
        state.drain(&router);
        router.emit(LibraryEvent::BooksUploaded);
        state.drain(&router);
        assert!(updates.is_closed());
        assert!(state.subscribe_updates().is_closed());
        assert!(router.sender.is_closed());
        assert!(sync.try_recv().is_err());
    }
}
