//! One book's page.
//!
//! Its owner keeps the browse view mounted in memory and caches this entity
//! when returning to it, preserving its contents. Reader entry drops hidden
//! pages' cover references; displayed pages in other panes keep their artwork.
//!
//! Details, cover art and torrent information load concurrently. The page
//! uses its current pane bounds to reveal the complete book in its final
//! layout rather than moving the title and controls as requests finish.

use ui_components::user_facing_error;

use crate::services::{book_transfer_error_message, import_catalog_book_with_progress, notify_error, notify_info};
use library_backend::{DownloadState, ThumbnailResolution};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, AppContext, AvailableSpace, Context, Entity, FocusHandle, Focusable, IntoElement, MouseButton, Render, SharedString, Task, Window, canvas, div, px};
use gpui_component::{IconName, Placement, WindowExt as _};
use gpui_image_eviction::track_image;
use library_model::{BookCardRow, BookDetail, BookLocator, BookSource, BookTocEntry, CardKey, LibraryFileTypeFilter};
use sync_common::{BookId, ROOT_DIR_ID};
use ui_components as components;
use ui_components::ScrollableElement as _;

use crate::library::pages::with_retry_banner;
use crate::library::services::{DownloadSubscription, LibraryContext};
use crate::library::widgets::{CoverVisibility, CoversEvicted, LoadedCover, cover_aspect_ratio, generated_cover_variant, loaded_cover_from_jpeg};

/// Leaves the book's page for the location it was opened from on Back.
/// Owned by the browse page, which is what knows how to leave the detail location.
pub(super) struct CloseBookDetail;

/// The page's own width — its pane's, not the window's — at which it takes
/// each layout. Below the sidebar breakpoint a column beside the player would
/// be too narrow to read; below the stacked one, so would the title beside
/// the cover.
const BOOK_DETAIL_SIDEBAR_BREAKPOINT: f32 = 950.0;
const BOOK_DETAIL_THREE_COLUMN_BREAKPOINT: f32 = 1200.0;
const BOOK_DETAIL_STACKED_BREAKPOINT: f32 = 600.0;
/// Where the title steps up to its larger size outside the columns.
const BOOK_DETAIL_LARGE_TITLE_WIDTH: f32 = 760.0;

#[derive(Clone, Copy, PartialEq)]
enum DetailLayout {
    /// The player column beside the contents and description, each scrolling
    /// on its own.
    Columns,
    /// One page: the cover beside the title, the player under both at full
    /// width, then the contents and description.
    Hero,
    /// One page, too narrow for anything beside the cover: the cover, then the
    /// title centred under it, the player, the contents and description.
    Stacked,
}

enum DetailCover {
    /// Retain the visible image while a higher-resolution replacement loads.
    Loading(Option<LoadedCover>),
    Loaded(LoadedCover),
    Missing,
    NeedsReload,
}

impl DetailCover {
    fn loaded(&self) -> Option<&LoadedCover> {
        match self {
            Self::Loading(cover) => cover.as_ref(),
            Self::Loaded(cover) => Some(cover),
            Self::Missing | Self::NeedsReload => None,
        }
    }
}

pub(crate) struct BookDetailPage {
    ui: LibraryContext,
    /// What the grid knew, replaced by the request's own copy when it lands.
    /// The card supplies the identity used to request the full details and art.
    book: BookCardRow,
    /// Once revealed, background refreshes keep the book visible.
    ready: bool,
    detail: Option<BookDetail>,
    error: Option<SharedString>,
    cover: DetailCover,
    cover_visibility: CoverVisibility,
    cover_generation: u64,
    _cover_evictions: gpui::Subscription,
    _contents_updates: gpui::Subscription,
    /// Contents open collapsed, so a book with two hundred entries does not bury
    /// everything under it. This is what the reader has opened since.
    expanded: HashSet<String>,
    /// Whether the contents and the description sections are open, where they
    /// can be folded: in two columns and narrower.
    contents_open: bool,
    description_open: bool,
    focus: FocusHandle,
    /// The load in flight. Replacing it drops the one before, which cancels it.
    request: Option<Task<()>>,
    _cover_task: Option<Task<()>>,
    download_requested_by_page: bool,
    /// The window owns the import so leaving this page does not cancel feedback.
    adding: bool,
    _download_updates: Option<DownloadSubscription>,
    #[cfg(not(target_arch = "wasm32"))]
    torrents: Vec<library_backend::BookTorrent>,
    #[cfg(not(target_arch = "wasm32"))]
    torrents_loading: bool,
    #[cfg(not(target_arch = "wasm32"))]
    torrent_task: Option<Task<()>>,
}

impl gpui::EventEmitter<CloseBookDetail> for BookDetailPage {}

impl BookDetailPage {
    fn reset_contents_read_status(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        let locator = self.locator();
        let playing_reset = self.ui.now_playing().read(cx).book.as_ref().filter(|playing| Some(&playing.locator) == locator.as_ref()).map(|playing| playing.reset_chapter_read_status.clone());
        if let Some(reset) = playing_reset {
            reset(target.clone(), cx);
            if let Some(detail) = &mut self.detail {
                if let Some(target) = target.as_deref() {
                    detail.chapter_read_status.reset_chapter(&detail.toc, target);
                } else {
                    detail.chapter_read_status.reset();
                }
            }
            cx.notify();
            return;
        }
        let Some(hash) = self.book.library_file() else { return };
        let library = self.ui.backend().clone();
        cx.spawn(async move |page, cx| {
            let result = match target {
                Some(target) => library.reset_chapter_read_status_at(hash, target).await,
                None => library.reset_chapter_read_status(hash).await,
            };
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(status) => {
                        if let Some(detail) = &mut page.detail {
                            detail.chapter_read_status = status;
                        }
                    }
                    Err(error) => page.error = Some(user_facing_error(&error, "Could not reset read status. Please try again.").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The page of a library, catalog or feed book.
    pub(crate) fn create(book: BookCardRow, cover: Option<LoadedCover>, ui: LibraryContext, cx: &mut impl AppContext) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(ui.now_playing(), |_, _, cx| cx.notify()).detach();
            if let Some(cover) = &cover {
                track_image(cover, cx);
            }
            let download_updates = Self::download_subscription(&book, &ui, cx);
            let contents_updates = cx.subscribe(&ui.updates(), |page: &mut Self, _, _: &crate::library::services::LibraryContentsChanged, cx| {
                let Some(hash) = page.book.library_file() else { return };
                let library = page.ui.backend().clone();
                page.finish_load(async move { library.book_detail(hash).await }, cx);
            });
            let cover_evictions = cx.observe_global::<CoversEvicted>(|page: &mut Self, cx| {
                if page.cover_visibility.is_visible() {
                    return;
                }
                page.cover_generation = page.cover_generation.wrapping_add(1);
                page._cover_task.take();
                page.cover = DetailCover::NeedsReload;
                cx.notify();
            });
            let mut page = Self {
                ui,
                book,
                ready: false,
                detail: None,
                error: None,
                cover: DetailCover::Loading(cover),
                cover_visibility: CoverVisibility::default(),
                cover_generation: 0,
                _cover_evictions: cover_evictions,
                _contents_updates: contents_updates,
                expanded: HashSet::new(),
                contents_open: true,
                description_open: true,
                focus: cx.focus_handle(),
                request: None,
                _cover_task: None,
                download_requested_by_page: false,
                adding: false,
                _download_updates: download_updates,
                #[cfg(not(target_arch = "wasm32"))]
                torrents: Vec::new(),
                #[cfg(not(target_arch = "wasm32"))]
                torrents_loading: false,
                #[cfg(not(target_arch = "wasm32"))]
                torrent_task: None,
            };
            page.load(cx);
            // Detail art must be fetched separately because card thumbnails
            // already carry their crop.
            page.load_cover(cx);
            page
        })
    }

    /// The book's identity, as its card has it.
    pub(super) fn key(&self) -> CardKey {
        self.book.key()
    }

    /// Where the book is in this library; none for a catalog or feed book.
    pub(super) fn locator(&self) -> Option<BookLocator> {
        self.book.library_file().map(|content_hash| BookLocator::new(*self.ui.backend().id(), content_hash))
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(content_hash) = self.book.content_hash() {
            self.load_torrents(content_hash, cx);
        }
        self.finish_load(self.ui.clone().book_detail(self.book.clone()), cx);
        cx.notify();
    }

    fn load_library(&mut self, content_hash: BookId, cx: &mut Context<Self>) {
        #[cfg(not(target_arch = "wasm32"))]
        self.load_torrents(content_hash, cx);
        let library = self.ui.backend().clone();
        self.finish_load(async move { library.book_detail(content_hash).await }, cx);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_torrents(&mut self, hash: BookId, cx: &mut Context<Self>) {
        let library = self.ui.backend().clone();
        self.torrents_loading = true;
        self.torrent_task = Some(cx.spawn(async move |page, cx| {
            let result = library.book_torrents(hash).await;
            let _ = page.update(cx, |page, cx| {
                page.torrents_loading = false;
                match result {
                    Ok(mut links) => {
                        links.dedup_by(|a, b| a.info_hash == b.info_hash);
                        page.torrents = links;
                    }
                    Err(error) => log::warn!("could not load book torrents: {error}"),
                }
                cx.notify();
            });
        }));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn change_torrent(&mut self, hash: String, keep: Option<bool>, cx: &mut Context<Self>) {
        let library = self.ui.backend().clone();
        let book = self.book.library_file();
        self.torrent_task = Some(cx.spawn(async move |page, cx| {
            let result = match keep {
                Some(keep) => library.keep_torrent_seeding(hash, keep).await,
                None => library.forget_torrent(hash).await,
            };
            let _ = page.update_in(cx, |page, window, cx| match result {
                Ok(()) => {
                    if let Some(book) = book {
                        page.load_torrents(book, cx);
                    }
                    if keep.is_none() {
                        notify_info("torrent-forgotten", "Torrent removed; downloaded files are kept", window, cx);
                    }
                }
                Err(error) => notify_error("torrent-error", user_facing_error(&error, "Could not update this download. Please try again."), window, cx),
            });
        }));
    }

    fn download_subscription(book: &BookCardRow, ui: &LibraryContext, cx: &mut Context<Self>) -> Option<DownloadSubscription> {
        let hash = book.library_file()?;
        ui.updates().update(cx, |updates, _| updates.track_download(hash, book.downloaded, book.download_requested));
        Some(ui.subscribe_download(hash, cx, |_: &mut Self, _, cx| cx.notify()))
    }

    fn finish_load(&mut self, load: impl std::future::Future<Output = Result<BookDetail, library_backend::BackendError>> + 'static, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |page, cx| {
            let result = load.await;
            cx.wait_for_update_slot().await;
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(detail) => {
                        // A catalog or feed book may already be, or have just
                        // become, a library book.
                        if detail.book.library_file() != page.book.library_file() {
                            page._download_updates = Self::download_subscription(&detail.book, &page.ui, cx);
                        }
                        page.book = detail.book.clone();
                        page.detail = Some(detail);
                        page.error = None;
                    }
                    Err(error) => page.error = Some(user_facing_error(&error, "Could not load book details. Please try again.").into()),
                }
                cx.notify();
            });
        });
        self.request = Some(task);
    }

    /// Adds the catalog or feed book, or an external audiobook from its
    /// source, to the library, then loads the library book's page in its
    /// place, with Read.
    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding || matches!(self.book.source, BookSource::ExternalCatalog { downloadable: false, .. }) {
            return;
        }
        let book = self.book.clone();
        let library = self.ui.backend().clone();
        let app = self.ui.app().clone();
        self.adding = true;
        cx.spawn_in(window, async move |page, cx| {
            let result = import_catalog_book_with_progress(&app, &library, book, ROOT_DIR_ID, Some("Added to library"), cx).await;
            let _ = page.update_in(cx, |page, _, cx| {
                page.adding = false;
                if let Ok(content_hash) = result {
                    page.load_library(content_hash, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens the store page that sells the feed book.
    fn buy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let BookSource::Opds { entry, .. } = self.book.source else { return };
        let (app, library_id) = (self.ui.app().clone(), *self.ui.backend().id());
        cx.spawn_in(window, async move |page, cx| {
            if let Err(error) = app.buy_opds_book(entry, library_id).await {
                let _ = page.update_in(cx, |_, window, cx| notify_error("book-buy-error", format!("Could not open the store. {}", user_facing_error(&error, "Please try again.")), window, cx));
            }
        })
        .detach();
    }

    /// The page's own cover, at the resolution a page-sized cover needs rather
    /// than the one a card in a grid does.
    fn load_cover(&mut self, cx: &mut Context<Self>) {
        // With "generated covers only", no art is read or decoded at all.
        if crate::stores::generated_covers_only(cx) {
            self._cover_task = None;
            self.cover = DetailCover::Missing;
            return;
        }
        self.cover = DetailCover::Loading(self.cover.loaded().cloned());
        let generation = self.cover_generation;
        let title = self.book.title.clone();
        let load = self.ui.clone().book_cover(self.book.source, ThumbnailResolution::HighDensity);
        let decoder = cx.background_executor().clone();
        self._cover_task = Some(cx.spawn(async move |page, cx| {
            let cover = match load.await {
                Ok(Some(bytes)) => decoder.spawn(async move { loaded_cover_from_jpeg(bytes) }).await.map(Some).ok_or_else(|| library_backend::BackendError::message("cover is not a readable JPEG")),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            let _ = page.update(cx, |page, cx| {
                if generation != page.cover_generation {
                    return;
                }
                let cover = match cover {
                    Ok(cover) => cover,
                    Err(error) => {
                        log::error!("failed to load the cover for book {title:?}: {error}");
                        None
                    }
                };
                if let Some(cover) = &cover {
                    track_image(cover, cx);
                }
                page.cover = cover.or_else(|| page.cover.loaded().cloned()).map_or(DetailCover::Missing, DetailCover::Loaded);
                cx.notify();
            });
        }));
    }

    fn open(&mut self, target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(content_hash) = self.book.library_file() else { return };
        self.ui.open_book_at(content_hash, self.book.title.clone(), target, window, cx);
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let Some(content_hash) = self.book.library_file() else { return };
        if !matches!(self.ui.updates().read(cx).download_state(content_hash), Some(DownloadState::NotDownloaded)) || self.download_requested_by_page {
            return;
        }
        self.download_requested_by_page = true;
        let library = self.ui.backend().clone();
        cx.spawn(async move |page, cx| {
            let result = library.download_book(content_hash).await;
            let _ = page.update_in(cx, |page, window, cx| {
                page.download_requested_by_page = false;
                if let Err(error) = result {
                    log::warn!("Could not download library book {content_hash}: {error}");
                    notify_error("book-download-error", book_transfer_error_message(error.failure()), window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn toggle_entry(&mut self, target: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(target) {
            self.expanded.insert(target.to_owned());
        }
        cx.notify();
    }
}

fn download_action_label(state: &DownloadState) -> SharedString {
    match state {
        DownloadState::Queued => "Waiting to download…".into(),
        DownloadState::Downloading(progress) if progress.value() > 0.0 => format!("Downloading… {:.0}%", progress.value() * 100.0).into(),
        DownloadState::Downloading(_) => "Starting download…".into(),
        DownloadState::Downloaded | DownloadState::NotDownloaded => "Download".into(),
    }
}

/// One entry, flattened for drawing, with the depth it was found at.
struct DrawnEntry<'a> {
    entry: &'a BookTocEntry,
    depth: usize,
}

/// Walks the navigation tree into the list actually drawn: an unopened entry
/// keeps its children to itself.
fn drawn_entries<'a>(entries: &'a [BookTocEntry], expanded: &HashSet<String>, depth: usize, drawn: &mut Vec<DrawnEntry<'a>>) {
    for entry in entries {
        drawn.push(DrawnEntry { entry, depth });
        if expanded.contains(&entry.target) {
            drawn_entries(&entry.children, expanded, depth + 1, drawn);
        }
    }
}

/// "12 h 41 min", for a fact read at a glance rather than a clock read against a
/// playhead — the player's `0:41:05` answers a different question.
pub(crate) fn spoken_duration(duration_ms: u64) -> String {
    let minutes = duration_ms / 60_000;
    let hours = minutes / 60;
    let minutes = minutes % 60;
    match (hours, minutes) {
        // Rounded up rather than down, so a chapter that plays for forty
        // seconds is not described as taking no time at all.
        (0, 0) => "1 min".to_owned(),
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

/// How long each chapter runs, from where the next one starts. An audiobook's
/// navigation stores positions, not lengths, and the last chapter's end is the
/// book's.
///
/// Keyed by target rather than returned in order, because the list that asks is
/// the *drawn* list — flattened, and shorter than the tree whenever anything is
/// collapsed. Position in one is not position in the other.
fn chapter_durations(entries: &[BookTocEntry], total_ms: Option<u64>) -> HashMap<&str, SharedString> {
    let positions = entries.iter().map(|entry| book_model::audiobook_toc_position(&entry.target)).collect::<Vec<_>>();
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let start = positions[index]?;
            let end = match positions.get(index + 1) {
                Some(Some(next)) => *next,
                _ => total_ms?,
            };
            Some((entry.target.as_str(), SharedString::from(spoken_duration(end.saturating_sub(start)))))
        })
        .collect()
}

impl Render for BookDetailPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.cover_visibility.track(window);
        if matches!(self.cover, DetailCover::NeedsReload) {
            self.load_cover(cx);
        }
        let page = cx.entity();
        // Build at the actual pane width in this frame. No stored measurement
        // or extra invalidation is needed when a split or window changes size.
        canvas(
            move |bounds, window, cx| {
                let compact = components::WindowWidthClass::for_window(window).is_compact();
                let mut body = page.update(cx, |page, cx| page.render_pane(f32::from(bounds.size.width), f32::from(bounds.size.height), f32::from(window.rem_size()), compact, cx));
                body.prepaint_as_root(bounds.origin, bounds.size.map(AvailableSpace::Definite), window, cx);
                body
            },
            |_, mut body, window, cx| body.paint(window, cx),
        )
        .size_full()
        .min_w_0()
        .min_h_0()
    }
}

impl BookDetailPage {
    fn show_description(&mut self, _: &components::ShowBookDescription, window: &mut Window, cx: &mut Context<Self>) {
        self.open_information_sheet(false, window, cx);
    }

    fn show_contents(&mut self, _: &components::ShowBookContents, window: &mut Window, cx: &mut Context<Self>) {
        self.open_information_sheet(true, window, cx);
    }

    fn open_information_sheet(&mut self, contents: bool, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.entity();
        window.open_sheet_at(Placement::Bottom, cx, move |sheet, window, cx| {
            let theme = components::browser_theme(cx);
            let safe_area = window.insets().effective();
            let (title, content) = page.update(cx, |page, cx| {
                let audiobook = page.book.format_category == LibraryFileTypeFilter::Audiobook;
                let title = if contents { if audiobook { "Chapters" } else { "Contents" } } else { "Description" };
                let content = if contents {
                    page.detail.clone().filter(|detail| !detail.toc.is_empty()).map(|detail| {
                        let locator = page.locator();
                        let position = page.ui.now_playing().read(cx).book.as_ref().filter(|playing| Some(&playing.locator) == locator.as_ref()).and_then(|playing| (playing.position)(cx)).map(|(position, _)| position);
                        page.render_contents(&detail, audiobook, position, false, theme, cx)
                    })
                } else {
                    page.detail.as_ref().and_then(|detail| components::book_detail_description(&detail.description, theme, cx)).map(IntoElement::into_any_element)
                };
                let empty = if contents { "No contents available." } else { "No description available." };
                (title, content.unwrap_or_else(|| components::muted_line(empty, theme).into_any_element()))
            });
            sheet.title(title).size((window.viewport_size().height - safe_area.top) * 0.75).resizable(false).bg(theme.page_bg).p_0().child(
                div()
                    .debug_selector(move || if contents { "book-contents-sheet".into() } else { "book-description-sheet".into() })
                    .flex_none()
                    .w_full()
                    .pt(px(components::SPACE_MD))
                    .pb(safe_area.bottom + px(components::SPACE_MD))
                    .pl(safe_area.left + px(components::SPACE_MD))
                    .pr(safe_area.right + px(components::SPACE_MD))
                    .child(content),
            )
        });
    }

    fn render_pane(&mut self, width: f32, height: f32, rem_size: f32, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = components::browser_theme(cx);
        let back = (cfg!(feature = "kobo") || !components::has_native_back_button()).then(|| components::book_detail_back("book-detail-back", theme).on_click(cx.listener(|_, _, _, cx| cx.emit(CloseBookDetail))));
        let page = cx.entity();
        let loaded = self.detail.is_some() && !matches!(self.cover, DetailCover::Loading(_));
        #[cfg(not(target_arch = "wasm32"))]
        let loaded = loaded && !self.torrents_loading;
        self.ready |= loaded && width > 0.0;
        if !self.ready {
            let label = if self.error.is_some() { "Could not load book details" } else { "Loading book…" };
            let body = components::book_detail_page()
                .track_focus(&self.focus)
                .when_some(back, |body, back| body.child(components::book_detail_measure().child(back)))
                .child(div().flex_1().w_full().flex().items_center().justify_center().child(components::browser_loading_message(label, theme)))
                .into_any_element();
            return with_retry_banner(body, self.error.clone(), "retry-book-detail", theme, move |_, _, cx| page.update(cx, |page, cx| page.load(cx)));
        }

        // The initial requests are complete before the book is displayed.
        let book = self.book.clone();
        let detail = self.detail.clone();

        // Columns only when there is something to put beside the player.
        let has_sections = detail.as_ref().is_some_and(|detail| !detail.toc.is_empty() || !detail.description.trim().is_empty());
        let layout = if width >= BOOK_DETAIL_SIDEBAR_BREAKPOINT && has_sections {
            DetailLayout::Columns
        } else if width >= BOOK_DETAIL_STACKED_BREAKPOINT {
            DetailLayout::Hero
        } else {
            DetailLayout::Stacked
        };
        let beside_cover = layout == DetailLayout::Hero;
        let large_title = !beside_cover || width >= BOOK_DETAIL_LARGE_TITLE_WIDTH;

        let audiobook = book.format_category == LibraryFileTypeFilter::Audiobook;
        let mobile = compact && layout == DetailLayout::Stacked && !cfg!(feature = "kobo");
        let page_padding = if mobile && width < 360.0 { components::SPACE_MD } else { components::SPACE_LG };
        let back_size = components::topbar_action_height_rem() * rem_size;
        // The book that is playing: its controls, and where playback is now.
        // The library's saved position lags behind, so its cover bar and
        // chapter mark follow the player instead.
        let locator = self.locator();
        let playing = self.ui.now_playing().read(cx).book.as_ref().filter(|playing| Some(&playing.locator) == locator.as_ref()).map(|playing| (playing.controls.clone(), playing.position.clone()));
        let listening = playing.as_ref().and_then(|(_, position)| position(cx));
        let progress = listening.map(|(_, progress)| progress).unwrap_or(book.progress);
        let cover_gutter = if back.is_some() || (mobile && playing.is_some()) { back_size + components::SPACE_SM } else { 0.0 };

        let cover = (if beside_cover { components::book_detail_cover_slot() } else { components::book_detail_sidebar_cover_slot() })
            .debug_selector(|| "book-detail-cover".into())
            .when(layout == DetailLayout::Stacked, |cover| cover.max_w(px((width - page_padding * 2.0 - cover_gutter * 2.0).max(0.0))))
            .when(mobile, |cover| {
                let aspect = if audiobook { 1.0 } else { self.cover.loaded().map(|cover| cover_aspect_ratio(cover)).unwrap_or(components::BOOK_COVER_ASPECT_RATIO) };
                cover.w(px((width - components::SPACE_LG * 2.0).min(height * 0.36 * aspect).max(0.0)))
            })
            .id("book-detail-open-cover")
            .when(book.library_file().is_some(), |cover| cover.cursor_pointer().on_click(cx.listener(|page, _, window, cx| page.open(None, window, cx))))
            .child(match self.cover.loaded().cloned() {
                Some(cover) => components::book_cover( cover_aspect_ratio(&cover)).child(components::book_cover_image(format!("book-detail-cover-{}", book.key()), cover, 1.0, false, theme)),
                None if matches!(self.cover, DetailCover::Missing) => components::book_cover_generated(book.title.clone(), book.subtitle.as_deref(), &book.author, generated_cover_variant(book.key())),
                None => components::book_cover( if audiobook { 1.0 } else { components::BOOK_COVER_ASPECT_RATIO }),
            })
            // Show the cover's progress bar only after the book has been started.
            .children(components::book_cover_progress(progress, theme))
            .when(audiobook, |cover| cover.child(components::book_cover_audiobook_length(book.audiobook_duration_ms, theme)));

        let headline = if beside_cover { components::book_detail_headline() } else { components::book_detail_sidebar_headline() };
        let mut headline = headline.child(components::book_detail_title(book.title.clone(), book.subtitle.as_deref(), large_title, theme));

        // The card knows one author line; the request knows who each of them is
        // and where their page lives. Until it lands the line is stated rather
        // than linked, so the byline neither appears late nor moves when it
        // becomes clickable.
        match detail.as_ref().filter(|detail| !detail.authors.is_empty()) {
            Some(detail) => {
                let mut byline = components::book_detail_authors(theme);
                for author in &detail.authors {
                    let open = crate::OpenAuthor { author: author.id, name: author.name.clone() };
                    byline = byline.child(components::book_detail_name_link(format!("book-detail-author-{}", author.id), author.name.clone(), theme).min_w_0().max_w_full().on_click(move |_, window, cx| {
                        if !window.has_text_selection(cx) {
                            window.dispatch_action(Box::new(open.clone()), cx);
                        }
                    }));
                }
                headline = headline.child(byline);
            }
            None if !book.author.trim().is_empty() => {
                headline = headline.child(components::book_detail_authors(theme).child(div().min_w_0().max_w_full().child(components::book_detail_text("book-detail-author", book.author.clone()))));
            }
            None => {}
        }

        for credit in detail.iter().flat_map(|detail| &detail.credits) {
            let role = match credit.role.as_str() {
                "nrt" => "Narrated by",
                _ => continue,
            };
            headline = headline.child(components::book_detail_credit_line(Some(role), theme).id(format!("book-detail-credit-row-{}-{}", credit.role, credit.id)).child(components::book_detail_name_link(
                format!("book-detail-credit-{}-{}", credit.role, credit.id),
                credit.name.clone(),
                theme,
            )));
        }

        // The year it was published, then how long it runs and how many
        // chapters, for a book where that is a question. Nothing about the
        // file — neither what kind it is nor when it arrived, which are facts
        // about the library rather than about the book. An audiobook with
        // chapters listed heads that list with the last two instead.
        // The year is bare rather than "Published 1998": for an ebook edition
        // it can be the edition's year, not the work's.
        let chapter_list = audiobook && detail.as_ref().is_some_and(|detail| !detail.toc.is_empty());
        let mut facts = Vec::new();
        if let Some(year) = detail.as_ref().and_then(|detail| detail.year) {
            facts.push(SharedString::from(year.to_string()));
        }
        if !chapter_list {
            if let Some(duration) = book.audiobook_duration_ms {
                facts.push(SharedString::from(spoken_duration(duration)));
            }
            if let Some(chapters) = detail.as_ref().and_then(|detail| detail.audiobook_chapter_count) {
                facts.push(SharedString::from(format!("{chapters} chapters")));
            }
        }
        if !facts.is_empty() {
            headline = headline.child(components::book_detail_facts(facts, theme).id("book-detail-metadata"));
        }

        // Where the reader is and what they can do about it, beside the cover
        // rather than under the hero: those two lines are what the page is for,
        // and a reader who opened it to carry on reading should not have to look
        // past the cover to find them.
        // `progress` is already a percentage, as the grid's bar has always read
        // it. Multiplying it by a hundred again put "2201%" beside the title of
        // a book a fifth of the way through.
        let position = book.library_file().and_then(|_| {
            let entry = detail.as_ref().and_then(|detail| detail.current_entry.clone()).filter(|entry| !entry.trim().is_empty()).map(SharedString::from);
            let percentage = (book.progress > 0.0).then(|| SharedString::from(format!("{}%", book.progress.round() as i32)));
            (entry.is_some() || percentage.is_some()).then(|| components::book_detail_position(entry, percentage, theme))
        });

        let download_state = book.library_file().and_then(|content_hash| self.ui.updates().read(cx).download_state(content_hash)).unwrap_or(DownloadState::Queued);
        let downloaded = matches!(download_state, DownloadState::Downloaded);
        let downloading = matches!(download_state, DownloadState::Queued | DownloadState::Downloading(_));
        let open_icon: gpui_component::Icon = if audiobook { components::Headphones.into() } else { IconName::BookOpen.into() };
        let action_padding = if width >= BOOK_DETAIL_LARGE_TITLE_WIDTH { components::SPACE_SM } else { components::SPACE_MD };
        let mut actions = components::book_detail_actions().child(
            components::primary_action_button("book-detail-open", if audiobook { "Listen" } else { "Read" }, open_icon, false).debug_selector(|| "book-detail-open".into()).px(gpui::px(action_padding)).on_click({
                let page = page.clone();
                move |_, window, cx| page.update(cx, |page, cx| page.open(None, window, cx))
            }),
        );
        let offline_download = cfg!(not(target_arch = "wasm32")) || book.library_file().is_some_and(|id| id.is_external());
        if !downloaded && offline_download {
            let label = download_action_label(&download_state);
            actions = actions.child(components::secondary_action_button("book-detail-download", label, IconName::ArrowDown, downloading).px(gpui::px(action_padding)).on_click({
                let page = page.clone();
                move |_, _, cx| page.update(cx, |page, cx| page.download(cx))
            }));
        }
        // Hosted catalog books, open feed books and external audiobooks can
        // be added. Other external entries keep reading on the source website.
        match book.source {
            BookSource::ExternalCatalog { downloadable: false, .. } => {
                let url = detail.as_ref().and_then(|detail| detail.external_url.clone());
                let label = if url.as_deref().is_some_and(|url| url.starts_with("https://litteraturbanken.se/")) { "Open on Litteraturbanken" } else { "Open at source" };
                actions = components::book_detail_actions().child(components::primary_action_button("book-detail-external", label, IconName::ExternalLink, url.is_none()).px(gpui::px(action_padding)).on_click(move |_, _, cx| {
                    if let Some(url) = &url {
                        cx.open_url(url);
                    }
                }));
            }
            BookSource::Library { .. } => {}
            // Sold, not offered: the store's page opens in a store window,
            // and what is bought there goes into the library's root.
            BookSource::Opds { downloadable: false, buyable: true, .. } => {
                actions = components::book_detail_actions().child(components::primary_action_button("book-detail-buy", "Buy", components::buy_glyph(), false).px(gpui::px(action_padding)).on_click({
                    let page = page.clone();
                    move |_, window, cx| page.update(cx, |page, cx| page.buy(window, cx))
                }));
            }
            BookSource::Opds { downloadable: false, .. } => {
                actions = components::book_detail_actions();
                headline = headline.child(components::book_detail_facts(vec![SharedString::from("Not offered for download")], theme).id("book-detail-status"));
            }
            BookSource::Catalog { .. } | BookSource::Opds { downloadable: true, .. } | BookSource::ExternalCatalog { downloadable: true, .. } => {
                actions = components::book_detail_actions().child(
                    components::primary_action_button("book-detail-add", if self.adding { "Adding…" } else { "Add to library" }, components::download_glyph(), self.adding).px(gpui::px(action_padding)).on_click({
                        let page = page.clone();
                        move |_, window, cx| page.update(cx, |page, cx| page.add(window, cx))
                    }),
                );
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        for (index, torrent) in self.torrents.iter().enumerate() {
            if !cfg!(feature = "kobo") {
                let hash = torrent.info_hash.clone();
                let keep = !torrent.keep_seeding;
                let target = page.clone();
                actions = actions.child(
                    components::secondary_action_button(("book-torrent-seeding", index), if keep { "Keep seeding torrent" } else { "Stop seeding torrent" }, IconName::ArrowUp, false)
                        .on_click(move |_, _, cx| target.update(cx, |page, cx| page.change_torrent(hash.clone(), Some(keep), cx))),
                );
            }
            let hash = torrent.info_hash.clone();
            let target = page.clone();
            actions = actions
                .child(components::secondary_action_button(("book-torrent-forget", index), "Remove torrent", IconName::Close, false).on_click(move |_, _, cx| target.update(cx, |page, cx| page.change_torrent(hash.clone(), None, cx))));
        }
        // The book that is playing is controlled here rather than offered: its
        // controls stand where Listen and the reading position would. Beside
        // the cover they would get half the row, so there they go under both.
        let player = playing.map(|(controls, _)| controls);
        if player.is_none() {
            headline = headline.children(position).child(actions);
        }
        let has_player = player.is_some();
        let player_under_hero = if beside_cover || mobile {
            player
        } else {
            headline = headline.children(player);
            None
        };
        if let DownloadState::Downloading(progress) = download_state
            && progress.value() > 0.0
        {
            headline = headline.child(components::progress_track(theme).w(gpui::px(240.0)).max_w_full().child(components::progress_fill(progress.value(), theme)));
        }

        // Three columns show the contents and the description side by side;
        // stacked, in two columns or fewer, either can be folded away.
        let three_columns = layout == DetailLayout::Columns && width >= BOOK_DETAIL_THREE_COLUMN_BREAKPOINT && detail.as_ref().is_some_and(|detail| !detail.toc.is_empty() && !detail.description.trim().is_empty());
        let collapsible = !three_columns;
        // Empty contents and descriptions do not occupy a section.
        let contents = detail.as_ref().filter(|detail| !mobile && !detail.toc.is_empty()).map(|detail| self.render_contents(detail, audiobook, listening.map(|(position, _)| position), collapsible, theme, cx));
        // A description containing only markup is empty.
        let description = detail.as_ref().filter(|_| !mobile).and_then(|detail| components::book_detail_description(&detail.description, theme, cx)).map(|description| {
            let open = !collapsible || self.description_open;
            let heading = components::book_detail_section_heading("Description", None::<SharedString>, collapsible.then_some(open), theme).id("book-detail-description-heading").when(collapsible, |heading| {
                heading.on_click(cx.listener(|page, _, window, cx| {
                    if !window.has_text_selection(cx) {
                        page.description_open = !page.description_open;
                        cx.notify();
                    }
                }))
            });
            components::book_detail_section().child(heading).when(open, |section| section.child(description))
        });
        // A description of markup alone has no section after all.
        let three_columns = three_columns && description.is_some();

        // On desktop each column scrolls on its own and the page does not; the
        // narrower pages scroll as one.
        let body = if mobile {
            let introduction = components::column(components::SPACE_MD)
                .w_full()
                .items_center()
                .debug_selector(|| "book-detail-stacked".into())
                .child(components::book_detail_cover_row(back, cover).when(has_player, |row| row.child(div().absolute().top_0().right_0().child(components::book_detail_close_player_button()))))
                .child(headline);
            components::now_playing_page(theme)
                .debug_selector(|| "book-detail-mobile".into())
                .min_w_0()
                .min_h_0()
                .track_focus(&self.focus)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|page, _, window, cx| {
                        if !page.focus.contains_focused(window, cx) {
                            page.focus.focus(window, cx);
                        }
                    }),
                )
                .on_action(cx.listener(Self::show_description))
                .on_action(cx.listener(Self::show_contents))
                .child(
                    components::now_playing_body()
                        .pt(px(components::SPACE_SM))
                        .when(width < 360.0, |body| body.px(px(components::SPACE_MD)))
                        .child(div().flex_1().min_h_0().w_full().child(introduction).overflow_y_scrollbar())
                        .children(player_under_hero)
                        .when(!has_player, |body| body.child(components::book_detail_mobile_tools(theme))),
                )
                .into_any_element()
        } else if layout == DetailLayout::Columns {
            let leading_margin = components::book_detail_page_margin()
                .relative()
                .when(back.is_some(), |margin| margin.min_w(px(components::BOOK_DETAIL_PAGE_MARGIN + cover_gutter)))
                .children(back.map(|back| div().absolute().top(px(components::SPACE_LG)).right(px(components::SPACE_SM)).child(back)));
            let sidebar = components::book_detail_sidebar_column().child(components::book_detail_sidebar().child(cover).child(headline.flex_none()).overflow_y_scrollbar());
            let mut columns = components::book_detail_columns_page().debug_selector(|| "book-detail-columns".into()).track_focus(&self.focus).child(leading_margin).child(sidebar);
            if three_columns {
                let contents = components::book_detail_column_body().children(contents).overflow_y_scrollbar();
                let description = components::book_detail_column_body().children(description).overflow_y_scrollbar();
                columns = columns
                    .child(components::book_detail_column_gap(theme))
                    .child(components::book_detail_column().child(contents))
                    .child(components::book_detail_column_gap(theme))
                    .child(components::book_detail_column().child(description));
            } else {
                let reading = components::book_detail_column_body().children(contents).children(description).overflow_y_scrollbar();
                columns = columns.child(components::book_detail_column_gap(theme)).child(components::book_detail_column().child(reading));
            }
            columns.child(components::book_detail_page_margin()).into_any_element()
        } else {
            let introduction = match layout {
                DetailLayout::Stacked => components::book_detail_stacked_introduction().debug_selector(|| "book-detail-stacked".into()).child(components::book_detail_cover_row(back, cover)).child(headline),
                _ => components::book_detail_introduction().child(components::book_detail_hero().debug_selector(|| "book-detail-hero".into()).children(back).child(cover).child(headline)).children(player_under_hero),
            };
            let content = components::book_detail_measure().child(introduction).child(components::book_detail_sections().children(contents).children(description));
            components::book_detail_page().track_focus(&self.focus).child(content).overflow_y_scrollbar().into_any_element()
        };

        // A failed request loses the credits and the contents —
        // not the book. Everything the card already knew is on screen and stays
        // there, with the banner over it saying what is missing.
        with_retry_banner(body, self.error.clone(), "retry-book-detail", theme, move |_, _, cx| page.update(cx, |page, cx| page.load(cx)))
    }
}

impl BookDetailPage {
    /// Called only for a book that has navigation stored: a page with no
    /// contents has no Contents section at all, rather than a heading over a
    /// sentence explaining the library to the reader.
    /// `listening` is where the player is, when this is the book playing.
    /// `collapsible`: the heading folds the list away, as it does in two
    /// columns and narrower.
    fn render_contents(&self, detail: &BookDetail, audiobook: bool, listening: Option<Duration>, collapsible: bool, theme: components::BrowserTheme, cx: &mut Context<Self>) -> AnyElement {
        let durations = if audiobook { chapter_durations(&detail.toc, detail.book.audiobook_duration_ms) } else { HashMap::new() };
        let mut drawn = Vec::new();
        drawn_entries(&detail.toc, &self.expanded, 0, &mut drawn);
        // Number against the full TOC, including entries hidden under collapsed
        // parents, so expanding a part does not renumber later chapters.
        let mut ordered = Vec::new();
        fn visit<'a>(entries: &'a [BookTocEntry], ordered: &mut Vec<&'a BookTocEntry>) {
            for entry in entries {
                ordered.push(entry);
                visit(&entry.children, ordered);
            }
        }
        visit(&detail.toc, &mut ordered);
        // The entry the reader is inside, and only that one: a parent is not
        // partly read, because a navigation document says where a section
        // starts and never how far into it anyone has got. Matched on the
        // target rather than the wording — two entries can be called the same
        // thing, and `current_entry` is the wording. While the book plays,
        // it is the last chapter starting at or before the player's position.
        let playing_target = listening.and_then(|position| {
            let position = position.as_millis() as u64;
            ordered.iter().filter(|entry| book_model::audiobook_toc_position(&entry.target).is_some_and(|start| start <= position)).last().map(|entry| entry.target.as_str())
        });
        let marked = playing_target.or(detail.current_target.as_deref());
        let locator = self.locator();
        let read_status = self.ui.now_playing().read(cx).book.as_ref().filter(|playing| Some(&playing.locator) == locator.as_ref()).map(|playing| (playing.chapter_read_status)(cx)).unwrap_or_else(|| detail.chapter_read_status.clone());
        // A closed part holding the current entry carries the mark itself, so
        // where the reader is never disappears into it.
        fn holds(entry: &BookTocEntry, target: &str) -> bool {
            entry.children.iter().any(|child| child.target == target || holds(child, target))
        }

        let page = cx.entity();
        let can_open = self.book.library_file().is_some();
        // Every entry is numbered by its place in reading order, so the number
        // stays put when a part is opened or closed. The caret column is kept
        // only in a contents that has parts at all.
        let number_digits = components::contents_digits(ordered.len());
        let nested = detail.toc.iter().any(|entry| !entry.children.is_empty());
        let mut panel = components::contents_panel(theme);
        for (index, DrawnEntry { entry, depth }) in drawn.into_iter().enumerate() {
            let expanded = self.expanded.contains(&entry.target);
            let row = components::ContentsRow {
                depth,
                has_children: !entry.children.is_empty(),
                expanded,
                marked: marked.is_some_and(|marked| marked == entry.target || (!expanded && holds(entry, marked))),
                read: read_status.completed.contains(&entry.target),
                focused: false,
                number: ordered.iter().position(|ordered| std::ptr::eq(*ordered, entry)).map(|position| position + 1),
                number_digits,
            };
            // The chevron opens the entry and the row goes to it, so the two
            // clicks have to be separable. Attach the toggle directly to the
            // disclosure, and leave leaf entries transparent to the row click.
            let disclosure = nested.then(|| {
                components::contents_disclosure(format!("book-detail-disclosure-{index}"), row, theme).when(row.has_children, |disclosure| {
                    let page = page.clone();
                    let target = entry.target.clone();
                    disclosure.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).on_click(move |_, _, cx| page.update(cx, |page, cx| page.toggle_entry(&target, cx)))
                })
            });
            let amount = durations.get(entry.target.as_str()).cloned();
            let reset_page = page.clone();
            let reset_target = entry.target.clone();
            let reset: components::ContentsReadReset = std::rc::Rc::new(move |_, _, cx| reset_page.update(cx, |page, cx| page.reset_contents_read_status(Some(reset_target.clone()), cx)));
            panel = panel.child(
                components::contents_row(format!("book-detail-entry-{index}"), row, disclosure, components::book_detail_text(format!("book-detail-entry-title-{index}"), entry.title.clone()).selectable(false), amount, Some(reset), theme)
                    .when(can_open, |row| {
                        row.on_click({
                            let page = page.clone();
                            let target = entry.target.clone();
                            move |_, window, cx| {
                                if !window.has_text_selection(cx) {
                                    if window.has_active_sheet(cx) {
                                        window.close_sheet(cx);
                                    }
                                    page.update(cx, |page, cx| page.open(Some(target.clone()), window, cx));
                                }
                            }
                        })
                    })
                    .when(!can_open, |row| row.cursor_default()),
            );
        }
        // An audiobook's heading says how many chapters and how long, on the
        // heading's own line.
        let summary = audiobook.then(|| {
            let chapters = detail.audiobook_chapter_count.unwrap_or(detail.toc.len());
            match detail.book.audiobook_duration_ms {
                Some(duration) => format!("{chapters} chapters · {}", spoken_duration(duration)),
                None => format!("{chapters} chapters"),
            }
        });
        let open = !collapsible || self.contents_open;
        let heading = components::book_detail_section_heading(if audiobook { "Chapters" } else { "Contents" }, summary, collapsible.then_some(open), theme).id("book-detail-contents-heading").when(collapsible, |heading| {
            heading.on_click(cx.listener(|page, _, window, cx| {
                if !window.has_text_selection(cx) {
                    page.contents_open = !page.contents_open;
                    cx.notify();
                }
            }))
        });
        let heading = components::row(components::SPACE_SM).child(heading.flex_1()).when(can_open, |heading| {
            heading.child(components::topbar_action_button("reset-chapter-read-status", "Reset read status", IconName::Undo2, theme).on_click(cx.listener(|page, _, _, cx| page.reset_contents_read_status(None, cx))))
        });
        components::book_detail_section().child(heading).when(open, |section| section.child(panel)).into_any_element()
    }
}

impl Focusable for BookDetailPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use crate::library::services::feed_detail;
    use crate::navigation::LibrarySession;
    use gpui::{Pixels, RenderImage, StyleRefinement, TestAppContext, VisualTestContext, px, size};
    use gpui_component::Root;
    use std::{cell::Cell, rc::Rc, sync::Arc};

    struct Neighbor(Rc<Cell<usize>>);

    impl Render for Neighbor {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.set(self.0.get() + 1);
            div().size_full()
        }
    }

    struct DetailFixture {
        page: Entity<BookDetailPage>,
        neighbor: Entity<Neighbor>,
        width: Pixels,
    }

    impl Render for DetailFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().flex().child(div().w(self.width).h_full().flex_none().child(self.page.clone())).child(div().w(px(50.0)).h_full().child(self.neighbor.clone().cached(StyleRefinement::default().size_full())))
        }
    }

    struct DetailSheetFixture(Entity<BookDetailPage>);

    impl Render for DetailSheetFixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone()).children(Root::render_sheet_layer(window, cx).map(|sheet| gpui::deferred(sheet).with_priority(2)))
        }
    }

    #[gpui::test]
    async fn reader_entry_preserves_displayed_details_and_releases_hidden_details(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let book = BookCardRow {
            source: BookSource::Opds { source: uuid::Uuid::new_v4(), entry: CardKey([9; 32]), downloadable: true, buyable: false },
            title: "A feed book".into(),
            subtitle: None,
            author: "Writer".into(),
            progress: 0.0,
            downloaded: false,
            download_requested: false,
            format_category: LibraryFileTypeFilter::Book,
            audiobook_duration_ms: None,
            added_at: 0,
            source_directory: None,
        };
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(2, 3))]));
        let displayed = BookDetailPage::create(book.clone(), Some(image.clone()), library.clone(), cx);
        let hidden = BookDetailPage::create(book, Some(image.clone()), library, cx);
        for page in [&displayed, &hidden] {
            cx.condition(page, |page, _| matches!(page.cover, DetailCover::Loaded(_))).await;
        }
        let window = cx.open_window(size(px(700.0), px(600.0)), |_, _| DetailSheetFixture(displayed.clone()));
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.update(|window, cx| { let _ = window.draw(cx); });
        cx.update(crate::library::evict_covers);
        displayed.read_with(cx, |page, _| {
            assert!(Arc::ptr_eq(page.cover.loaded().expect("displayed detail must keep its cover"), &image));
            assert_eq!(page.cover_generation, 0);
        });
        hidden.read_with(cx, |page, _| {
            assert!(matches!(page.cover, DetailCover::NeedsReload));
            assert!(page._cover_task.is_none());
        });
        visual.update(|window, cx| { window.replace_root(cx, |_, _| gpui::Empty); });
        visual.update(|window, cx| { let _ = window.draw(cx); });
        cx.update(crate::library::evict_covers);
        displayed.read_with(cx, |page, _| assert!(matches!(page.cover, DetailCover::NeedsReload), "a detail hidden after being displayed must also release its artwork"));
    }

    #[gpui::test]
    async fn failed_cover_refresh_retains_the_image_until_eviction(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let book = BookCardRow {
            // An unknown feed fails locally, without making a network request.
            source: BookSource::Opds { source: uuid::Uuid::new_v4(), entry: CardKey([9; 32]), downloadable: true, buyable: false },
            title: "A feed book".into(),
            subtitle: None,
            author: "Writer".into(),
            progress: 0.0,
            downloaded: false,
            download_requested: false,
            format_category: LibraryFileTypeFilter::Book,
            audiobook_duration_ms: None,
            added_at: 0,
            source_directory: None,
        };
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(image::RgbaImage::new(2, 3))]));
        let page = BookDetailPage::create(book, Some(image.clone()), library, cx);
        page.read_with(cx, |page, _| {
            assert!(matches!(page.cover, DetailCover::Loading(_)));
            assert!(Arc::ptr_eq(page.cover.loaded().unwrap(), &image));
        });
        cx.condition(&page, |page, _| matches!(page.cover, DetailCover::Loaded(_))).await;
        page.read_with(cx, |page, _| assert!(Arc::ptr_eq(page.cover.loaded().unwrap(), &image)));
        cx.update(crate::library::evict_covers);
        page.update(cx, |page, cx| {
            assert!(matches!(page.cover, DetailCover::NeedsReload));
            assert!(page.cover.loaded().is_none());
            assert!(page._cover_task.is_none());
            page.load_cover(cx);
        });
        cx.condition(&page, |page, _| matches!(page.cover, DetailCover::Missing)).await;
        page.update(cx, |page, cx| {
            cx.set_global(crate::stores::GeneratedCoversOnly(true));
            page.load_cover(cx);
            assert!(matches!(page.cover, DetailCover::Missing));
            assert!(page._cover_task.is_none());
        });
    }

    #[gpui::test]
    async fn mobile_book_details_keep_information_in_sheets_without_ebook_player_controls(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let book = BookCardRow {
            source: BookSource::Library { content_hash: BookId::from_bytes([18; 32]) },
            title: "A mobile EPUB".into(),
            subtitle: None,
            author: "Author".into(),
            progress: 25.0,
            downloaded: true,
            download_requested: false,
            format_category: LibraryFileTypeFilter::Book,
            audiobook_duration_ms: None,
            added_at: 0,
            source_directory: None,
        };
        let page = BookDetailPage::create(book.clone(), None, library, cx);
        page.update(cx, |page, _| {
            page.request.take();
            page._cover_task.take();
            page.torrent_task.take();
            let mut detail = feed_detail(book, "<p>A long description.</p>".repeat(60));
            detail.current_entry = Some("Chapter one".into());
            detail.toc.push(BookTocEntry { title: "Chapter one".into(), target: "chapter-one.xhtml".into(), children: Vec::new() });
            page.detail = Some(detail);
            page.cover = DetailCover::Missing;
            page.torrents_loading = false;
        });
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, visual) = cx.add_window_view(|window, cx| {
            let fixture = cx.new(|_| DetailSheetFixture(page.clone()));
            Root::new(fixture, window, cx).bordered(false)
        });
        visual.simulate_resize(size(px(390.0), px(640.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("book-detail-mobile").is_some());
        let cover = visual.debug_bounds("book-detail-cover").expect("the cover remains visible");
        if let Some(back) = visual.debug_bounds("book-detail-back") {
            assert!((back.top() - cover.top()).abs() < px(1.0), "Back must align with the cover's top");
            assert!(back.right() <= cover.left(), "Back must sit beside the cover");
        }
        let read = visual.debug_bounds("book-detail-open").expect("EPUBs retain their Read action");
        assert!((read.center().x - px(195.0)).abs() < px(1.0), "Read must be centred on mobile");
        let progress = visual.debug_bounds("book-detail-position-entry-box").expect("the current chapter must be visible");
        assert!((progress.center().x - px(195.0)).abs() < px(1.0), "the progress percentage must not displace the chapter name");
        assert!(visual.debug_bounds("audiobook-play").is_none(), "EPUBs have no audio transport");
        assert!(visual.debug_bounds("book-detail-description-heading").is_none(), "long descriptions must not extend the mobile page");
        for (button, sheet) in [("book-description", "book-description-sheet"), ("book-contents", "book-contents-sheet")] {
            let bounds = visual.debug_bounds(button).expect("information buttons remain visible without scrolling");
            assert!(bounds.top() >= px(0.0) && bounds.bottom() <= px(640.0));
            visual.simulate_click(bounds.center(), Default::default());
            visual.run_until_parked();
            visual.update(|window, cx| {
                assert!(window.has_active_sheet(cx), "{button} must open its sheet from a pointer click");
                let _ = window.draw(cx);
            });
            assert!(visual.debug_bounds(sheet).is_some(), "{button} must open its sheet from a pointer click");
            visual.update(|window, cx| {
                assert!(window.has_active_sheet(cx));
                assert!(Root::dismiss_active_overlay(window, cx));
            });
            visual.run_until_parked();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            assert!(visual.debug_bounds(sheet).is_none());
        }
    }

    #[gpui::test]
    async fn detail_uses_current_pane_width_without_followup_invalidation(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let book = BookCardRow {
            source: BookSource::Library { content_hash: BookId::from_bytes([15; 32]) },
            title: "Book".into(),
            subtitle: None,
            author: "Author".into(),
            progress: 0.0,
            downloaded: true,
            download_requested: false,
            format_category: LibraryFileTypeFilter::Book,
            audiobook_duration_ms: None,
            added_at: 0,
            source_directory: None,
        };
        let page = BookDetailPage::create(book.clone(), None, library, cx);
        page.update(cx, |page, _| {
            page.request.take();
            page._cover_task.take();
            page.torrent_task.take();
            page.detail = Some(feed_detail(book, "A description".into()));
            page.cover = DetailCover::Missing;
            page.torrents_loading = false;
        });
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _observation = cx.update(|cx| cx.observe(&page, move |_, _| observed.set(observed.get() + 1)));
        let neighbor_renders = Rc::new(Cell::new(0));
        let neighbor = cx.new(|_| Neighbor(neighbor_renders.clone()));
        let root = cx.open_window(size(px(1500.0), px(800.0)), |_, _| DetailFixture { page, neighbor, width: px(450.0) });
        let mut visual = VisualTestContext::from_window(root.into(), cx);
        for (index, (width, selector)) in [(450.0, "book-detail-stacked"), (800.0, "book-detail-hero"), (1000.0, "book-detail-columns"), (1300.0, "book-detail-columns"), (450.0, "book-detail-stacked")].into_iter().enumerate() {
            if index > 0 {
                root.update(cx, |root, _, cx| {
                    root.width = px(width);
                    cx.notify();
                })
                .unwrap();
            }
            // Opening the window and each notified width change draw a frame
            // immediately. Check it before explicitly drawing another one.
            assert!(visual.debug_bounds(selector).is_some(), "the first frame at {width}px must use the right layout");
            let cover = visual.debug_bounds("book-detail-cover").expect("the cover remains visible");
            if let Some(back) = visual.debug_bounds("book-detail-back") {
                assert!((back.top() - cover.top()).abs() < px(1.0), "Back must align with the cover's top at {width}px");
                assert!(back.right() <= cover.left(), "Back must sit beside the cover at {width}px");
            }
            if selector == "book-detail-stacked" {
                let read = visual.debug_bounds("book-detail-open").expect("the Read action remains visible");
                assert!((read.center().x - cover.center().x).abs() < px(1.0), "single-column actions must be centred beneath the cover");
            }
            assert_eq!(notifications.get(), 0, "layout must not invalidate the page again");
            let rendered = neighbor_renders.get();
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
            assert_eq!(neighbor_renders.get(), rendered, "detail layout must not force a window-wide cache refresh");
        }
    }

    #[gpui::test]
    async fn catalog_detail_subscribes_when_loaded_as_a_library_book(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        let hash = BookId::from_bytes([16; 32]);
        let mut book = BookCardRow {
            source: BookSource::Catalog { content_hash: hash },
            title: "Catalog book".into(),
            subtitle: None,
            author: String::new(),
            progress: 0.0,
            downloaded: false,
            download_requested: false,
            format_category: LibraryFileTypeFilter::Book,
            audiobook_duration_ms: None,
            added_at: 0,
            source_directory: None,
        };
        let page = BookDetailPage::create(book.clone(), None, library, cx);
        page.update(cx, |page, cx| {
            page.request.take();
            page._cover_task.take();
            page.torrent_task.take();
            assert!(page._download_updates.is_none());
            book.source = BookSource::Library { content_hash: hash };
            page.finish_load(async move { Ok(feed_detail(book, String::new())) }, cx);
        });
        cx.condition(&page, |page, _| page.book.library_file() == Some(hash)).await;
        let notifications = Rc::new(Cell::new(0));
        let observed = notifications.clone();
        let _observation = cx.update(|cx| cx.observe(&page, move |_, _| observed.set(observed.get() + 1)));
        for state in [DownloadState::Queued, DownloadState::Downloading(library_model::DownloadProgress::ZERO), DownloadState::Downloaded] {
            updates.update(cx, |updates, cx| updates.set_download_state_for_test(hash, state, cx));
        }
        assert_eq!(notifications.get(), 3, "the page must receive progress after its source becomes a library book");
    }
}
