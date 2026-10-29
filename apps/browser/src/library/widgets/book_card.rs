use ui_components::user_facing_error;

use crate::library::pages::browse::profile::Stage;
use crate::services::{BookCommand, book_transfer_error_message, import_catalog_book, notify_error, notify_info, open_book_windows};
use app_preferences::CoverText;
use library_backend::{BackendError, DownloadState, ThumbnailResolution};
use std::collections::HashSet;
use std::future::Future;
use std::mem::discriminant;
use std::rc::Rc;
use std::sync::{Arc, Weak};
use std::time::Duration;

use web_time::Instant;

/// Long enough to read as a settle rather than a flash, short enough that a
/// fast-filling grid does not feel sluggish.
const COVER_FADE_DURATION: Duration = Duration::from_millis(180);

use gpui::prelude::*;
use gpui::{AnyElement, App, AppContext, Context, DismissEvent, Div, Entity, FocusHandle, IntoElement, Pixels, Point, Render, RenderImage, SharedString, StyleRefinement, Subscription, Task, Window, deferred, div, px};
use gpui_component::menu::PopupMenu;
use gpui_component::{Icon, IconName};
use library_model::{BookCardRow, BookDetail, BookSource, CardKey, FINISHED_PROGRESS_THRESHOLD};
use ui_components as components;
use ui_components::Button;

use crate::library::services::{DownloadSubscription, FolderPlacement, LibraryContext, LibraryCoversChanged, initial_download_state};
use sync_common::{BookId, DirId, ROOT_DIR_ID};
use uuid::Uuid;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use super::SelectionAction;
use super::cover_cache::CoverCache;
use super::CoverVisibility;
use super::mobile_context_menu::ContextMenu;
use gpui_image_eviction::{track_image, trim_unused_images_after_transition};

mod cache_scope;
use cache_scope::CacheScope;

pub(crate) type LoadedCover = Arc<RenderImage>;

/// Book lookups are weak; the shared budgeted image cache retains the pixels.
type BrowseCovers = CoverCache<(Uuid, CardKey)>;

impl gpui::Global for BrowseCovers {}

#[derive(Default)]
pub(crate) struct CoversEvicted;

impl gpui::Global for CoversEvicted {}

pub(crate) fn evict_covers(cx: &mut App) {
    // Weak lookups do not retain pixels. Preserve them so other windows can
    // reuse their displayed images; only hidden views cancel their cover work.
    cx.set_global(CoversEvicted);
    cx.defer(|cx| {
        for handle in cx.windows() {
            let _ = handle.update(cx, |_, window, _| {
                trim_unused_images_after_transition(window);
            });
        }
    });
}

/// What a card asks of the page its grid is on.
pub(crate) enum BookCardEvent {
    /// Show the book's own page. The card hands over its row and its decoded
    /// cover rather than being looked up afterwards, and not only to save the
    /// lookup: anything that went back and read the cards while this one is
    /// being activated would read one that is leased, and panic.
    OpenDetail(BookCardRow, Option<LoadedCover>),
    /// Choose a destination for these books. A folder listing supplies their
    /// source; a flat listing lets the backend resolve each source.
    Place(Vec<BookId>, Option<DirId>, FolderPlacement),
    /// Take the book out of the folder it is listed in.
    Remove(sync_common::BookId, DirId),
}

pub(crate) type BookCardEvents = Rc<dyn Fn(BookCardEvent, &mut Window, &mut gpui::App)>;

/// A decoded cover's width over its height, for boxing it at its own
/// proportions instead of the placeholder's.
pub(crate) fn cover_aspect_ratio(image: &RenderImage) -> f32 {
    let size = image.size(0);
    size.width.0.max(1) as f32 / size.height.0.max(1) as f32
}

/// Which generated cover a book with no art gets. Its content hash or
/// source-entry identity keeps the cover consistent across cards and detail
/// pages; both are hashes already, so their first bytes are evenly spread.
pub(crate) fn generated_cover_variant(key: CardKey) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&key.0[..8]);
    u64::from_le_bytes(bytes)
}

pub(crate) fn loaded_cover_from_jpeg(bytes: Vec<u8>) -> Option<LoadedCover> {
    if !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return None;
    }
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes.as_slice()), options);
    let pixels = decoder.decode().ok()?;
    let (width, height) = decoder.dimensions()?;
    if width == 0 || height == 0 {
        return None;
    }
    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    let frame = image::Frame::new(image::RgbaImage::from_raw(width, height, pixels)?);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

#[derive(Clone)]
enum CoverState {
    Pending,
    Loaded(Weak<RenderImage>, ThumbnailResolution),
    Missing,
    Unavailable,
}

impl CoverState {
    fn loaded(&self) -> Option<LoadedCover> {
        match self {
            Self::Loaded(image, _) => image.upgrade(),
            Self::Pending | Self::Missing | Self::Unavailable => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BookCardPresentation {
    Grid,
    Section,
    GraphCover,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BookCardInteraction {
    pub(crate) focused: bool,
    pub(crate) selected: bool,
    pub(crate) marquee: bool,
}

pub(crate) struct BookCardElement {
    card: Entity<BookCardView>,
    presentation: BookCardPresentation,
    interaction: BookCardInteraction,
    _subscription: Subscription,
    #[cfg(test)]
    renders: usize,
}

impl Render for BookCardElement {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        self.card.update(cx, |card, cx| card.render_card(self.presentation, self.interaction, false, window, cx))
    }
}

/// The feed books added to the library this session. The library does not
/// know which feed entry a file came from, so a feed card learns that its
/// book is held only from an add made here; after a restart it offers Add
/// again, and adding it again finds the same file.
#[derive(Default)]
pub(crate) struct AddedFeedBooks(HashSet<library_model::CardKey>);

impl gpui::Global for AddedFeedBooks {}

pub(crate) fn mark_feed_book_added(key: library_model::CardKey, cx: &mut App) {
    cx.default_global::<AddedFeedBooks>().0.insert(key);
}

fn feed_book_added(key: library_model::CardKey, cx: &App) -> bool {
    cx.try_global::<AddedFeedBooks>().is_some_and(|added| added.0.contains(&key))
}

/// A catalog or feed book added this session. The catalog's next listing
/// leaves an added catalog book out; a feed lists it until the session ends.
fn held(book: &BookCardRow, cx: &App) -> bool {
    match book.source {
        BookSource::Catalog { .. } | BookSource::Opds { .. } | BookSource::ExternalCatalog { downloadable: true, .. } => feed_book_added(book.key(), cx),
        BookSource::Library { .. } => true,
        BookSource::ExternalCatalog { downloadable: false, .. } => false,
    }
}

pub(crate) struct BookCardView {
    book: BookCardRow,
    /// When the artwork arrived, which is what the fade is computed from.
    ///
    /// A timestamp rather than an element animation: the paginator repaints its
    /// children on every cursor move, and an animation keyed to an element
    /// restarts each time that element is rebuilt — which is what made covers
    /// flash. Deriving opacity from *when the cover loaded* is unaffected by how
    /// often the card is drawn.
    cover_loaded_at: Option<Instant>,
    cover: CoverState,
    cover_visibility: CoverVisibility,
    cover_generation: u64,
    ui: LibraryContext,
    events: BookCardEvents,
    /// Whether the page offers the folder commands: copying, moving and
    /// removing the book from the folder it is listed in.
    folder_commands: bool,
    copy_from_library: bool,
    cover_task: Option<Task<()>>,
    cover_updates: Option<Subscription>,
    _download_updates: Option<DownloadSubscription>,
    download_task: Option<Task<()>>,
    /// Adding catalog books to the library, while it runs.
    add_task: Option<Task<()>>,
    /// The menu over several selected books.
    context_menu: Option<ContextMenu>,
    /// The book's overlay, which one book opens in place of a context menu.
    overlay: Option<BookOverlay>,
    /// Repaints the card when the "cover text" setting changes, so an
    /// already-visible card hides or shows its title without waiting for
    /// some unrelated event to trigger a redraw.
    _cover_text_updates: Subscription,
    /// Starts over when "generated covers only" changes: switching it on
    /// drops the art, switching it off loads it.
    _generated_covers_updates: Subscription,
    _cover_evictions: Subscription,
    /// Repaints a feed card when a feed book is added, to mark it held.
    _feed_added_updates: Option<Subscription>,
}

/// What a book is, then what can be done with it: opened from a card where a
/// context menu used to be. The card's row draws the header at once; the
/// blurb, the subjects and the linked byline arrive with the book's detail.
struct BookOverlay {
    detail: Option<BookDetail>,
    focus: FocusHandle,
    /// Where the keyboard was, to hand it back on close.
    previous_focus: Option<FocusHandle>,
    _load: Task<()>,
}

/// The overlay's commands, in the order it shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayCommand {
    Open,
    NewWindow,
    Add,
    CopyToFolder,
    MoveToFolder,
    /// Read, or unread again; where the reader is stays put.
    ToggleRead,
    RemoveDownload,
    /// Out of the folder the book is listed in, and to Trash with its last
    /// folder — or straight to Trash where no folder is in view.
    Remove,
}

impl OverlayCommand {
    /// Opening the book sits beside its title; the rest share a row below.
    fn opens(self) -> bool {
        matches!(self, Self::Open | Self::NewWindow | Self::Add)
    }

    fn about_folder(self) -> bool {
        matches!(self, Self::CopyToFolder | Self::MoveToFolder)
    }
}

impl BookCardView {
    pub(crate) fn title(&self) -> &str {
        &self.book.title
    }
    pub(crate) fn thumbnail_resolution(window: &Window) -> ThumbnailResolution {
        let logical_width = gpui::px(components::BOOK_CARD_CONTENT_WIDTH);
        let physical_width = f32::from(logical_width) * window.scale_factor();
        if physical_width <= ThumbnailResolution::Browse.width() as f32 { ThumbnailResolution::Browse } else { ThumbnailResolution::CardHighDensity }
    }

    /// A card in a browse grid, which is what gives it somewhere to navigate to.
    pub(crate) fn create_in_grid(row: BookCardRow, ui: LibraryContext, events: BookCardEvents, folder_commands: bool, copy_from_library: bool, cx: &mut impl AppContext) -> Entity<Self> {
        cx.new(|cx| Self::new(row, ui, events, folder_commands, copy_from_library, cx))
    }

    fn new(row: BookCardRow, ui: LibraryContext, events: BookCardEvents, folder_commands: bool, copy_from_library: bool, cx: &mut Context<Self>) -> Self {
        // A book without a file has no thumbnail to wait for.
        let cover_updates = row.content_hash().map(|content_hash| Self::subscribe_to_cover_updates(&ui, content_hash, cx));
        // Only library books have a download state; catalog and feed books are added.
        if let Some(content_hash) = row.library_file() {
            ui.updates().update(cx, |updates, _| updates.track_download(content_hash, row.downloaded, row.download_requested));
        }
        let download_updates = Self::subscribe_to_download_updates(&ui, row.library_file(), cx);
        let cover_text_updates = cx.observe_global::<crate::stores::CoverTextSetting>(|_, cx| cx.notify());
        let generated_covers_updates = cx.observe_global::<crate::stores::GeneratedCoversOnly>(|card: &mut Self, cx| {
            card.cover_task.take();
            card.cover = CoverState::Pending;
            cx.notify();
        });
        let cover_evictions = cx.observe_global::<CoversEvicted>(|card: &mut Self, cx| {
            if card.cover_visibility.is_visible() {
                return;
            }
            card.cover_generation = card.cover_generation.wrapping_add(1);
            card.cover_task.take();
            card.cover = CoverState::Pending;
            cx.notify();
        });
        let feed_added_updates = matches!(row.source, BookSource::Opds { .. }).then(|| cx.observe_global::<AddedFeedBooks>(|_, cx| cx.notify()));
        Self {
            book: row,
            cover_loaded_at: None,
            cover: CoverState::Pending,
            cover_visibility: CoverVisibility::default(),
            cover_generation: 0,
            ui,
            events,
            folder_commands,
            copy_from_library,
            cover_task: None,
            cover_updates,
            _download_updates: download_updates,
            download_task: None,
            add_task: None,
            context_menu: None,
            overlay: None,
            _cover_text_updates: cover_text_updates,
            _generated_covers_updates: generated_covers_updates,
            _cover_evictions: cover_evictions,
            _feed_added_updates: feed_added_updates,
        }
    }

    /// The book's file, when it has one.
    pub(crate) fn content_hash(&self) -> Option<BookId> {
        self.book.content_hash()
    }

    pub(crate) fn can_drag(&self) -> bool {
        matches!(self.book.source, BookSource::Library { .. })
    }

    /// The card's identity in a listing.
    pub(crate) fn key(&self) -> library_model::CardKey {
        self.book.key()
    }

    /// The concrete placement represented by a folder card.
    pub(crate) fn source_directory(&self) -> Option<DirId> {
        self.book.source_directory
    }

    /// Each placement has a cache and interaction state of its own, even when
    /// the same book appears in both a section and the graph's cover rail.
    pub(crate) fn presentation(card: Entity<Self>, presentation: BookCardPresentation, cx: &mut App) -> Entity<BookCardElement> {
        cx.new(|cx| {
            let subscription = cx.observe(&card, |_, _, cx| cx.notify());
            BookCardElement {
                card,
                presentation,
                interaction: BookCardInteraction::default(),
                _subscription: subscription,
                #[cfg(test)]
                renders: 0,
            }
        })
    }

    pub(crate) fn present(view: Entity<BookCardElement>, interaction: BookCardInteraction, window: &Window, cx: &mut App) -> impl IntoElement {
        view.update(cx, |view, _| view.interaction = interaction);
        // Interaction comes from the parent during layout, after GPUI has
        // collected this frame's dirty views. A changed element path busts
        // the cache in this frame without requesting another frame.
        let interaction_key = usize::from(interaction.focused) | (usize::from(interaction.selected) << 1) | (usize::from(interaction.marquee) << 2);
        // rem and display density can change while a cell keeps its bounds.
        let key = format!("card-presentation-{interaction_key}-{}-{}", f32::from(window.rem_size()).to_bits(), window.scale_factor().to_bits());
        CacheScope::new(key.into(), view.cached(StyleRefinement::default().size_full()).into_any_element())
    }

    pub(crate) fn preview(card: &Entity<Self>, presentation: BookCardPresentation, interaction: BookCardInteraction, window: &mut Window, cx: &mut App) -> gpui::AnyElement {
        card.update(cx, |card, cx| card.render_card(presentation, interaction, true, window, cx))
    }

    pub(crate) fn replace_row(&mut self, row: BookCardRow, cx: &mut Context<Self>) {
        debug_assert_eq!(self.book.key(), row.key());
        if self.book != row {
            self.book = row;
            cx.notify();
        }
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let Some(content_hash) = self.book.library_file() else { return };
        if !matches!(self.ui.updates().read(cx).download_state(content_hash), Some(DownloadState::NotDownloaded)) || self.download_task.is_some() {
            return;
        }
        let library = self.ui.backend().clone();
        self.download_task = Some(cx.spawn(async move |card, cx| {
            let result = library.download_book(content_hash).await;
            let _ = card.update_in(cx, |card, window, cx| {
                card.download_task.take();
                if let Err(error) = result {
                    log::warn!("Could not download library book {content_hash}: {error}");
                    notify_error("book-download-error", book_transfer_error_message(error.failure()), window, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Adds catalog and feed books to the library, and marks them held for the
    /// session. The catalog's next listing then leaves the catalog books out.
    /// Opens the store page that sells a feed book; what is bought there
    /// goes into the library's root.
    fn buy(&mut self, entry: library_model::CardKey, window: &mut Window, cx: &mut Context<Self>) {
        let (app, library_id) = (self.ui.app().clone(), *self.ui.backend().id());
        cx.spawn_in(window, async move |card, cx| {
            if let Err(error) = app.buy_opds_book(entry, library_id).await {
                let _ = card.update_in(cx, |_, window, cx| notify_error("book-buy-error", format!("Could not open the store. {}", user_facing_error(&error, "Please try again.")), window, cx));
            }
        })
        .detach();
    }

    fn add(&mut self, books: Vec<BookCardRow>, cx: &mut Context<Self>) {
        if self.add_task.is_some() {
            return;
        }
        let library = self.ui.backend().clone();
        let app = self.ui.app().clone();
        self.add_task = Some(cx.spawn(async move |card, cx| {
            let mut results = Vec::new();
            for book in books {
                if matches!(book.source, BookSource::Library { .. } | BookSource::Opds { downloadable: false, .. } | BookSource::ExternalCatalog { downloadable: false, .. }) {
                    continue;
                }
                let result = import_catalog_book(&app, &library, book.source, ROOT_DIR_ID).await;
                results.push((book.key(), book.title, result));
            }
            let _ = card.update_in(cx, |card, window, cx| {
                card.add_task.take();
                for (key, title, result) in results {
                    match result {
                        Ok(_) => {
                            mark_feed_book_added(key, cx);
                            notify_info("catalog-add", format!("Added “{title}” to the library"), window, cx);
                        }
                        Err(error) => notify_error("catalog-add-error", format!("Could not add “{title}”. {}", user_facing_error(&error, "Please try again.")), window, cx),
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn subscribe_to_cover_updates(ui: &LibraryContext, content_hash: sync_common::BookId, cx: &mut Context<Self>) -> Subscription {
        let updates = ui.updates();
        cx.subscribe(&updates, move |view, _, event: &LibraryCoversChanged, cx| {
            if event.contains(&content_hash) {
                view.thumbnail_available(cx);
            }
        })
    }

    fn subscribe_to_download_updates(ui: &LibraryContext, content_hash: Option<BookId>, cx: &mut Context<Self>) -> Option<DownloadSubscription> {
        let content_hash = content_hash?;
        let mut previous = ui.updates().read(cx).download_state(content_hash);
        Some(ui.subscribe_download(content_hash, cx, move |_view, state, cx| {
            // A card draws the phase, not the percentage. Detail pages
            // still receive every progress event for their progress bar.
            let changed = previous.as_ref().is_none_or(|previous| discriminant(previous) != discriminant(state));
            previous = Some(state.clone());
            if changed {
                cx.notify();
            }
        }))
    }

    fn thumbnail_available(&mut self, cx: &mut Context<Self>) {
        if self.cover.loaded().is_some() {
            return;
        }
        // The worker has committed a thumbnail, so any in-flight or missing
        // result is stale.
        self.cover = CoverState::Pending;
        // A prior asynchronous lookup may still be about to report "missing".
        // Cancelling it ensures the next render starts a fresh read of the file
        // that the scanner has just committed.
        self.cover_task.take();
        cx.notify();
    }

    fn listen_for_thumbnail(&mut self, cx: &mut Context<Self>) {
        let Some(content_hash) = self.book.content_hash().filter(|_| self.cover_updates.is_none()) else { return };
        self.cover_updates = Some(Self::subscribe_to_cover_updates(&self.ui, content_hash, cx));
    }

    fn load_cover(&mut self, resolution: ThumbnailResolution, cx: &mut Context<Self>) {
        let load = self.ui.clone().book_cover(self.book.source, resolution);
        self.load_cover_from(resolution, load, cx);
    }

    fn load_cover_from(&mut self, resolution: ThumbnailResolution, load: impl Future<Output = Result<Option<Vec<u8>>, BackendError>> + 'static, cx: &mut Context<Self>) {
        let key = self.book.key();
        // Library thumbnails can be published later by the worker. A remote
        // cover has no such update stream after a missing or failed response.
        let library_thumbnail = matches!(self.book.source, BookSource::Library { .. } | BookSource::Catalog { .. });
        let generation = self.cover_generation;
        let decoder = cx.background_executor().clone();
        self.cover_task = Some(cx.spawn(async move |view, cx| {
            let cover_stage = Stage::deferred("cover retrieval and decoding");
            let result = match load.await {
                Ok(Some(bytes)) => decoder.spawn(async move { loaded_cover_from_jpeg(bytes) }).await.map(Some).ok_or_else(|| BackendError::message("cover is not a readable JPEG")),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            cover_stage.finish(match &result {
                Ok(Some(_)) => "decoded",
                Ok(None) => "missing",
                Err(_) => "failed",
            });
            cx.defer_update(view, move |view, cx| {
                if generation != view.cover_generation {
                    return;
                }
                view.cover_task.take();
                if view.book.key() != key {
                    return;
                }
                match result {
                    Ok(Some(cover)) => {
                        // Only the first arrival fades; replacing a cover
                        // with a denser image keeps its original timestamp.
                        view.remember_cover(cover, resolution, cx);
                        view.cover_updates.take();
                    }
                    result => {
                        if let Err(error) = &result {
                            if library_thumbnail {
                                log::error!("failed to load thumbnail for book {:?}: {error}", view.book.title);
                            } else {
                                log::warn!("failed to load a catalog or feed cover: {error}");
                            }
                        }
                        if view.cover.loaded().is_none() {
                            view.cover = if result.is_err() { CoverState::Unavailable } else { CoverState::Missing };
                        }
                        if library_thumbnail {
                            view.listen_for_thumbnail(cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn cover_cache_key(&self) -> (Uuid, CardKey) {
        (*self.ui.backend().id(), self.book.key())
    }

    fn remember_cover(&mut self, cover: LoadedCover, resolution: ThumbnailResolution, cx: &mut Context<Self>) {
        // A slower request must not replace an already decoded denser cover.
        let cached = cx.default_global::<BrowseCovers>().get(&self.cover_cache_key()).filter(|(_, cached)| *cached == resolution || (*cached == ThumbnailResolution::CardHighDensity && resolution == ThumbnailResolution::Browse));
        let replace_cache = cached.is_none();
        let (cover, resolution) = cached.unwrap_or((cover, resolution));
        track_image(&cover, cx);
        if replace_cache {
            cx.default_global::<BrowseCovers>().insert(self.cover_cache_key(), &cover, resolution);
        }
        self.cover = CoverState::Loaded(Arc::downgrade(&cover), resolution);
        self.cover_loaded_at.get_or_insert_with(Instant::now);
    }

    pub(crate) fn prepare(&mut self, resolution: ThumbnailResolution, cx: &mut Context<Self>) {
        // With "generated covers only", no art is read or decoded at all.
        if crate::stores::generated_covers_only(cx) {
            self.cover_task.take();
            self.cover_updates.take();
            self.cover = CoverState::Missing;
            return;
        }
        // Touch covers as they are drawn or prefetched, and share a decoded
        // image with other cards for this book. An evicted card reads its JPEG
        // again on demand. Higher densities replace the same cache entry.
        if let Some((image, resolution)) = cx.default_global::<BrowseCovers>().get(&self.cover_cache_key()) {
            track_image(&image, cx);
            self.cover = CoverState::Loaded(Arc::downgrade(&image), resolution);
            self.cover_loaded_at.get_or_insert_with(Instant::now);
            self.cover_updates.take();
        } else if let CoverState::Loaded(image, _) = &self.cover {
            // A painted frame or detail can still own this image. Keep using
            // it without reinserting it and displacing other cached covers.
            if image.upgrade().is_none() {
                self.cover = CoverState::Pending;
            }
        }
        let needs_cover = match &self.cover {
            CoverState::Pending => true,
            CoverState::Loaded(image, loaded_resolution) => image.upgrade().is_none() || (*loaded_resolution == ThumbnailResolution::Browse && resolution == ThumbnailResolution::CardHighDensity),
            CoverState::Missing | CoverState::Unavailable => false,
        };
        if needs_cover && self.cover_task.is_none() {
            // Listen before the asynchronous read: a synced thumbnail can be
            // published while that read is still reporting the old absence.
            if matches!(self.cover, CoverState::Pending) {
                self.listen_for_thumbnail(cx);
            }
            self.load_cover(resolution, cx);
        }
    }

    /// Allows a container to fetch a card's cover before the card is visible.
    /// A visible card also starts this request from its own render path.
    pub(crate) fn prefetch_for_window(card: &Entity<Self>, window: &Window, cx: &mut gpui::App) {
        let resolution = Self::thumbnail_resolution(window);
        card.update(cx, |card, cx| card.prepare(resolution, cx));
    }

    /// What a book's page opens with: the row this card is drawing, and the
    /// cover it has already decoded. Handing both over is why that page paints
    /// complete in the frame the card was clicked in.
    pub(crate) fn detail_source(&self) -> (BookCardRow, Option<LoadedCover>) {
        (self.book.clone(), self.cover.loaded())
    }

    /// Activating the cover opens the book's page. The overlay's play button
    /// uses `play` to open the reader or start audiobook playback directly.
    /// Takes what it needs out of the card and lets go of it before acting.
    ///
    /// Deliberately not `card.update(|card, cx| …)`: going to the book's page is
    /// navigation, and the load that follows touches every card in the grid —
    /// their cursor, their card mode — including this one. Holding this card
    /// leased across that is a panic, and was one.
    pub(crate) fn activate(card: &Entity<Self>, window: &mut Window, cx: &mut gpui::App) {
        let (book, cover, events) = {
            let card = card.read(cx);
            let (book, cover) = card.detail_source();
            (book, cover, card.events.clone())
        };
        events(BookCardEvent::OpenDetail(book, cover), window, cx);
    }

    /// Release the card before either navigation or playback touches the grid.
    fn play(card: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let (book, cover, events, ui) = {
            let card = card.read(cx);
            let (book, cover) = card.detail_source();
            (book, cover, card.events.clone(), card.ui.clone())
        };
        let Some(content_hash) = book.library_file() else { return };
        if book.format_category == library_model::LibraryFileTypeFilter::Audiobook {
            events(BookCardEvent::OpenDetail(book.clone(), cover), window, cx);
        }
        ui.open_book_at(content_hash, book.title, None, window, cx);
    }

    /// Runs a book command from the card menu: the backend call, with failures
    /// surfaced as toasts.
    fn run_menu_action(target: Entity<Self>, command: BookCommand, books: Vec<BookId>, cx: &mut App) {
        let library = target.read(cx).ui.backend().clone();
        // The task retains its error-reporting target if the card leaves the page.
        cx.spawn(async move |cx| {
            if let Err(error) = command.execute(&library, books).await {
                let _ = target.downgrade().update_in(cx, |_, window, cx| command.report_failure(error, window, cx));
            }
        })
        .detach();
    }

    /// Mouse selections and keyboard menus: one book opens its overlay, several
    /// the menu of what can be done to all of them at once.
    pub(crate) fn open_items_menu(cards: &[Entity<Self>], position: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let Some(target) = cards.first() else { return };
        let mut seen = HashSet::new();
        let books = cards.iter().map(|card| card.read(cx).book.clone()).filter(|book| seen.insert(book.key())).collect::<Vec<_>>();
        if books.len() == 1 {
            target.update(cx, |card, cx| card.open_overlay(window, cx));
        } else {
            target.update(cx, |card, cx| card.open_books_menu(books, position, window, cx));
        }
    }

    /// Opens the overlay over this card. The header draws from the card's row
    /// at once; the blurb, subjects and linked names follow with the detail.
    fn open_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        let previous_focus = window.focused(cx);
        focus.focus(window, cx);
        let load = self.load_overlay_detail(cx);
        self.overlay = Some(BookOverlay { detail: None, focus, previous_focus, _load: load });
        cx.notify();
    }

    /// Closes the overlay, handing the keyboard back to where it was if the
    /// overlay still has it — a click elsewhere has already moved it on.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.overlay.take() else { return };
        if overlay.focus.contains_focused(window, cx)
            && let Some(previous) = overlay.previous_focus
        {
            previous.focus(window, cx);
        }
        cx.notify();
    }

    /// The same detail the book's page loads, by where the book comes from.
    fn load_overlay_detail(&self, cx: &mut Context<Self>) -> Task<()> {
        let book = self.book.clone();
        let ui = self.ui.clone();
        cx.spawn(async move |card, cx| {
            let result = ui.book_detail(book).await;
            let _ = card.update(cx, |card, cx| {
                match result {
                    Ok(detail) => {
                        if let Some(overlay) = &mut card.overlay {
                            overlay.detail = Some(detail);
                        }
                    }
                    Err(error) => log::warn!("Could not load the overlay of {:?}: {error}", card.book.title),
                }
                cx.notify();
            });
        })
    }

    /// The commands this book has, in the overlay's order: opening it, then
    /// those about the folder it is listed in, then those about the book.
    fn overlay_commands(&self, cx: &App) -> Vec<OverlayCommand> {
        let book = &self.book;
        let Some(content_hash) = book.library_file() else {
            let offered = !matches!(book.source, BookSource::Opds { downloadable: false, .. } | BookSource::ExternalCatalog { downloadable: false, .. });
            return if offered && !held(book, cx) { vec![OverlayCommand::Add] } else { Vec::new() };
        };
        let in_folder = self.folder_commands && book.source_directory.is_some();
        let mut commands = vec![OverlayCommand::Open];
        if crate::native_windows_available() {
            commands.push(OverlayCommand::NewWindow);
        }
        if in_folder || self.copy_from_library {
            commands.push(OverlayCommand::CopyToFolder);
        }
        if in_folder {
            commands.push(OverlayCommand::MoveToFolder);
        }
        commands.push(OverlayCommand::ToggleRead);
        if matches!(self.ui.updates().read(cx).download_state(content_hash), Some(DownloadState::Downloaded)) {
            commands.push(OverlayCommand::RemoveDownload);
        }
        commands.push(OverlayCommand::Remove);
        commands
    }

    /// A command's button, without what it does: the overlay measures these
    /// as well as drawing them, and `suffix` keeps the measured ones apart.
    fn overlay_button(&self, command: OverlayCommand, suffix: &str, theme: components::BrowserTheme) -> Button {
        let id = SharedString::from(format!("book-overlay-{command:?}-{}{suffix}", self.book.key()));
        let audiobook = self.book.format_category == library_model::LibraryFileTypeFilter::Audiobook;
        match command {
            OverlayCommand::Open => {
                let icon: Icon = if audiobook { components::Headphones.into() } else { IconName::BookOpen.into() };
                components::primary_action_button(id, if audiobook { "Listen" } else { "Read" }, icon, false)
            }
            OverlayCommand::Add => components::primary_action_button(id, if self.add_task.is_some() { "Adding…" } else { "Add to library" }, components::download_glyph(), self.add_task.is_some()),
            OverlayCommand::NewWindow => components::book_overlay_command(id, "New window", IconName::ExternalLink, theme),
            OverlayCommand::CopyToFolder => components::book_overlay_command(id, "Copy to folder", IconName::Copy, theme),
            OverlayCommand::MoveToFolder => components::book_overlay_command(id, "Move to folder", IconName::FolderOpen, theme),
            // Finished is what the cover's bar calls finished.
            OverlayCommand::ToggleRead if self.book.progress >= FINISHED_PROGRESS_THRESHOLD => components::book_overlay_command(id, "Mark unread", IconName::Undo2, theme),
            OverlayCommand::ToggleRead => components::book_overlay_command(id, "Mark read", IconName::Check, theme),
            OverlayCommand::RemoveDownload => components::book_overlay_command(id, "Remove download", IconName::HardDrive, theme),
            OverlayCommand::Remove => {
                let tooltip = if self.book.source_directory.is_some() { "Remove from this folder, and to Trash if it is in no other" } else { "Move to Trash" };
                components::book_overlay_command(id, "Remove", components::navigation_icon(components::NavigationIcon::Trash2), theme).tooltip(tooltip)
            }
        }
    }

    fn overlay_command_button(&self, command: OverlayCommand, card: &Entity<Self>, theme: components::BrowserTheme) -> Button {
        let card = card.clone();
        self.overlay_button(command, "", theme).on_click(move |_, window, cx| Self::run_overlay_command(&card, command, window, cx))
    }

    /// Closes the overlay, then acts. Lets go of the card before acting, as
    /// `activate` does: opening a book or a folder dialog touches the grid.
    fn run_overlay_command(card: &Entity<Self>, command: OverlayCommand, window: &mut Window, cx: &mut App) {
        card.update(cx, |card, cx| card.close_overlay(window, cx));
        let (book, events, backend, folder_commands) = {
            let card = card.read(cx);
            (card.book.clone(), card.events.clone(), card.ui.backend().clone(), card.folder_commands)
        };
        let content_hash = book.library_file();
        let folder = book.source_directory;
        match (command, content_hash) {
            (OverlayCommand::Open, _) => Self::play(card, window, cx),
            (OverlayCommand::Add, _) => card.update(cx, |card, cx| card.add(vec![card.book.clone()], cx)),
            (OverlayCommand::NewWindow, Some(_)) => open_book_windows(*backend.id(), std::slice::from_ref(&book), window, cx),
            (OverlayCommand::CopyToFolder, Some(content_hash)) => events(BookCardEvent::Place(vec![content_hash], folder.filter(|_| folder_commands), FolderPlacement::Copy), window, cx),
            (OverlayCommand::MoveToFolder, Some(content_hash)) => {
                if let Some(folder) = folder {
                    events(BookCardEvent::Place(vec![content_hash], Some(folder), FolderPlacement::Move), window, cx);
                }
            }
            (OverlayCommand::ToggleRead, Some(content_hash)) => Self::run_menu_action(card.clone(), BookCommand::SetRead(book.progress < FINISHED_PROGRESS_THRESHOLD), vec![content_hash], cx),
            (OverlayCommand::RemoveDownload, Some(content_hash)) => Self::run_menu_action(card.clone(), BookCommand::RemoveDownload, vec![content_hash], cx),
            // The backend trashes a book whose last folder it leaves.
            (OverlayCommand::Remove, Some(content_hash)) => match folder {
                Some(folder) if folder_commands => events(BookCardEvent::Remove(content_hash, folder), window, cx),
                Some(folder) => Self::run_menu_action(card.clone(), BookCommand::RemoveFromFolder(folder), vec![content_hash], cx),
                None => Self::run_menu_action(card.clone(), BookCommand::TrashBook, vec![content_hash], cx),
            },
            (_, None) => {}
        }
    }

    pub(crate) fn selection_actions(cards: &[Entity<Self>], cx: &App) -> Vec<SelectionAction> {
        let Some(target) = cards.first() else { return Vec::new() };
        let mut seen = HashSet::new();
        let books = cards.iter().map(|card| card.read(cx).book.clone()).filter(|book| seen.insert(book.key())).collect();
        target.read(cx).book_selection_actions(books, target.clone(), cx)
    }

    pub(crate) fn download_selection_action(cards: &[Entity<Self>], cx: &App) -> Option<SelectionAction> {
        let target = cards.iter().find(|card| card.read(cx).book.library_file().is_some())?;
        let books = cards.iter().map(|card| card.read(cx).book.clone()).collect::<Vec<_>>();
        target.read(cx).library_download_action(&books, target.clone(), cx)
    }

    fn library_download_action(&self, books: &[BookCardRow], target: Entity<Self>, cx: &App) -> Option<SelectionAction> {
        let updates = self.ui.updates().read(cx);
        let mut seen = HashSet::new();
        let mut pending = false;
        let mut downloads = Vec::new();
        for book in books {
            let Some(hash) = book.library_file().filter(|hash| seen.insert(*hash)) else { continue };
            let state = updates.download_state(hash).unwrap_or_else(|| initial_download_state(book.downloaded, book.download_requested));
            match state {
                DownloadState::NotDownloaded => downloads.push((hash, book.title.clone())),
                DownloadState::Queued | DownloadState::Downloading(_) => pending = true,
                DownloadState::Downloaded => {}
            }
        }
        if seen.is_empty() {
            return None;
        }
        let disabled = downloads.is_empty();
        let label = if !disabled {
            "Download selected books"
        } else if pending {
            "Downloading…"
        } else {
            "Downloaded"
        };
        let library = self.ui.backend().clone();
        Some(
            SelectionAction::new("download", label, components::download_glyph(), move |_, cx| {
                let (library, downloads) = (library.clone(), downloads.clone());
                target.update(cx, |_, cx| {
                    // Queue the whole selection even if its first card leaves the
                    // page while the requests are being sent.
                    cx.spawn(async move |card, cx| {
                        for (book, title) in downloads {
                            if let Err(error) = library.download_book(book).await {
                                let _ = card.update_in(cx, |_, window, cx| notify_error("book-download-error", format!("“{title}”: {}", book_transfer_error_message(error.failure())), window, cx));
                            }
                        }
                    })
                    .detach();
                });
            })
            .disabled(disabled),
        )
    }

    fn book_selection_actions(&self, books: Vec<BookCardRow>, target: Entity<Self>, cx: &App) -> Vec<SelectionAction> {
        if books.iter().any(|book| book.library_file().is_none()) {
            let multiple = books.len() > 1;
            let not_offered = |book: &BookCardRow| matches!(book.source, BookSource::Opds { downloadable: false, .. } | BookSource::ExternalCatalog { downloadable: false, .. });
            let nothing_offered = books.iter().all(not_offered);
            let missing = books.into_iter().filter(|book| !held(book, cx) && !not_offered(book)).collect::<Vec<_>>();
            if missing.is_empty() {
                let label = if nothing_offered { "Not offered for download" } else { "In your library" };
                return vec![SelectionAction::new("download", label, IconName::Info, |_, _| {}).disabled(true)];
            }
            let label = if self.add_task.is_some() {
                "Downloading…"
            } else if multiple {
                "Download selected books"
            } else {
                "Add to library"
            };
            return vec![
                SelectionAction::new("download", label, components::download_glyph(), move |_, cx| {
                    target.update(cx, |card, cx| card.add(missing.clone(), cx));
                })
                .disabled(self.add_task.is_some()),
            ];
        }
        let hashes = books.iter().filter_map(BookCardRow::library_file).collect::<Vec<_>>();
        let mut actions = Vec::new();
        actions.extend(self.library_download_action(&books, target.clone(), cx));
        if crate::native_windows_available() {
            let (library_id, books) = (*self.ui.backend().id(), books.clone());
            actions.push(SelectionAction::new("new-window", "Open in New Window", IconName::ExternalLink, move |window, cx| {
                open_book_windows(library_id, &books, window, cx);
            }));
        }
        if self.copy_from_library {
            let (events, hashes) = (self.events.clone(), hashes.clone());
            actions.push(SelectionAction::new("copy", "Copy to folder…", IconName::Copy, move |window, cx| events(BookCardEvent::Place(hashes.clone(), None, FolderPlacement::Copy), window, cx)));
        }
        let downloaded = hashes.iter().copied().filter(|hash| matches!(self.ui.updates().read(cx).download_state(*hash), Some(DownloadState::Downloaded))).collect::<Vec<_>>();
        if !downloaded.is_empty() {
            let target = target.clone();
            actions.push(SelectionAction::new("remove-download", "Remove download", IconName::HardDrive, move |_, cx| {
                Self::run_menu_action(target.clone(), BookCommand::RemoveDownload, downloaded.clone(), cx);
            }));
        }
        actions.push(SelectionAction::new("trash", "Move to Trash", components::navigation_icon(components::NavigationIcon::Trash2), move |_, cx| {
            Self::run_menu_action(target.clone(), BookCommand::TrashBooks, hashes.clone(), cx);
        }));
        actions
    }

    fn open_books_menu(&mut self, books: Vec<BookCardRow>, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let actions = self.book_selection_actions(books, cx.entity(), cx);
        let previous_focus = window.focused(cx);
        let items = move |mut menu: PopupMenu| {
            if let Some(focus) = previous_focus {
                menu = menu.action_context(focus);
            }
            actions.iter().fold(menu, |menu, action| action.menu_item(menu))
        };
        self.context_menu = Some(ContextMenu::new(position, window, cx, items, |menu, _, cx| {
            cx.subscribe(menu, |card, _, _: &DismissEvent, cx| {
                card.context_menu = None;
                cx.notify();
            })
        }));
        cx.notify();
    }
}

impl BookCardView {
    fn render_card(&mut self, presentation: BookCardPresentation, interaction: BookCardInteraction, preview: bool, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        if !preview {
            self.cover_visibility.track(window);
            self.prepare(Self::thumbnail_resolution(window), cx);
        }
        let theme = components::browser_theme(cx);
        let grid_cover = presentation == BookCardPresentation::Grid;
        let cover_opacity = match self.cover_loaded_at {
            Some(_) if cfg!(feature = "kobo") || cx.reduce_motion() => 1.0,
            Some(loaded_at) => {
                let progress = loaded_at.elapsed().as_secs_f32() / COVER_FADE_DURATION.as_secs_f32();
                if progress < 1.0 {
                    window.request_animation_frame();
                }
                progress.clamp(0.0, 1.0)
            }
            None => 1.0,
        };
        let book = self.book.clone();
        let cover = self.cover.loaded();
        let key = book.key();
        let element_id = format!("book-card-{key}");
        let cover_id = format!("book-cover-{key}");

        let cover_text = if presentation == BookCardPresentation::GraphCover { CoverText::CoverOnly } else { crate::stores::current_cover_text(cx) };
        let open_target = cx.entity();
        let cover_ratio = match &cover {
            Some(cover) => cover_aspect_ratio(&cover),
            // A generated cover keeps its own shape, audiobook or not.
            None if book.format_category == library_model::LibraryFileTypeFilter::Audiobook && !matches!(self.cover, CoverState::Missing) => 1.0,
            None => components::BOOK_COVER_ASPECT_RATIO,
        };
        // A book not on this device says so on its cover, in the corner nothing
        // else uses, and the mark is the way to fetch it. A downloaded book
        // carries no mark: which state is the rarer one differs from device to
        // device, and marking only the one that needs acting on keeps a mostly
        // downloaded library quiet and a mostly remote one legible.
        let download_badge = match book.source {
            // A catalog or feed book's corner adds it, or marks that the library holds it.
            BookSource::Catalog { .. } | BookSource::Opds { .. } | BookSource::ExternalCatalog { downloadable: true, .. } if held(&book, cx) => {
                Some(components::book_cover_in_library_badge(format!("book-in-library-{key}"), theme).tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("In your library").build(window, cx)))
            }
            // A feed book the feed sells opens its store page.
            BookSource::Opds { entry, downloadable: false, buyable: true, .. } => {
                let buy_target = cx.entity();
                Some(
                    components::book_cover_buy_badge(format!("book-buy-{key}"), theme)
                        .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Buy").build(window, cx))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            buy_target.update(cx, |card, cx| card.buy(entry, window, cx));
                        }),
                )
            }
            // A feed book the feed only lends or locks has nothing to add.
            BookSource::Opds { downloadable: false, .. } | BookSource::ExternalCatalog { downloadable: false, .. } => None,
            BookSource::Catalog { .. } | BookSource::Opds { downloadable: true, .. } | BookSource::ExternalCatalog { downloadable: true, .. } => {
                let add_target = cx.entity();
                let added = book.clone();
                Some(
                    components::book_cover_add_badge(format!("book-add-{key}"), self.add_task.is_some(), theme)
                        .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Add to library").build(window, cx))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let added = added.clone();
                            add_target.update(cx, |card, cx| card.add(vec![added], cx));
                        }),
                )
            }
            BookSource::Library { content_hash } => match self.ui.updates().read(cx).download_state(content_hash).unwrap_or(DownloadState::Queued) {
                DownloadState::Downloaded => None,
                DownloadState::Queued => Some(components::book_cover_queued_badge(format!("book-download-{key}"), theme).tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Queued for download").build(window, cx))),
                DownloadState::Downloading(_) => Some(components::book_cover_download_badge(format!("book-download-{key}"), true, theme)),
                // Provider libraries can also retain browser offline copies.
                DownloadState::NotDownloaded if cfg!(target_arch = "wasm32") && !content_hash.is_external() => None,
                DownloadState::NotDownloaded => {
                    let download_target = cx.entity();
                    Some(
                        components::book_cover_download_badge(format!("book-download-{key}"), false, theme)
                            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Download to this device").build(window, cx))
                            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                download_target.update(cx, |card, cx| card.download(cx));
                            }),
                    )
                }
            },
        };
        let play_button = book.library_file().filter(|_| !interaction.marquee).map(|_| {
            let play_target = cx.entity();
            components::book_cover_play_button(format!("book-play-{key}"), theme)
                .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Play").build(window, cx))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    Self::play(&play_target, window, cx);
                })
        });
        let progress = (grid_cover || book.progress >= FINISHED_PROGRESS_THRESHOLD).then_some(book.progress);
        let cover_frame = match cover {
            Some(cover) => components::book_cover_with_image(cover, cover_ratio, cover_opacity, progress, theme),
            None => {
                let frame = if matches!(self.cover, CoverState::Missing) {
                    components::book_cover_with_generated_art(book.title.clone(), book.subtitle.as_deref(), &book.author, generated_cover_variant(key), theme)
                } else {
                    // Leave the slot empty until absence is confirmed.
                    components::book_cover_frame(theme, cover_ratio)
                };
                frame.children(progress.and_then(|progress| components::book_cover_progress(progress, theme)))
            }
        };
        let cover_frame = cover_frame
            .id(cover_id)
            .when(presentation == BookCardPresentation::Section, |frame| frame.flex_none())
            .on_click(move |_, window, cx| {
                if !interaction.marquee {
                    Self::activate(&open_target, window, cx);
                }
            })
            .children(play_button)
            // The cover's action — add, download, or the held mark — is on every
            // cover, including those in section cards and the graph.
            .children(download_badge)
            .when(book.format_category == library_model::LibraryFileTypeFilter::Audiobook, |frame| frame.child(components::book_cover_audiobook_length(book.audiobook_duration_ms, theme)));

        let context_menu = self.context_menu.as_ref().map(|active| active.render(window));
        let context_menu_target = cx.entity();
        let long_press_target = context_menu_target.clone();
        let hover_enabled = !interaction.marquee && presentation != BookCardPresentation::Section;
        let card = components::book_card(element_id, interaction.focused, interaction.selected, hover_enabled, theme)
            .when(presentation == BookCardPresentation::Section, |card| {
                // The section's keyboard frame adds a 1px border outside this
                // card's 2px outline. Together with padding they total 8px.
                card.p(px((components::BOOK_CARD_PADDING - 3.0).max(0.0)))
            })
            .on_aux_click(move |event, window, cx| {
                // A card inside a selection is not the subject of its own menu:
                // the page opens one over everything the band picked up.
                if event.is_secondary() && !interaction.selected {
                    context_menu_target.update(cx, |card, cx| card.open_overlay(window, cx));
                    cx.stop_propagation();
                }
            })
            .on_long_press(move |_, window, cx| {
                if !interaction.selected {
                    long_press_target.update(cx, |card, cx| card.open_overlay(window, cx));
                    cx.stop_propagation();
                }
            });

        let card = card.child(cover_frame);
        // No text block at all under "cover only", not an empty one: an empty
        // child would still hold the cover-to-text gap open under a cover that
        // has nothing under it. The download mark is on the cover, so it stays.
        let card = match cover_text {
            CoverText::Always => card.child(components::book_card_title_row(book.title, book.subtitle.as_deref())),
            CoverText::CoverOnly => card,
        };

        // A drag preview draws the card alone.
        let overlay = if preview { None } else { self.render_overlay(window, cx) };
        components::book_card_cell().when(presentation == BookCardPresentation::GraphCover, |cell| cell.justify_start()).child(card.children(context_menu).children(overlay)).into_any_element()
    }

    /// The overlay: beside the card where the window has room — right, else
    /// left, else over the grid — and a bottom sheet on a phone.
    ///
    /// Its width is its commands'. Opening the book and the book's own
    /// commands, measured as though in one row, set it, so that moving the
    /// first two up beside the title costs the title nothing; the row under
    /// it can widen it further. The text wraps to what that leaves.
    fn render_overlay(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let overlay = self.overlay.as_ref()?;
        let theme = components::browser_theme(cx);
        let key = self.book.key();
        let card = cx.entity();
        let mobile = components::uses_mobile_navigation(window);
        let commands = self.overlay_commands(cx);
        let opening = commands.iter().copied().filter(|command| command.opens()).collect::<Vec<_>>();
        let row = commands.iter().copied().filter(|command| !command.opens()).collect::<Vec<_>>();
        let viewport = window.viewport_size();
        let margin = px(components::SPACE_MD);
        let width = if mobile {
            viewport.width
        } else {
            let sizing = commands.iter().copied().filter(|command| !command.about_folder()).collect::<Vec<_>>();
            let sizing = components::book_overlay_row_width(sizing.iter().map(|command| self.overlay_button(*command, "-measure", theme)).collect(), window, cx);
            let row = components::book_overlay_row_width(row.iter().map(|command| self.overlay_button(*command, "-measure", theme)).collect(), window, cx);
            components::book_overlay_width(&[sizing, row], viewport.width - margin * 2.0, window)
        };

        let mut header = components::book_overlay_header().child(self.overlay_headline(overlay.detail.as_ref(), &card, theme, cx));
        if !opening.is_empty() {
            header = header.child(components::book_overlay_actions().children(opening.iter().map(|command| self.overlay_command_button(*command, &card, theme))));
        }
        let body = self.overlay_body(overlay.detail.as_ref(), &card, theme);
        let commands = (!row.is_empty()).then(|| components::book_overlay_commands(mobile, theme).children(row.iter().map(|command| self.overlay_command_button(*command, &card, theme))));
        let escape_card = card.clone();
        let on_key_down = move |event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut App| {
            if event.keystroke.key == "escape" {
                escape_card.update(cx, |card, cx| card.close_overlay(window, cx));
                cx.stop_propagation();
            }
        };

        if mobile {
            let scrim_card = card.clone();
            let sheet = components::bottom_sheet_surface(theme).id(format!("book-overlay-{key}")).track_focus(&overlay.focus).on_key_down(on_key_down).child(header).child(body).children(commands);
            let layer =
                components::bottom_sheet_overlay().child(components::modal_scrim(format!("book-overlay-scrim-{key}"), theme).on_click(move |_, window, cx| scrim_card.update(cx, |card, cx| card.close_overlay(window, cx)))).child(sheet);
            return Some(deferred(components::anchor_to_window(layer, window)).with_priority(1).into_any_element());
        }

        let outside_card = card.clone();
        let surface = components::book_overlay_surface(format!("book-overlay-{key}"), width, theme)
            .track_focus(&overlay.focus)
            .on_key_down(on_key_down)
            .on_mouse_down_out(move |_, window, cx| outside_card.update(cx, |card, cx| card.close_overlay(window, cx)))
            .child(header)
            .child(body)
            .children(commands);
        Some(deferred(components::book_overlay_beside(surface, px(components::SPACE_XS), margin)).with_priority(1).into_any_element())
    }

    /// Title, byline, and the facts worth a glance: an audiobook's length, or
    /// whether a catalog book is held or can be had at all.
    fn overlay_headline(&self, detail: Option<&BookDetail>, card: &Entity<Self>, theme: components::BrowserTheme, cx: &App) -> Div {
        let book = &self.book;
        let mut headline = components::book_overlay_headline().child(components::book_overlay_title(book.title.clone(), book.subtitle.as_deref(), theme));
        // The card knows one author line; the detail knows who each name is.
        // Until it lands the line is stated rather than linked.
        let mut byline = components::book_overlay_byline(theme);
        match detail.filter(|detail| !detail.authors.is_empty()) {
            Some(detail) => {
                byline = byline.child(components::book_overlay_byline_text("by"));
                let names = detail.authors.iter().map(|author| (author.id, author.name.clone()));
                byline = self.overlay_name_links(byline, names, "author", card, theme);
                if !detail.credits.is_empty() {
                    byline = byline.child(components::book_overlay_byline_text("read by"));
                    let names = detail.credits.iter().map(|credit| (credit.id, credit.name.clone()));
                    byline = self.overlay_name_links(byline, names, "narrator", card, theme);
                }
            }
            None if !book.author.trim().is_empty() => byline = byline.child(components::book_overlay_byline_text("by")).child(components::book_overlay_byline_text(book.author.clone())),
            None => {}
        }
        headline = headline.child(byline);

        let mut facts = Vec::new();
        if let Some(duration) = book.audiobook_duration_ms {
            facts.push(SharedString::from(crate::library::pages::browse::spoken_duration(duration)));
        }
        if book.library_file().is_none() {
            if matches!(book.source, BookSource::Opds { downloadable: false, .. } | BookSource::ExternalCatalog { downloadable: false, .. }) {
                facts.push("Not offered for download".into());
            } else if held(book, cx) {
                facts.push("In your library".into());
            }
        }
        if !facts.is_empty() {
            headline = headline.child(components::book_overlay_facts(facts, theme));
        }
        headline
    }

    /// Names that open their author's page, a comma after each but the last.
    fn overlay_name_links(&self, mut byline: Div, names: impl Iterator<Item = (book_model::AuthorId, String)>, role: &str, card: &Entity<Self>, theme: components::BrowserTheme) -> Div {
        let key = self.book.key();
        let names = names.collect::<Vec<_>>();
        let last = names.len().saturating_sub(1);
        for (index, (author, name)) in names.into_iter().enumerate() {
            let open = crate::OpenAuthor { author, name: name.clone() };
            let card = card.clone();
            let link = components::book_overlay_name_link(format!("book-overlay-{role}-{key}-{author}"), name, theme).on_click(move |_, window, cx| {
                card.update(cx, |card, cx| card.close_overlay(window, cx));
                window.dispatch_action(Box::new(open.clone()), cx);
            });
            byline = byline.child(div().flex().min_w_0().child(link).when(index < last, |name| name.child(",")));
        }
        byline
    }

    /// The blurb, the subjects, and the way to the book's page.
    fn overlay_body(&self, detail: Option<&BookDetail>, card: &Entity<Self>, theme: components::BrowserTheme) -> Div {
        let key = self.book.key();
        let mut body = components::book_overlay_body();
        if let Some(detail) = detail {
            body = body.children(components::book_overlay_description(&detail.description, theme));
            if !detail.subjects.is_empty() {
                let mut subjects = components::book_overlay_subjects();
                for (index, path) in detail.subjects.iter().enumerate() {
                    let labels = path.iter().map(|segment| SharedString::from(segment.name.clone())).collect::<Vec<_>>();
                    let (path, card) = (path.clone(), card.clone());
                    subjects = subjects.child(components::book_overlay_subject(format!("book-overlay-subject-{key}-{index}"), &labels, theme, move |crumb, element| {
                        let (open, card) = (crate::OpenSubject { path: path[..=crumb].to_vec() }, card.clone());
                        element.on_click(move |_, window, cx| {
                            card.update(cx, |card, cx| card.close_overlay(window, cx));
                            window.dispatch_action(Box::new(open.clone()), cx);
                        })
                    }));
                }
                body = body.child(subjects);
            }
        }
        let card = card.clone();
        body.child(components::book_overlay_more_details(format!("book-overlay-details-{key}"), theme).on_click(move |_, window, cx| {
            card.update(cx, |card, cx| card.close_overlay(window, cx));
            Self::activate(&card, window, cx);
        }))
    }
}

impl Render for BookCardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_card(BookCardPresentation::Grid, BookCardInteraction::default(), false, window, cx)
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests;
