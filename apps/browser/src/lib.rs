//! GPUI library-browsing window.
//!
//! [`root::BrowserRoot`] owns window policy. Navigation owns routing,
//! [`navigation::Libraries`] owns library lifecycle and per-library
//! pages, and global pages remain stable across library selection.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

mod library;
mod model;
mod navigation;
mod root;
mod services;
mod session;
mod stores;
mod universal;

pub(crate) const BOOK_IMPORT_EXTENSIONS: [&str; 10] = ["epub", "pdf", "mobi", "azw", "azw3", "m4b", "fb2", "fbz", "zip", "cbz"];

use gpui::{App, KeyBinding, Window, actions};
pub use gpui_component::menu::OpenContextMenu;
use library::CursorMove;
use library_backend::{DirectoryImport, DirectoryImportFile, ImportSource};

/// Set by the Android host before opening any settings or libraries.
#[cfg(target_os = "android")]
pub struct AndroidStoragePolicy {
    pub all_files_access_available: bool,
}

#[cfg(target_os = "android")]
impl gpui::Global for AndroidStoragePolicy {}

pub(crate) fn split_panes_available(window: &gpui::Window) -> bool {
    native_windows_available() && !ui_components::WindowWidthClass::for_window(window).is_compact()
}

pub fn native_windows_available() -> bool {
    !cfg!(any(target_arch = "wasm32", target_os = "android", target_os = "ios", feature = "mobile"))
}

#[derive(Clone, Debug, PartialEq)]
pub enum NewWindowTarget {
    Book { locator: library_model::BookLocator, title: String },
    Folder { library_id: sync_common::LibraryId, directory_id: sync_common::DirId, title: String },
    Subject { library_id: sync_common::LibraryId, location: library_model::BrowseLocation, label: String },
}

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub struct OpenInNewWindow(pub NewWindowTarget);

/// Opens one author's books on the Authors page, from wherever their name is
/// shown.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub struct OpenAuthor {
    pub author: book_model::AuthorId,
    pub name: String,
}

/// Opens a subject page — the library's or the catalog's, by the location
/// its crumbs carry — at the end of `path`, which runs from the root down.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub struct OpenSubject {
    pub path: Vec<library_model::BrowsePathSegment>,
}

fn directory_import(selected: gpui::SelectedDirectory) -> DirectoryImport {
    DirectoryImport {
        name: selected.name,
        directories: selected.directories,
        files: selected
            .files
            .into_iter()
            .map(|file| {
                let path = file.path.clone();
                #[cfg(target_arch = "wasm32")]
                let source = {
                    let mut file = file;
                    ImportSource::from_export(file.take_browser_file())
                };
                #[cfg(not(target_arch = "wasm32"))]
                let source = {
                    let size_bytes = file.size_bytes;
                    ImportSource::new(Box::new(move || Box::pin(async move { file.open().await.map_err(|error| error.to_string()) }))).with_size_bytes(size_bytes)
                };
                DirectoryImportFile { path, source }
            })
            .collect(),
    }
}

pub use navigation::{GlobalRoute, LibraryRoute, Route};
pub use root::{BrowserRoot, NowPlaying, NowPlayingBook, OpenBookRequested};
#[cfg(feature = "kobo")]
pub use services::SaveKoboAutoRotate;
#[cfg(not(target_arch = "wasm32"))]
pub use services::bind_update_worker;
pub use services::{AppServices, AppStartup, PublishUpdate, UpdateAction, UpdateActionHandler, UpdateControls, UpdateEvent, UpdateView};
pub use services::{LibraryRemovalHook, LibraryRemovalPreparation, LibraryRemovalService, library_is_being_removed};
pub use session::{BrowsePageSession, NavigationSession, PaneRequest, ReaderSession, SaveNavigationSession, WindowSession};
#[cfg(target_arch = "wasm32")]
pub use universal::sync::SyncEvent;
/// Switches the selected library to one of its browse views, from the menu in
/// the phone's view heading.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub(crate) struct OpenLibraryView(pub(crate) LibraryRoute);

pub use universal::sync::AuthenticationRequest;
pub use universal::sync::SyncState;

/// Release hidden library cover images after displaying a reader. Other
/// windows and panes keep their displayed artwork and in-flight cover loads.
pub fn evict_covers(window: &Window) {
    window.on_next_frame(|window, _| window.on_next_frame(|_, cx| library::evict_covers(cx)));
}

actions!(
    bokheim_browser,
    [
        DismissSettings,
        CloseSplitPane,
        OpenTrash,
        ShowSettings,
        DecreaseUiFontSize,
        IncreaseUiFontSize,
        PreviousDestination,
        NextDestination,
        OpenLibrarySwitcher,
        FocusNextSetting,
        FocusPreviousSetting,
        UndoLibraryAction,
        CopyBooks,
        CutBooks,
        PasteBooks
    ]
);

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub(crate) struct ToggleGridSelection(pub(crate) CursorMove);

const GRID_SELECTION_KEY_CONTEXT: &str = "BrowseSelection && !BrowseItem && !Input && !PopupMenu && !Dialog";
const BOOK_CLIPBOARD_KEY_CONTEXT: &str = "BrowseClipboard && !Input && !PopupMenu && !Dialog";

pub fn bind_keys(cx: &mut App) {
    use ui_components::{ShortcutPage::Browse, bind_shortcut};
    bind_shortcut(cx, Browse, "browse.dismiss", "Close settings", &["escape"], DismissSettings, "Browser");
    bind_shortcut(cx, Browse, "browse.font_decrease", "Decrease interface font size", &["secondary--"], DecreaseUiFontSize, "Browser");
    bind_shortcut(cx, Browse, "browse.font_increase", "Increase interface font size", &["secondary-="], IncreaseUiFontSize, "Browser");
    bind_shortcut(cx, Browse, "browse.previous_destination", "Previous destination", &["secondary-up"], PreviousDestination, "Browser");
    bind_shortcut(cx, Browse, "browse.next_destination", "Next destination", &["secondary-down"], NextDestination, "Browser");
    // Selection contexts retain precedence over destination navigation.
    bind_shortcut(cx, Browse, "browse.select_left", "Toggle selection to the left", &["secondary-left"], ToggleGridSelection(CursorMove::Left), GRID_SELECTION_KEY_CONTEXT);
    bind_shortcut(cx, Browse, "browse.select_right", "Toggle selection to the right", &["secondary-right"], ToggleGridSelection(CursorMove::Right), GRID_SELECTION_KEY_CONTEXT);
    bind_shortcut(cx, Browse, "browse.select_up", "Toggle selection above", &["secondary-up"], ToggleGridSelection(CursorMove::Up), GRID_SELECTION_KEY_CONTEXT);
    bind_shortcut(cx, Browse, "browse.select_down", "Toggle selection below", &["secondary-down"], ToggleGridSelection(CursorMove::Down), GRID_SELECTION_KEY_CONTEXT);
    bind_shortcut(cx, Browse, "browse.undo", "Undo library action", &["secondary-z"], UndoLibraryAction, "Browser");
    cx.bind_keys([
        KeyBinding::new("secondary-c", CopyBooks, Some(BOOK_CLIPBOARD_KEY_CONTEXT)),
        KeyBinding::new("secondary-x", CutBooks, Some(BOOK_CLIPBOARD_KEY_CONTEXT)),
        KeyBinding::new("secondary-v", PasteBooks, Some(BOOK_CLIPBOARD_KEY_CONTEXT)),
        #[cfg(not(target_os = "android"))]
        KeyBinding::new("back", DismissSettings, Some("Browser")),
        // Settings is a form rather than a grid, so it is walked control by
        // control. The arrows do what Tab does, for the devices whose only
        // keyboard is a four-way pad; a text field keeps them for its cursor,
        // and Tab still leaves the field.
        KeyBinding::new("tab", FocusNextSetting, Some(universal::SETTINGS_KEY_CONTEXT)),
        KeyBinding::new("down", FocusNextSetting, Some(universal::SETTINGS_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", FocusPreviousSetting, Some(universal::SETTINGS_KEY_CONTEXT)),
        KeyBinding::new("up", FocusPreviousSetting, Some(universal::SETTINGS_KEY_CONTEXT)),
    ]);
}

#[cfg(test)]
mod tests {
    use super::{BOOK_CLIPBOARD_KEY_CONTEXT, GRID_SELECTION_KEY_CONTEXT};
    use gpui::{KeyBindingContextPredicate, KeyContext};

    #[test]
    fn keyboard_selection_context_excludes_inputs_menus_and_dialogs() {
        let predicate = KeyBindingContextPredicate::parse(GRID_SELECTION_KEY_CONTEXT).unwrap();
        let mut contexts = vec![KeyContext::parse("Browser").unwrap()];
        assert_eq!(predicate.depth_of(&contexts), None);
        contexts.push(KeyContext::parse("BrowseSelection").unwrap());
        assert!(predicate.depth_of(&contexts).is_some());
        for child in ["Input", "PopupMenu", "Dialog"] {
            contexts.push(KeyContext::parse(child).unwrap());
            assert_eq!(predicate.depth_of(&contexts), None);
            contexts.pop();
        }
    }

    #[test]
    fn book_clipboard_context_keeps_text_editing_and_other_surfaces_separate() {
        let predicate = KeyBindingContextPredicate::parse(BOOK_CLIPBOARD_KEY_CONTEXT).unwrap();
        let mut contexts = vec![KeyContext::parse("Browser").unwrap()];
        assert_eq!(predicate.depth_of(&contexts), None);
        contexts.push(KeyContext::parse("BrowseSelection BrowseClipboard").unwrap());
        assert!(predicate.depth_of(&contexts).is_some());
        for child in ["Input", "PopupMenu", "Dialog"] {
            contexts.push(KeyContext::parse(child).unwrap());
            assert_eq!(predicate.depth_of(&contexts), None);
            contexts.pop();
        }
    }
}

#[cfg(target_os = "android")]
pub mod android_authentication;

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    // Native backends configure one process-wide transfer-policy file. Keep
    // their temporary directories alive throughout each backend fixture.
    pub(crate) static BACKEND: Mutex<()> = Mutex::new(());
}
