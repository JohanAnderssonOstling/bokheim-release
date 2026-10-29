//! GPUI reader surfaces embedded by the application shell.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod book;
mod cjk;
mod epub;
mod invalidation;
mod pdf;
mod comic;
mod settings;
mod shell;
mod windows;

use gpui::actions;

type CloseReader = std::rc::Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>;

pub use app_preferences::PdfZoomMode;
pub use app_preferences::{ReaderFontFamily, ReaderTextAlignment};
pub use audiobook_player::{ActiveAudiobook, AudiobookDock, DockAction, PlaybackSession};
pub use settings::{CjkFontChoice, ReaderPreferences, SaveReaderSettings, SystemFontsLoading};
pub use windows::AudiobookOpened;
pub use windows::{BookOpenFailed, CloseRequested, ReaderView, configure};

fn save_audiobook_speed(speed: f64, cx: &mut gpui::App) {
    settings::ReaderSettings::update(cx, |preferences| preferences.audiobook_speed = speed);
}

pub fn open_audiobook(locator: library_model::BookLocator, library: library_backend::LibraryClient, resolved: library_backend::ResolvedBook, initial_target: Option<String>, cx: &mut gpui::App) -> gpui::Entity<PlaybackSession> {
    use gpui::AppContext;
    let speed = settings::ReaderSettings::preferences(cx).audiobook_speed;
    cx.new(|cx| PlaybackSession::open(locator, library, resolved, initial_target, speed, save_audiobook_speed, cx))
}

pub fn restore_audiobook(record: &ActiveAudiobook, library: library_backend::LibraryClient, cx: &mut gpui::App) -> gpui::Entity<PlaybackSession> {
    use gpui::AppContext;
    cx.new(|cx| PlaybackSession::restore(record, library, save_audiobook_speed, cx))
}

actions!(
    bokheim_reader,
    [
        PreviousPage,
        NextPage,
        PreviousLine,
        NextLine,
        PreviousSection,
        NextSection,
        ToggleToc,
        ToggleSidebar,
        NextSidebarTab,
        PreviousSidebarTab,
        FocusReaderSearch,
        OpenReaderSettings,
        ToggleSearch,
        CloseSearch,
        IncreaseFont,
        DecreaseFont,
        IncreaseScale,
        DecreaseScale,
        IncreaseColumnWidth,
        DecreaseColumnWidth,
        ToggleFullscreen,
        ReturnToLibrary
    ]
);

pub fn bind_keys(cx: &mut gpui::App) {
    shell::input::bind_reader_keys(cx);
    epub::render::bind_keys(cx);
    pdf::bind_pdf_keys(cx);
    shell::input::bind_document_keys(cx, "ComicReaderDocument");
}

/// Returns debug-only invalidation counts grouped by component and reason.
#[cfg(debug_assertions)]
pub fn notification_counts() -> Vec<((invalidation::Component, &'static str), u64)> {
    invalidation::snapshot()
}

#[cfg(debug_assertions)]
pub use invalidation::{Component, ReaderPerformanceSnapshot};

/// Returns reader invalidation and render counts accumulated since the previous
/// reset.
#[cfg(debug_assertions)]
pub fn performance_snapshot() -> ReaderPerformanceSnapshot {
    invalidation::performance_snapshot()
}

#[cfg(debug_assertions)]
pub fn reset_performance_measurements() {
    invalidation::reset_performance_measurements();
}

#[cfg(feature = "kobo")]
pub use shell::brightness::{BrightnessControls, popover as brightness_popover, popover_with_height as brightness_popover_with_height};

#[cfg(not(target_arch = "wasm32"))]
pub use html_view_gpui::configure_font_cache;
