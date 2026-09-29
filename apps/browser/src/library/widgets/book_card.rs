use std::sync::Arc;
use std::time::Duration;

use web_time::Instant;

/// Long enough to read as a settle rather than a flash, short enough that a
/// fast-filling grid does not feel sluggish.
const COVER_FADE_DURATION: Duration = Duration::from_millis(180);

use gpui::prelude::*;
use gpui::{App, AppContext, Context, DismissEvent, Entity, Focusable as _, IntoElement, Pixels, Point, PromptLevel, Render, RenderImage, Subscription, Task, Window, px};
use gpui_component::menu::PopupMenu;
use gpui_component::IconName;
use library_model::BookCardRow;
use ui_components as components;

use crate::library::services::{LibraryContext, LibraryCoversChanged, LibraryDownloadChanged};
use sync_common::DirId;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use super::mobile_context_menu;

#[derive(Clone)]
pub(crate) struct LoadedCover {
    pub image: Arc<RenderImage>,
}

/// Navigates the grid this card is in to the book's own page.
///
/// The card hands over its row and its decoded cover rather than being looked
/// up afterwards, and not only to save the lookup: this is called from inside
/// the card's own update, so anything that went back and read the cards would
/// read one that is currently leased, and panic.
pub(crate) type OpenBookDetail = std::rc::Rc<dyn Fn(BookCardRow, Option<LoadedCover>, &mut gpui::App)>;

#[derive(Clone, Copy)]
pub(crate) enum BookPlacement {
    Copy,
    Move,
}

pub(crate) type PlaceBook = std::rc::Rc<dyn Fn(sync_common::ContentHash, DirId, BookPlacement, &mut gpui::App)>;
pub(crate) type RemoveBook = std::rc::Rc<dyn Fn(sync_common::ContentHash, DirId, String, &mut Window, &mut gpui::App)>;

/// A decoded cover's width over its height, for boxing it at its own
/// proportions instead of the placeholder's.
pub(crate) fn cover_aspect_ratio(image: &RenderImage) -> f32 {
    let size = image.size(0);
    size.width.0.max(1) as f32 / size.height.0.max(1) as f32
}

/// Which of the generated cover's fixed jacket colours a book with no art
/// gets. Keyed off the content hash rather than drawn at random, so a book
/// generates the same cover every time it is shown.
fn generated_cover_variant(content_hash: sync_common::ContentHash) -> usize {
    content_hash.as_str().bytes().fold(0usize, |acc, byte| acc.wrapping_add(byte as usize))
}

/// Short enough to fit on a cover while still distinguishing, for example,
/// a seven-hour book from one that runs nearly eight hours.
fn audiobook_card_length(duration_ms: u64) -> String {
    let total_minutes = (duration_ms / 60_000).max(1);
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    match (hours, minutes) {
        (0, minutes) => format!("{minutes} m"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} m"),
    }
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
    let image = Arc::new(RenderImage::new(vec![frame]));
    Some(LoadedCover { image })
}

#[derive(Clone)]
enum CoverState {
    Pending,
    Loaded(LoadedCover, app::ThumbnailResolution),
    Missing,
}

pub(crate) struct BookCardView {
    book: BookCardRow,
    /// Whether this card holds the paginator's keyboard cursor. Pushed in by
    /// the page rather than read out of anything: the card knows what focused
    /// looks like, not where the cursor is.
    focused: bool,
    /// Whether a rubber band has picked this card up. Painted exactly like
    /// `focused` — the two cannot coexist, since moving the cursor drops the
    /// selection — but held apart, because a selected card hands its secondary
    /// click to the page's menu over the whole selection.
    selected: bool,
    /// Whether a rubber band is currently sweeping the grid. Suppresses the
    /// card's hover style: a band drags the pointer across every card it picks
    /// up, and a card answering that with hover reports where the pointer went
    /// rather than what is selected.
    marquee: bool,
    /// Whether the card is drawn in its detailed mode: twice as wide, with the
    /// blurb beside the cover instead of nothing but the title under it. Pushed
    /// in by the page, exactly like the cursor — the grid decides how wide a
    /// cell is, and the card decides what to put in one.
    detailed: bool,
    /// A cover carousel can reuse the book's cover loading and activation
    /// behavior without adding title or author text around the artwork.
    cover_only: bool,
    /// Section covers keep their total padding, with the artwork optically
    /// aligned to the section heading when a page starts with books.
    section_cover_left_aligned: bool,
    cover_top_aligned: bool,
    /// When the artwork arrived, which is what the fade is computed from.
    ///
    /// A timestamp rather than an element animation: the paginator repaints its
    /// children on every cursor move, and an animation keyed to an element
    /// restarts each time that element is rebuilt — which is what made covers
    /// flash. Deriving opacity from *when the cover loaded* is unaffected by how
    /// often the card is drawn.
    cover_loaded_at: Option<Instant>,
    cover: CoverState,
    ui: LibraryContext,
    /// How this card asks for the book's page. Absent on cards that are not in
    /// a browse grid — an author's shelf, the home page — which still open the
    /// reader directly until those pages route through it too.
    open_detail: Option<OpenBookDetail>,
    cover_task: Option<Task<()>>,
    cover_updates: Option<Subscription>,
    _download_updates: Subscription,
    download_task: Option<Task<()>>,
    source_directory: Option<DirId>,
    place_book: Option<PlaceBook>,
    remove_book: Option<RemoveBook>,
    context_menu: Option<BookCardContextMenu>,
    context_menu_subscription: Option<Subscription>,
    /// Repaints the card when the "cover text" setting changes, so an
    /// already-visible card hides or shows its title without waiting for
    /// some unrelated event to trigger a redraw.
    _cover_text_updates: Subscription,
}

struct BookCardContextMenu {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
}

impl BookCardView {
    pub(crate) fn title(&self) -> &str {
        &self.book.title
    }
    pub(crate) fn thumbnail_resolution(window: &Window) -> app::ThumbnailResolution {
        let logical_width = gpui::px(components::BOOK_CARD_CONTENT_WIDTH);
        let physical_width = f32::from(logical_width) * window.scale_factor();
        if physical_width <= app::ThumbnailResolution::Browse.width() as f32 { app::ThumbnailResolution::Browse } else { app::ThumbnailResolution::HighDensity }
    }

    pub(crate) fn create(row: BookCardRow, ui: LibraryContext, cx: &mut impl AppContext) -> Entity<Self> {
        cx.new(|cx| Self::new(row, ui, None, cx))
    }

    pub(crate) fn create_in_folder(row: BookCardRow, ui: LibraryContext, source_directory: DirId, cx: &mut impl AppContext) -> Entity<Self> {
        cx.new(|cx| Self::new(row, ui, Some(source_directory), cx))
    }

    /// A card in a browse grid, which is what gives it somewhere to navigate to.
    pub(crate) fn create_in_grid(row: BookCardRow, ui: LibraryContext, source_directory: Option<DirId>, open_detail: OpenBookDetail, place_book: Option<PlaceBook>, remove_book: Option<RemoveBook>, cx: &mut impl AppContext) -> Entity<Self> {
        cx.new(|cx| {
            let mut card = Self::new(row, ui, source_directory, cx);
            card.open_detail = Some(open_detail);
            card.place_book = place_book;
            card.remove_book = remove_book;
            card
        })
    }

    fn new(row: BookCardRow, ui: LibraryContext, source_directory: Option<DirId>, cx: &mut Context<Self>) -> Self {
        let cover_updates = Self::subscribe_to_cover_updates(&ui, row.content_hash, cx);
        ui.updates().update(cx, |updates, cx| updates.track_download(row.content_hash, row.downloaded, row.download_requested, cx));
        let download_updates = Self::subscribe_to_download_updates(&ui, row.content_hash, cx);
        let cover_text_updates = cx.observe_global::<crate::stores::CoverTextSetting>(|_, cx| cx.notify());
        Self {
            book: row,
            focused: false,
            selected: false,
            marquee: false,
            detailed: false,
            cover_only: false,
            section_cover_left_aligned: false,
            cover_top_aligned: false,
            cover_loaded_at: None,
            cover: CoverState::Pending,
            ui,
            open_detail: None,
            cover_task: None,
            cover_updates: Some(cover_updates),
            _download_updates: download_updates,
            download_task: None,
            source_directory,
            place_book: None,
            remove_book: None,
            context_menu: None,
            context_menu_subscription: None,
            _cover_text_updates: cover_text_updates,
        }
    }

    pub(crate) fn content_hash(&self) -> sync_common::ContentHash {
        self.book.content_hash
    }

    /// Repaints only on a real change: the page pushes the cursor to every card
    /// on every move, so most of these calls are for cards that are already in
    /// the state being set.
    pub(crate) fn set_focused(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        cx.notify();
    }

    pub(crate) fn set_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        if self.selected == selected {
            return;
        }
        self.selected = selected;
        cx.notify();
    }

    pub(crate) fn set_marquee(&mut self, marquee: bool, cx: &mut Context<Self>) {
        if self.marquee == marquee {
            return;
        }
        self.marquee = marquee;
        cx.notify();
    }

    /// Switches the card between its two modes. The paginator has to be told
    /// separately, because the cell's width is the grid's business — a card
    /// drawn detailed in an ordinary cell would have no room for the blurb.
    pub(crate) fn set_detailed(&mut self, detailed: bool, cx: &mut Context<Self>) {
        if self.detailed == detailed {
            return;
        }
        self.detailed = detailed;
        cx.notify();
    }

    pub(crate) fn set_cover_only(&mut self, cover_only: bool, cx: &mut Context<Self>) {
        if self.cover_only == cover_only {
            return;
        }
        self.cover_only = cover_only;
        cx.notify();
    }

    pub(crate) fn set_section_cover_left_aligned(&mut self, aligned: bool, cx: &mut Context<Self>) {
        if self.section_cover_left_aligned == aligned {
            return;
        }
        self.section_cover_left_aligned = aligned;
        cx.notify();
    }

    pub(crate) fn set_cover_top_aligned(&mut self, aligned: bool, cx: &mut Context<Self>) {
        if self.cover_top_aligned == aligned {
            return;
        }
        self.cover_top_aligned = aligned;
        cx.notify();
    }

    pub(crate) fn replace_row(&mut self, row: BookCardRow, source_directory: Option<DirId>) {
        debug_assert_eq!(self.book.content_hash, row.content_hash);
        self.book = row;
        self.source_directory = source_directory;
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let content_hash = self.book.content_hash;
        if !matches!(self.ui.updates().read(cx).download_state(content_hash), Some(app::DownloadState::NotDownloaded)) || self.download_task.is_some() {
            return;
        }
        self.ui.updates().update(cx, |updates, cx| updates.watch_download(content_hash, cx));
        let library = self.ui.backend().clone();
        let operation = async move { library.download_book(content_hash).await };
        self.download_task = Some(cx.spawn(async move |card, cx| {
            let result = operation.await;
            let _ = card.update_in(cx, |card, window, cx| {
                card.download_task.take();
                if let Err(error) = result {
                    crate::services::notify_error("book-download-error", format!("Could not download book: {error}"), window, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn subscribe_to_cover_updates(ui: &LibraryContext, content_hash: sync_common::ContentHash, cx: &mut Context<Self>) -> Subscription {
        let updates = ui.updates();
        cx.subscribe(&updates, move |view, _, event: &LibraryCoversChanged, cx| {
            if event.contains(&content_hash) {
                view.thumbnail_available(cx);
            }
        })
    }

    fn subscribe_to_download_updates(ui: &LibraryContext, content_hash: sync_common::ContentHash, cx: &mut Context<Self>) -> Subscription {
        let updates = ui.updates();
        cx.subscribe(&updates, move |view, _, event: &LibraryDownloadChanged, cx| {
            if event.status.content_hash == content_hash {
                cx.notify();
            }
        })
    }

    fn thumbnail_available(&mut self, cx: &mut Context<Self>) {
        if matches!(self.cover, CoverState::Loaded(..)) {
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
        if self.cover_updates.is_some() {
            return;
        }
        self.cover_updates = Some(Self::subscribe_to_cover_updates(&self.ui, self.book.content_hash, cx));
    }

    fn load_cover(&mut self, resolution: app::ThumbnailResolution, cx: &mut Context<Self>) {
        let content_hash = self.book.content_hash;
        let library = self.ui.backend().clone();
        let load = async move { library.thumbnail(content_hash, resolution).await };
        let decoder = cx.background_executor().clone();
        self.cover_task = Some(cx.spawn(async move |view, cx| {
            let result = match load.await {
                Ok(Some(bytes)) => decoder.spawn(async move { loaded_cover_from_jpeg(bytes) }).await.map(Some).ok_or_else(|| format!("thumbnail for book {content_hash} is not a readable JPEG")),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            cx.defer_update(view, move |view, cx| {
                view.cover_task.take();
                if view.book.content_hash != content_hash {
                    return;
                }
                match result {
                    Ok(Some(cover)) => {
                        // Only the first arrival fades; a higher-resolution
                        // thumbnail replacing a lower one is not a new cover.
                        view.cover_loaded_at.get_or_insert_with(Instant::now);
                        view.cover = CoverState::Loaded(cover, resolution);
                        view.cover_updates.take();
                    }
                    Ok(None) => {
                        view.cover = CoverState::Missing;
                        view.listen_for_thumbnail(cx);
                    }
                    Err(error) => {
                        log::error!("failed to load thumbnail for book {content_hash}: {error}");
                        view.cover = CoverState::Missing;
                        view.listen_for_thumbnail(cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn prepare(&mut self, resolution: app::ThumbnailResolution, cx: &mut Context<Self>) {
        let needs_cover = match &self.cover {
            CoverState::Pending => true,
            CoverState::Loaded(_, loaded_resolution) => *loaded_resolution == app::ThumbnailResolution::Browse && resolution == app::ThumbnailResolution::HighDensity,
            CoverState::Missing => false,
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
        let cover = match &self.cover {
            CoverState::Loaded(cover, _) => Some(cover.clone()),
            CoverState::Pending | CoverState::Missing => None,
        };
        (self.book.clone(), cover)
    }

    /// Activating a card goes to the book's page, not into the book. Reading is
    /// one deliberate step further in, from the page's own Read button, so that
    /// a click on a cover can mean "tell me about this" — which is what it
    /// usually means in a library.
    /// Takes what it needs out of the card and lets go of it before acting.
    ///
    /// Deliberately not `card.update(|card, cx| …)`: going to the book's page is
    /// navigation, and the load that follows touches every card in the grid —
    /// their cursor, their card mode — including this one. Holding this card
    /// leased across that is a panic, and was one.
    pub(crate) fn activate(card: &Entity<Self>, window: &mut Window, cx: &mut gpui::App) {
        let (book, cover, open_detail, ui) = {
            let card = card.read(cx);
            let (book, cover) = card.detail_source();
            (book, cover, card.open_detail.clone(), card.ui.clone())
        };
        match open_detail {
            Some(open_detail) => open_detail(book, cover, cx),
            None => ui.open_book(book.content_hash, book.title, window, cx),
        }
    }

    /// Runs a confirmed book command from the card menu: one prompt, then the
    /// backend call, with failures surfaced as toasts.
    fn confirm_menu_action(
        target: Entity<Self>, title: String, detail: String, choices: [&str; 2], error_id: &'static str, error_prefix: String, operation: impl std::future::Future<Output = Result<(), String>> + 'static, window: &mut Window, cx: &mut App,
    ) {
        let answer = window.prompt(PromptLevel::Warning, &title, Some(&detail), &choices, cx);
        target.update(cx, |_, cx| {
            cx.spawn(async move |card, cx| {
                if answer.await.ok() != Some(0) {
                    return;
                }
                if let Err(error) = operation.await {
                    let _ = card.update_in(cx, |_, window, cx| {
                        crate::services::notify_error(error_id, format!("{error_prefix}: {error}"), window, cx);
                    });
                }
            })
            .detach();
        });
    }

    fn open_context_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let mobile_size = mobile_context_menu::size(window);
        let new_window = crate::OpenInNewWindow(crate::NewWindowTarget::Book { locator: library_model::BookLocator::new(*self.ui.backend().id(), self.book.content_hash), title: self.book.title.clone() });
        let source_directory = self.source_directory;
        let content_hash = self.book.content_hash;
        let title = self.book.title.clone();
        let removal_target = cx.entity();
        let backend = self.ui.backend().clone();
        let downloaded = matches!(self.ui.updates().read(cx).download_state(content_hash), Some(app::DownloadState::Downloaded));
        let place_book = self.place_book.clone();
        let remove_book = self.remove_book.clone();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if crate::native_windows_available() {
                let action = new_window.clone();
                menu = menu
                    .item(components::menu_item("Open in New Window", move |_, window, cx| {
                        window.dispatch_action(Box::new(action.clone()), cx);
                    }))
                    .separator();
            }
            if let (Some(source_directory), Some(place_book)) = (source_directory, place_book) {
                let copy_book = place_book.clone();
                let move_book = place_book;
                let content_hash = content_hash;
                menu = menu
                    .item(components::menu_item_with_icon("Copy to folder…", IconName::Copy, move |_, _, cx| copy_book(content_hash, source_directory, BookPlacement::Copy, cx)))
                    .item(components::menu_item_with_icon("Move to folder…", IconName::FolderOpen, move |_, _, cx| move_book(content_hash, source_directory, BookPlacement::Move, cx)))
                    .separator();
            }
            if downloaded {
                let library = backend.clone();
                let evict_target = removal_target.clone();
                menu = menu
                    .item(components::menu_item_with_icon("Remove download", IconName::HardDrive, move |_, _window, cx| {
                        let library = library.clone();
                        let operation = async move { library.remove_local_book_copy(content_hash).await.map(|_| ()) };
                        evict_target.update(cx, |_, cx| {
                            cx.spawn(async move |card, cx| {
                                if let Err(error) = operation.await {
                                    let _ = card.update_in(cx, |_, window, cx| {
                                        crate::services::notify_error("book-evict-error", format!("Could not remove download: {error}"), window, cx);
                                    });
                                }
                            })
                            .detach();
                        });
                    }))
                    .separator();
            }
            // Leaving a folder and leaving the library are separate commands:
            // the first keeps the book in its other folders, the second moves
            // it to Trash outright.
            if source_directory.is_some() {
                if let (Some(source_directory), Some(remove_book)) = (source_directory, remove_book) {
                    let description = format!("“{title}” stays in its other folders. If this is its last folder, it moves to Trash.");
                    menu = menu.item(components::menu_item_with_icon("Remove from folder", components::navigation_icon(components::NavigationIcon::Folder), move |_, window, cx| {
                        remove_book(content_hash, source_directory, description.clone(), window, cx);
                    }));
                } else if let Some(source_id) = source_directory {
                    let library = backend.clone();
                    let direct_target = removal_target.clone();
                    let detail = format!("“{title}” stays in its other folders. If this is its last folder, it moves to Trash.");
                    menu = menu.item(components::menu_item_with_icon("Remove from folder", components::navigation_icon(components::NavigationIcon::Folder), move |_, window, cx| {
                        let library = library.clone();
                        let operation = async move { library.remove_book_from_directory(content_hash, source_id).await.map(|_| ()) };
                        Self::confirm_menu_action(
                            direct_target.clone(),
                            "Remove this book from the folder?".to_owned(),
                            detail.clone(),
                            ["Remove", "Cancel"],
                            "book-remove-error",
                            "Could not remove book from folder".to_owned(),
                            operation,
                            window,
                            cx,
                        );
                    }));
                }
                let library = backend.clone();
                let trash_target = removal_target.clone();
                let detail = format!("“{title}” will be moved to Trash.");
                menu = menu.item(components::menu_item_with_icon("Move to Trash", components::navigation_icon(components::NavigationIcon::Trash2), move |_, window, cx| {
                    let library = library.clone();
                    let operation = async move { library.move_book_to_trash(content_hash).await };
                    Self::confirm_menu_action(trash_target.clone(), "Move this book to Trash?".to_owned(), detail.clone(), ["Move to Trash", "Cancel"], "book-trash-error", "Could not move book to Trash".to_owned(), operation, window, cx);
                }));
            } else {
                let library = backend.clone();
                let trash_target = removal_target.clone();
                let detail = format!("“{title}” will be moved to Trash.");
                menu = menu.item(components::menu_item_with_icon("Move to Trash", components::navigation_icon(components::NavigationIcon::Trash2), move |_, window, cx| {
                    let library = library.clone();
                    let operation = async move { library.move_book_to_trash(content_hash).await };
                    Self::confirm_menu_action(trash_target.clone(), "Move this book to Trash?".to_owned(), detail.clone(), ["Move to Trash", "Cancel"], "book-trash-error", "Could not move book to Trash".to_owned(), operation, window, cx);
                }));
            }
            mobile_context_menu::configure(menu, mobile_size)
        });
        let subscription = cx.subscribe(&menu, |card, _, _: &DismissEvent, cx| {
            card.context_menu = None;
            card.context_menu_subscription = None;
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        self.context_menu = Some(BookCardContextMenu { menu, position });
        self.context_menu_subscription = Some(subscription);
        cx.notify();
    }
}

impl Render for BookCardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare(Self::thumbnail_resolution(window), cx);
        let theme = components::browser_theme(cx);
        let highlighted = self.focused || self.selected;
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
        let cover = self.cover.clone();
        let element_id = format!("book-card-{}", book.content_hash);
        let cover_id = format!("book-cover-{}", book.content_hash);

        let cover_text = if self.cover_only {
            app_preferences::CoverText::CoverOnly
        } else if self.detailed {
            app_preferences::CoverText::Always
        } else {
            crate::stores::current_cover_text(cx)
        };
        let open_target = cx.entity();
        let cover_ratio = match &cover {
            CoverState::Loaded(cover, _) => cover_aspect_ratio(&cover.image),
            CoverState::Pending | CoverState::Missing if book.format_category == library_model::LibraryFileTypeFilter::Audiobook => 1.0,
            CoverState::Pending | CoverState::Missing => components::BOOK_COVER_ASPECT_RATIO,
        };
        // A book not on this device says so on its cover, in the corner nothing
        // else uses, and the mark is the way to fetch it. A downloaded book
        // carries no mark: which state is the rarer one differs from device to
        // device, and marking only the one that needs acting on keeps a mostly
        // downloaded library quiet and a mostly remote one legible.
        let download_state = self.ui.updates().read(cx).download_state(book.content_hash).unwrap_or(app::DownloadState::Queued);
        let download_badge = match download_state {
            app::DownloadState::Downloaded => None,
            app::DownloadState::Queued | app::DownloadState::Downloading(_) => Some(components::book_cover_download_badge(format!("book-download-{}", book.content_hash), true)),
            app::DownloadState::NotDownloaded => {
                let download_target = cx.entity();
                Some(
                    components::book_cover_download_badge(format!("book-download-{}", book.content_hash), false)
                        .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new("Download to this device").build(window, cx))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            download_target.update(cx, |card, cx| card.download(cx));
                        }),
                )
            }
        };
        let cover_frame = components::book_cover_frame(theme, cover_ratio)
            .id(cover_id)
            .on_click(move |_, window, cx| Self::activate(&open_target, window, cx))
            .child(match cover {
                CoverState::Loaded(cover, _) => components::book_cover(theme, cover_ratio).child(components::book_cover_image(format!("book-cover-art-{}", book.content_hash), cover.image, cover_opacity, highlighted, theme)),
                CoverState::Pending => components::book_cover(theme, cover_ratio),
                // A blank placeholder says nothing without the title line
                // under it, so "cover only" replaces it with a generated
                // cover that carries the title itself.
                CoverState::Missing if cover_text == app_preferences::CoverText::CoverOnly => components::book_cover_generated(book.title.clone(), book.subtitle.as_deref(), generated_cover_variant(book.content_hash), theme).aspect_ratio(cover_ratio),
                CoverState::Missing => components::missing_book_cover("No cover", theme).aspect_ratio(cover_ratio),
            })
            .when(!self.cover_only, |frame| frame.children(components::book_cover_progress(book.progress, theme)).children(download_badge))
            .when(!self.cover_only && book.format_category == library_model::LibraryFileTypeFilter::Audiobook, |frame| {
                frame.child(components::book_cover_audiobook_length(book.audiobook_duration_ms.map(audiobook_card_length)))
            });

        let context_menu = self.context_menu.as_ref().map(|active| mobile_context_menu::render(active.menu.clone(), active.position, window));
        let context_menu_target = cx.entity();
        let section_cover = self.section_cover_left_aligned;
        let card = components::book_card(element_id, highlighted, self.marquee, theme)
            .when(self.section_cover_left_aligned, |card| card.pl(px(4.0)).pr(px(components::BOOK_CARD_PADDING * 2.0 - 4.0)))
            .on_mouse_down(gpui::MouseButton::Right, move |_, _, cx| {
                if section_cover {
                    cx.stop_propagation();
                }
            })
            .on_aux_click(move |event, window, cx| {
                // A card inside a selection is not the subject of its own menu:
                // the page opens one over everything the band picked up.
                if event.is_secondary() && !context_menu_target.read(cx).selected {
                    context_menu_target.update(cx, |card, cx| card.open_context_menu(event.position(), window, cx));
                    cx.stop_propagation();
                }
            });

        // The two modes differ in where the cover stands, so each lays the card
        // out itself rather than sharing a stack the cover is threaded into.
        let card = if self.detailed {
            let description = components::book_card_description(format!("book-description-{}", book.content_hash), &book.description, theme);
            card.child(
                components::book_card_detail_body()
                    .child(components::book_card_detail_cover().child(cover_frame).child(components::book_card_text().child(components::book_card_title_row(book.title, book.subtitle.as_deref()))))
                    .child(components::book_card_detail_text().children(description)),
            )
        } else {
            let card = card.child(cover_frame);
            // No text block at all under "cover only", not an empty one: an
            // empty child would still hold the cover-to-text gap open under
            // a cover that has nothing under it. The download mark is on the
            // cover, so it stays.
            match cover_text {
                app_preferences::CoverText::Always => card.child(components::book_card_text_title_only().child(components::book_card_title_row(book.title, book.subtitle.as_deref()))),
                app_preferences::CoverText::CoverOnly => card,
            }
        };

        components::book_card_cell()
            .when(self.cover_top_aligned, |cell| cell.justify_start())
            .child(card.children(context_menu))
            .into_any_element()
    }
}
