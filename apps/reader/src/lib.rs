//! GPUI reader surfaces embedded by the application shell.

/// Records a book-open startup phase against the `book_startup` log target.
///
/// Book launch is profiled phase by phase on slow e-ink hardware. Routing the
/// trace through `log` keeps it off stdout in normal runs while leaving one
/// named seam to redirect if the device workflow needs something else.
macro_rules! startup_phase {
    ($($arg:tt)*) => {
        log::debug!(target: "book_startup", $($arg)*)
    };
}

mod book;
mod epub;
mod invalidation;
mod pdf;
mod settings;
mod shell;
mod windows;

use gpui::actions;

type CloseReader = std::rc::Rc<dyn Fn(&mut gpui::Window, &mut gpui::App)>;

pub use app_preferences::PdfZoomMode;
pub use app_preferences::{ReaderFontFamily, ReaderTextAlignment};
#[cfg(feature = "audiobooks")]
pub use audiobook_player::{ActiveAudiobook, AudiobookDock, DockAction, PlaybackSession};
pub use settings::{ReaderPreferences, SaveReaderSettings};
#[cfg(feature = "audiobooks")]
pub use windows::AudiobookOpened;
pub use windows::{CloseRequested, ReaderView, configure};

#[cfg(feature = "audiobooks")]
pub fn restore_audiobook(record: &ActiveAudiobook, library: library_backend::LibraryClient, cx: &mut gpui::App) -> gpui::Entity<PlaybackSession> {
    use gpui::AppContext;
    let save_speed = std::rc::Rc::new(|speed, cx: &mut gpui::App| {
        settings::ReaderSettings::update(cx, |preferences| preferences.audiobook_speed = speed);
    });
    cx.new(|cx| PlaybackSession::restore(record, library, save_speed, cx))
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
        FocusReaderSearch,
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
