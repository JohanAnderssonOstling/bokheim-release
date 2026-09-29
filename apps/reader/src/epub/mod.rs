//! The EPUB reader surface.

pub(crate) mod marks;
pub(crate) mod progress;
pub(crate) mod render;
pub(crate) mod search;
pub(crate) mod state;
pub(crate) mod toc;
pub(crate) mod toolbar;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use gpui::{Entity, FocusHandle, Pixels, Point, Subscription};
use gpui_component::input::InputState;
use gpui_component::tree::TreeState;
use html_view_core::SearchOptions;
use html_view_gpui::HtmlView;
use library_backend::LibraryClient;
use library_model::BookLocator;

use crate::epub::toc::TocState;
use crate::shell::marks::MarkSession;
use crate::shell::persistence::ReadingPositionWriter;
use crate::shell::state_panel::SidebarState;

enum LoadState {
    Loading,
    Error(String),
    Ready(Entity<HtmlView>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Selection {
    cfi_range: String,
    text: String,
}

#[derive(Debug, PartialEq)]
struct ProgressState {
    fraction: f32,
    doc_fraction: f32,
    location: u64,
    total_locations: u64,
    section: usize,
    section_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProgressSummary {
    percent: u8,
    location: u64,
    total_locations: u64,
    section: usize,
    section_count: usize,
}

struct SearchState {
    /// `None` while search is closed; `Some` owns the active query.
    query: Option<String>,
    pending: bool,
    cancellation: Arc<AtomicU64>,
    options: SearchOptions,
    match_position: Option<(usize, usize)>,
}

pub(crate) struct ReaderView {
    #[cfg(feature = "kobo")]
    brightness: Entity<crate::shell::brightness::BrightnessControls>,
    locator: BookLocator,
    library: Rc<LibraryClient>,
    book_title: String,
    title: String,
    load_state: LoadState,
    applied_palette: Option<ui_components::theme::SelectedPalette>,
    desired_column_count: u8,
    toc: TocState,
    toc_tree: Entity<TreeState>,
    sidebar: SidebarState,
    chrome: crate::shell::chrome::ChromeState,
    /// A wheel or trackpad burst is one gesture: every event postpones the turn,
    /// and once input goes quiet the net direction advances exactly one page.
    /// Same shape as the browse paginator, so both surfaces feel alike.
    wheel_delta: f32,
    wheel_debounce: Option<gpui::Task<()>>,
    touch_swipe_active: bool,
    search_input: Entity<InputState>,
    search: SearchState,
    book: Arc<crate::book::NativeBook>,
    current_cfi: Option<String>,
    progress: ProgressState,
    marks: MarkSession,
    /// The text the reader has selected, which only an EPUB anchors by CFI.
    selection: Option<Selection>,
    annotation_note_input: Entity<InputState>,
    annotation_popup_anchor: Option<Point<Pixels>>,
    footnote: Option<(html_view_core::FootnotePreview, html_view_gpui::PreparedNote)>,
    image_preview: Option<(String, Arc<gpui::Image>)>,
    context_sheet: Option<crate::shell::context_sheet::ContextActions>,
    focus: FocusHandle,
    renderer_subscription: Option<Subscription>,
    document_tap_subscription: Option<Subscription>,
    reading_positions: ReadingPositionWriter,
    close_reader: crate::CloseReader,
    _search_subscription: Subscription,
}
