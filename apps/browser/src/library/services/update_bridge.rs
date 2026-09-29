//! Translates backend library updates into GPUI events.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{AppContext, Context, Entity, EventEmitter, Task};
use library_backend::LibraryClient;
use library_model::LibraryUpdate;
use sync_common::{ContentHash, LibraryId};

pub(crate) struct LibraryContentsChanged;

pub(crate) struct LibraryScanChanged;

pub(crate) struct LibraryCoversChanged {
    content_hashes: Rc<[ContentHash]>,
}

pub(crate) struct LibraryDownloadChanged {
    pub(crate) status: app::BookDownloadStatus,
}

impl LibraryCoversChanged {
    pub(crate) fn contains(&self, content_hash: &ContentHash) -> bool {
        self.content_hashes.contains(content_hash)
    }
}

pub(crate) struct LibraryUpdateBridge {
    library: LibraryClient,
    downloads: HashMap<ContentHash, app::DownloadState>,
    download_watches: HashMap<ContentHash, Task<()>>,
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
                cx.defer_update(bridge.clone(), move |bridge, cx| bridge.apply(batch, cx));
            }
        });
        let mut bridge = Self { library, downloads: HashMap::new(), download_watches: HashMap::new(), scanning: None, importing: false, book_count: None, _updates: task, _book_count_request: None };
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

    pub(crate) fn download_state(&self, content_hash: ContentHash) -> Option<app::DownloadState> {
        self.downloads.get(&content_hash).cloned()
    }

    /// Book rows bootstrap the display only once. From then on this bridge's
    /// backend subscription owns the state; a later browse response cannot
    /// roll progress or completion back to an older row snapshot.
    pub(crate) fn track_download(&mut self, content_hash: ContentHash, downloaded: bool, requested: bool, cx: &mut Context<Self>) {
        self.downloads.entry(content_hash).or_insert_with(|| {
            if downloaded {
                app::DownloadState::Downloaded
            } else if requested {
                app::DownloadState::Queued
            } else {
                app::DownloadState::NotDownloaded
            }
        });
        if requested {
            self.watch_download(content_hash, cx);
        }
    }

    pub(crate) fn watch_download(&mut self, content_hash: ContentHash, cx: &mut Context<Self>) {
        if self.download_watches.contains_key(&content_hash) {
            return;
        }
        let library = self.library.clone();
        let task = cx.spawn(async move |bridge, cx| {
            let (states, initial) = match library.download_changes(content_hash).await {
                Ok(subscription) => subscription,
                Err(error) => {
                    log::warn!("cannot subscribe to download status for {content_hash}: {error}");
                    cx.defer_update(bridge, move |bridge, _| {
                        bridge.download_watches.remove(&content_hash);
                    });
                    return;
                }
            };
            cx.defer_update(bridge.clone(), move |bridge, cx| bridge.set_download_state(content_hash, initial, cx));
            while let Ok(state) = states.recv().await {
                cx.defer_update(bridge.clone(), move |bridge, cx| bridge.set_download_state(content_hash, state, cx));
            }
            cx.defer_update(bridge, move |bridge, _| {
                bridge.download_watches.remove(&content_hash);
            });
        });
        self.download_watches.insert(content_hash, task);
    }

    fn set_download_state(&mut self, content_hash: ContentHash, state: app::DownloadState, cx: &mut Context<Self>) {
        if matches!(self.downloads.get(&content_hash), Some(app::DownloadState::Downloaded)) && matches!(state, app::DownloadState::Queued | app::DownloadState::Downloading(_)) {
            return;
        }
        if self.downloads.get(&content_hash) == Some(&state) {
            return;
        }
        self.downloads.insert(content_hash, state.clone());
        cx.emit(LibraryDownloadChanged { status: app::BookDownloadStatus::new(content_hash, state) });
        cx.notify();
    }

    fn refresh_book_count(&mut self, cx: &mut Context<Self>) {
        let library = self.library.clone();
        let load = async move { library.book_count().await };
        self._book_count_request = Some(cx.spawn(async move |bridge, cx| {
            let result = load.await;
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
        let UpdateBatch { contents, covers, downloads, scanning } = batch;
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
            if !self.download_watches.contains_key(&status.content_hash) {
                self.set_download_state(status.content_hash, status.state, cx);
            }
        }
    }
}

/// Everything that arrived before the UI could answer, folded by how each kind
/// of update composes.
#[derive(Default)]
struct UpdateBatch {
    contents: bool,
    covers: Vec<ContentHash>,
    /// Kept per book: each status names a different book, or supersedes an
    /// earlier status for the same one.
    downloads: Vec<app::BookDownloadStatus>,
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
            LibraryUpdate::Scanning(scanning) => self.scanning = Some(scanning),
        }
    }
}
