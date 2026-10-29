//! Translates backend library updates into GPUI events.

use library_backend::{BookDownloadStatus, DownloadState};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{AppContext, Context, Entity, EventEmitter, Subscription, Task, WeakEntity};
use library_backend::LibraryClient;
use library_model::LibraryUpdate;
use sync_common::{BookId, LibraryId};

/// Rows seed availability until the download stream publishes a newer state.
pub(crate) fn initial_download_state(downloaded: bool, requested: bool) -> DownloadState {
    match (downloaded, requested) {
        (true, _) => DownloadState::Downloaded,
        (false, true) => DownloadState::Queued,
        (false, false) => DownloadState::NotDownloaded,
    }
}

pub(crate) struct LibraryContentsChanged;

pub(crate) struct LibraryScanChanged;

pub(crate) struct LibraryCoversChanged {
    content_hashes: Rc<[BookId]>,
}

/// Library-wide invalidation, emitted only when file availability changes.
pub(crate) struct LibraryDownloadChanged;

impl LibraryCoversChanged {
    pub(crate) fn contains(&self, content_hash: &BookId) -> bool {
        self.content_hashes.contains(content_hash)
    }
}

pub(super) struct BookDownload;

impl EventEmitter<DownloadState> for BookDownload {}

/// Keeps the per-book event source alive only while a view subscribes.
pub(crate) struct DownloadSubscription {
    _subscription: Subscription,
    _source: Entity<BookDownload>,
}

pub(crate) struct LibraryUpdateBridge {
    library: LibraryClient,
    downloads: HashMap<BookId, DownloadState>,
    download_events: HashMap<BookId, WeakEntity<BookDownload>>,
    scanning: Option<bool>,
    importing: bool,
    book_count: Option<usize>,
    _updates: Task<()>,
    _book_count_request: Option<Task<()>>,
}

impl EventEmitter<LibraryContentsChanged> for LibraryUpdateBridge {}
impl EventEmitter<LibraryCoversChanged> for LibraryUpdateBridge {}
impl EventEmitter<LibraryDownloadChanged> for LibraryUpdateBridge {}
impl EventEmitter<LibraryScanChanged> for LibraryUpdateBridge {}

impl LibraryUpdateBridge {
    pub(crate) fn create<C: AppContext>(backend: Rc<app::AppClient>, library_id: LibraryId, cx: &mut C) -> Entity<Self> {
        cx.new(|cx| Self::new(backend, library_id, cx))
    }

    fn new(backend: Rc<app::AppClient>, library_id: LibraryId, cx: &mut Context<Self>) -> Self {
        let library = backend.library(library_id);
        let update_library = library.clone();
        let task = cx.spawn(async move |bridge, cx| {
            let (updates, scanning) = match update_library.updates().await {
                Ok(state) => state,
                Err(error) => {
                    log::error!("failed to subscribe to library updates: {error}");
                    return;
                }
            };
            cx.defer_update(bridge.clone(), move |bridge, cx| {
                bridge.scanning = Some(scanning);
                cx.emit(LibraryScanChanged);
                cx.notify();
            });
            while let Ok(update) = updates.recv().await {
                // Everything already queued describes the same instant, so it
                // is answered together. "Contents changed" is idempotent — ten
                // copies ask for exactly what one asks for — and cover
                // notifications compose by union, so a burst costs one reload
                // instead of one per book.
                let mut batch = UpdateBatch::default();
                batch.absorb(update);
                while let Ok(update) = updates.try_recv() {
                    batch.absorb(update);
                }
                // The session owns this subscription, so an alert still arrives
                // after its initiating card or detail page has been closed.
                for (_, failure) in &batch.download_failures {
                    let message = crate::services::book_transfer_error_message(*failure);
                    if let Err(error) = bridge.update_in(cx, |_, window, cx| crate::services::notify_error("book-download-error", message, window, cx)) {
                        log::warn!("could not show download failure: {error}");
                    }
                }
                cx.defer_update(bridge.clone(), move |bridge, cx| bridge.apply(batch, cx));
            }
        });
        let mut bridge = Self { library, downloads: HashMap::new(), download_events: HashMap::new(), scanning: None, importing: false, book_count: None, _updates: task, _book_count_request: None };
        bridge.refresh_book_count(cx);
        bridge
    }

    pub(crate) fn set_importing(&mut self, importing: bool, cx: &mut Context<Self>) {
        self.importing = importing;
        if !importing {
            self.book_count = None;
            self.refresh_book_count(cx);
        }
        cx.emit(LibraryScanChanged);
        cx.notify();
    }

    pub(crate) fn scanning(&self) -> bool {
        self.importing || self.scanning == Some(true)
    }

    pub(crate) fn loading(&self) -> bool {
        self.scanning.is_none() || self.book_count.is_none()
    }

    pub(crate) fn book_count(&self) -> Option<usize> {
        self.book_count
    }

    pub(crate) fn download_state(&self, content_hash: BookId) -> Option<DownloadState> {
        self.downloads.get(&content_hash).cloned()
    }

    pub(super) fn download_source(&mut self, content_hash: BookId, cx: &mut Context<Self>) -> Entity<BookDownload> {
        self.download_events.get(&content_hash).and_then(WeakEntity::upgrade).unwrap_or_else(|| {
            let source = cx.new(|_| BookDownload);
            self.download_events.insert(content_hash, source.downgrade());
            source
        })
    }

    pub(super) fn subscribe_download<T: 'static>(source: Entity<BookDownload>, cx: &mut Context<T>, mut callback: impl FnMut(&mut T, &DownloadState, &mut Context<T>) + 'static) -> DownloadSubscription {
        let subscription = cx.subscribe(&source, move |view, _, state, cx| callback(view, state, cx));
        DownloadSubscription { _subscription: subscription, _source: source }
    }

    /// Book rows bootstrap the display only once. From then on the update
    /// stream owns the state; a later browse response cannot roll progress or
    /// completion back to an older row snapshot.
    pub(crate) fn track_download(&mut self, content_hash: BookId, downloaded: bool, requested: bool) {
        self.downloads.entry(content_hash).or_insert_with(|| initial_download_state(downloaded, requested));
    }

    fn set_download_state(&mut self, content_hash: BookId, state: DownloadState, cx: &mut Context<Self>) {
        if self.downloads.get(&content_hash) == Some(&state) {
            return;
        }
        self.downloads.insert(content_hash, state.clone());
        if let Some(source) = self.download_events.get(&content_hash).and_then(WeakEntity::upgrade) {
            source.update(cx, |_, cx| cx.emit(state.clone()));
        } else {
            self.download_events.remove(&content_hash);
        }
        // Only navigators need library-wide availability invalidation. Progress
        // goes directly to subscribers of this book, without visiting other cards.
        if matches!(state, DownloadState::Downloaded | DownloadState::NotDownloaded) {
            cx.emit(LibraryDownloadChanged);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_download_state_for_test(&mut self, content_hash: BookId, state: DownloadState, cx: &mut Context<Self>) {
        self.set_download_state(content_hash, state, cx);
    }

    fn refresh_book_count(&mut self, cx: &mut Context<Self>) {
        let library = self.library.clone();
        self._book_count_request = Some(cx.spawn(async move |bridge, cx| {
            let result = library.book_count().await;
            cx.defer_update(bridge, |bridge, cx| match result {
                Ok(book_count) => {
                    if bridge.book_count != Some(book_count) {
                        bridge.book_count = Some(book_count);
                        cx.notify();
                    }
                }
                Err(error) => log::warn!("failed to determine whether the selected library is empty: {error}"),
            });
        }));
    }

    fn apply(&mut self, batch: UpdateBatch, cx: &mut Context<Self>) {
        let UpdateBatch { contents, covers, downloads, scanning, .. } = batch;
        if let Some(scanning) = scanning {
            self.scanning = Some(scanning);
            if !scanning {
                self.book_count = None;
            }
            cx.emit(LibraryScanChanged);
            cx.notify();
        }
        if contents || scanning == Some(false) {
            self.refresh_book_count(cx);
        }
        if contents {
            cx.emit(LibraryContentsChanged);
        }
        if !covers.is_empty() {
            cx.emit(LibraryCoversChanged { content_hashes: covers.into() });
        }
        for status in downloads {
            self.set_download_state(status.content_hash, status.state, cx);
        }
    }
}

/// Everything that arrived before the UI could answer, folded by how each kind
/// of update composes.
#[derive(Default)]
struct UpdateBatch {
    contents: bool,
    covers: Vec<BookId>,
    /// Kept per book: each status names a different book, or supersedes an
    /// earlier status for the same one.
    downloads: Vec<BookDownloadStatus>,
    download_failures: Vec<(BookId, Option<ui_components::Failure>)>,
    scanning: Option<bool>,
}

impl UpdateBatch {
    fn absorb(&mut self, update: LibraryUpdate) {
        match update {
            LibraryUpdate::Contents => self.contents = true,
            LibraryUpdate::Covers(content_hashes) => self.covers.extend(content_hashes),
            LibraryUpdate::Download(status) => {
                self.downloads.retain(|existing| existing.content_hash != status.content_hash);
                self.downloads.push(status);
            }
            LibraryUpdate::DownloadFailed { content_hash, failure } => {
                self.download_failures.retain(|(hash, _)| *hash != content_hash);
                self.download_failures.push((content_hash, failure));
            }
            LibraryUpdate::Scanning(scanning) => self.scanning = Some(scanning),
            // These have their own subscribers and do not change book views.
            LibraryUpdate::CatalogImport { .. } | LibraryUpdate::SyncStatusChanged => {}
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use crate::navigation::LibrarySession;
    use gpui::TestAppContext;
    use library_model::DownloadProgress;
    use std::cell::Cell;

    #[test]
    fn download_alerts_survive_progress_coalescing_and_keep_each_books_failure() {
        let first = BookId::from_bytes([1; 32]);
        let second = BookId::from_bytes([2; 32]);
        let mut batch = UpdateBatch::default();
        use ui_components::Failure;
        batch.absorb(LibraryUpdate::DownloadFailed { content_hash: first, failure: Some(Failure::StorageFull) });
        batch.absorb(LibraryUpdate::Download(BookDownloadStatus::new(first, DownloadState::NotDownloaded)));
        batch.absorb(LibraryUpdate::DownloadFailed { content_hash: second, failure: Some(Failure::PermissionDenied) });
        assert_eq!(batch.download_failures, vec![(first, Some(Failure::StorageFull)), (second, Some(Failure::PermissionDenied))]);
        assert_eq!(batch.downloads, vec![BookDownloadStatus::new(first, DownloadState::NotDownloaded)]);
    }

    #[gpui::test]
    async fn download_progress_emits_precise_events_without_notifying_session_observers(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading()).await;
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _observation = cx.update(|cx| cx.observe(&updates, move |_, _| observed.set(observed.get() + 1)));
        let events = Rc::new(Cell::new(0));
        let received = events.clone();
        let _subscription = cx.update(|cx| cx.subscribe(&updates, move |_, _: &LibraryDownloadChanged, _| received.set(received.get() + 1)));
        let hash = BookId::from_bytes([9; 32]);
        let states = [DownloadState::Queued, DownloadState::Downloading(DownloadProgress::new(0.5).unwrap()), DownloadState::Downloaded, DownloadState::Downloaded, DownloadState::NotDownloaded, DownloadState::NotDownloaded];
        for state in states.iter().cloned() {
            updates.update(cx, |updates, cx| updates.set_download_state(hash, state, cx));
        }
        assert_eq!(notifications.get(), 0);
        assert_eq!(events.get(), 2, "only availability changes invalidate library-wide contents");
        updates.update(cx, |updates, cx| updates.apply(UpdateBatch { scanning: Some(true), ..Default::default() }, cx));
        assert_eq!(notifications.get(), 1, "scan/loading changes must still reach the session");
    }

    struct DownloadListener {
        states: Vec<DownloadState>,
        subscription: Option<DownloadSubscription>,
    }

    #[gpui::test]
    async fn downloads_dispatch_only_to_owners_of_the_matching_book(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        let hash = BookId::from_bytes([11; 32]);
        let other_hash = BookId::from_bytes([12; 32]);
        let mut listen = |hash| cx.new(|cx| DownloadListener { states: Vec::new(), subscription: Some(library.subscribe_download(hash, cx, |listener: &mut DownloadListener, state, _| listener.states.push(state.clone()))) });
        let card = listen(hash);
        let detail = listen(hash);
        let unrelated = listen(other_hash);
        let states = [DownloadState::Queued, DownloadState::Downloading(DownloadProgress::ZERO), DownloadState::Downloading(DownloadProgress::new(0.5).unwrap()), DownloadState::Downloaded];
        for state in states.iter().cloned() {
            updates.update(cx, |updates, cx| updates.set_download_state(hash, state.clone(), cx));
            updates.update(cx, |updates, cx| updates.set_download_state(hash, state, cx));
        }
        cx.update(|cx| {
            assert_eq!(card.read(cx).states, states);
            assert_eq!(detail.read(cx).states, states);
            assert!(unrelated.read(cx).states.is_empty(), "unrelated callbacks must never run");
        });
        updates.update(cx, |updates, _| {
            updates.track_download(hash, false, true);
            assert_eq!(updates.download_state(hash), Some(DownloadState::Downloaded), "stale rows must not replace stream state");
        });
        card.update(cx, |listener, _| {
            listener.subscription.take();
        });
        cx.update(|cx| assert!(updates.read(cx).download_events[&hash].upgrade().is_some(), "the detail still owns the subscription"));
        detail.update(cx, |listener, _| {
            listener.subscription.take();
        });
        cx.update(|cx| assert!(updates.read(cx).download_events[&hash].upgrade().is_none(), "the bridge must not keep abandoned event sources alive"));
        updates.update(cx, |updates, cx| updates.set_download_state(hash, DownloadState::NotDownloaded, cx));
        cx.update(|cx| assert!(!updates.read(cx).download_events.contains_key(&hash)));
    }
}
