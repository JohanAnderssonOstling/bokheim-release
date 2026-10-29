//! The EPUB reader surface.

mod font_picker;
pub(crate) mod marks;
pub(crate) mod progress;
pub(crate) mod render;
pub(crate) mod search;
pub(crate) mod state;
pub(crate) mod toc;
pub(crate) mod toolbar;
#[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
mod tts;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use gpui::{Entity, FocusHandle, ListState, Pixels, Point, Size, Subscription};
use gpui_component::input::InputState;
use gpui_component::tree::TreeState;
use skrift_column_paginator::{ReadingPosition, SearchOptions};
use html_view_gpui::{HtmlPageSnapshot, HtmlView};
use library_backend::LibraryClient;
use library_model::BookLocator;
use ui_components::PageTurn;

use crate::epub::toc::TocState;
use crate::shell::context_sheet::ContextActions;
use crate::shell::marks::MarkSession;
use crate::shell::persistence::ReadingPositionWriter;
use crate::shell::state_panel::SidebarState;

enum LoadState {
    Loading,
    Error(String),
    Ready(Entity<HtmlView>),
}

#[derive(Clone, Copy)]
pub(crate) enum TableCopyFormat {
    Markdown,
    UnstyledHtml,
    StyledHtml,
}

#[derive(Clone)]
pub(crate) enum EpubContextCommand {
    CopyTable { view: Entity<HtmlView>, position: Point<Pixels>, format: TableCopyFormat },
    CopyImage { view: Entity<HtmlView>, position: Point<Pixels> },
    CopyEquationLatex { view: Entity<HtmlView>, position: Point<Pixels> },
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
    /// The matches the Search panel lists, each with its excerpt.
    results: Vec<skrift_column_paginator::BookSearchMatch>,
    rows: Vec<search::SearchRow>,
    list: ListState,
}

pub(crate) struct ReaderView {
    #[cfg(feature = "kobo")]
    device_controls: Option<gpui::AnyView>,
    locator: BookLocator,
    library: Rc<LibraryClient>,
    book_title: String,
    title: String,
    load_state: LoadState,
    applied_palette: Option<ui_components::theme::SelectedPalette>,
    last_column_viewport_plan: Option<(gpui::EntityId, f64, f64, u8, f64)>,
    desired_column_count: u8,
    toc: TocState,
    toc_tree: Entity<TreeState>,
    annotation_tree: crate::shell::annotation_tree::AnnotationTreeState,
    sidebar: SidebarState,
    chrome: crate::shell::chrome::ChromeState,
    /// A wheel burst is one gesture: every event postpones the turn,
    /// and once input goes quiet the net direction advances exactly one page.
    wheel_delta: f32,
    wheel_debounce: Option<gpui::Task<()>>,
    touchpad: ui_components::TouchpadNavigation,
    /// How far the finger has travelled on both axes, while a touch swipe is
    /// in progress.
    touch_swipe: Option<gpui::Point<f32>>,
    swipe_preview: ui_components::SwipePreview,
    page_turn: Option<PageTurn<HtmlPageSnapshot>>,
    page_size: Option<Size<Pixels>>,
    /// The swipe began in the bottom gesture area, so it is the system's and
    /// turns no page.
    touch_swipe_from_edge: bool,
    search_input: Entity<InputState>,
    font_picker: Entity<font_picker::FontPicker>,
    cjk_font_picker: Entity<font_picker::FontPicker>,
    search: SearchState,
    book: Arc<crate::book::NativeBook>,
    cjk_font: Option<crate::cjk::CjkFont>,
    reading_position: Option<ReadingPosition>,
    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    tts: crate::shell::tts::Narration,
    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    tts_fragments: Vec<html::layout::ReadingTarget>,
    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    tts_doc: usize,
    #[cfg(all(any(feature = "native-tts", target_arch = "wasm32"), not(feature = "kobo")))]
    tts_navigation_revision: u64,
    progress: ProgressState,
    marks: MarkSession,
    /// The text the reader has selected, which only an EPUB anchors by CFI.
    selection: Option<Selection>,
    touch_selection: render::touch_selection::TouchSelectionState,
    annotation_note_input: Entity<InputState>,
    annotation_filter_input: Entity<InputState>,
    annotation_popup_anchor: Option<Point<Pixels>>,
    footnote: Option<(skrift_column_paginator::FootnotePreview, html_view_gpui::PreparedNote)>,
    image_preview: Option<(String, Arc<gpui::Image>)>,
    context_sheet: Option<ContextActions<EpubContextCommand>>,
    focus: FocusHandle,
    sidebar_focus: FocusHandle,
    rail_focus: FocusHandle,
    settings_navigation: crate::shell::settings_navigation::SettingsNavigation,
    renderer_subscription: Option<Subscription>,
    document_tap_subscription: Option<Subscription>,
    reading_positions: ReadingPositionWriter,
    close_reader: crate::CloseReader,
    _search_subscription: Subscription,
    _annotation_filter_subscription: Subscription,
}
