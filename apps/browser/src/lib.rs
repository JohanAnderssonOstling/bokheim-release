//! GPUI library-browsing window.
//!
//! [`root::BrowserRoot`] owns window policy. Navigation owns routing,
//! [`navigation::Libraries`] owns library lifecycle and per-library
//! pages, and global pages remain stable across library selection.

mod library;
mod model;
mod navigation;
mod root;
mod services;
mod stores;
mod universal;

pub(crate) const BOOK_IMPORT_EXTENSIONS: [&str; 6] = ["epub", "pdf", "mobi", "azw", "azw3", "m4b"];

use gpui::{App, KeyBinding, actions};

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
    Subject { library_id: sync_common::LibraryId, location: String, label: String },
}

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub struct OpenInNewWindow(pub NewWindowTarget);

/// A picker may provide just a local root. Enumerate it only when copying its
/// contents into a library; library registration consumes the root directly.
async fn prepare_directory_import(selected: gpui::SelectedDirectory, executor: &gpui::BackgroundExecutor) -> Result<app::DirectoryImport, String> {
    #[cfg(not(target_arch = "wasm32"))]
    let selected = match selected.local_path.as_ref() {
        Some(path) => {
            let path = path.clone();
            let mut contents = executor.spawn(async move { gpui::SelectedDirectory::from_path(&path, BOOK_IMPORT_EXTENSIONS).map_err(|error| format!("{error:#}")) }).await?;
            contents.name = selected.name;
            contents
        }
        None => selected,
    };
    #[cfg(target_arch = "wasm32")]
    let _ = executor;
    Ok(directory_import(selected))
}

fn directory_import(selected: gpui::SelectedDirectory) -> app::DirectoryImport {
    app::DirectoryImport {
        name: selected.name,
        directories: selected.directories,
        files: selected
            .files
            .into_iter()
            .map(|mut file| {
                let path = file.path.clone();
                #[cfg(target_arch = "wasm32")]
                let source = app::ImportSource::from_export(file.take_browser_file());
                #[cfg(not(target_arch = "wasm32"))]
                let source = {
                    let size_bytes = file.size_bytes;
                    app::ImportSource::new(Box::new(move || Box::pin(async move { file.open().await.map_err(|error| error.to_string()) }))).with_size_bytes(size_bytes)
                };
                app::DirectoryImportFile { path, source }
            })
            .collect(),
    }
}

pub use navigation::{GlobalRoute, LibraryRoute, Route};
pub use root::{BookDetailDismissed, BrowserRoot, OpenBookRequested};
#[cfg(feature = "kobo")]
pub use services::SaveKoboAutoRotate;
pub use services::{AppServices, AppStartup, UpdateAction, UpdateActionHandler, UpdateControls, UpdateView};
pub use services::{LibraryRemovalHook, LibraryRemovalPreparation, library_is_being_removed};
#[cfg(target_arch = "wasm32")]
pub use universal::sync::SyncEvent;
pub use universal::sync::SyncState;

actions!(
    bokheim_browser,
    [DismissSettings, ShowSettings, DecreaseUiFontSize, IncreaseUiFontSize, ActivateAuthor, PreviousDestination, NextDestination, OpenLibrarySwitcher, FocusNextSetting, FocusPreviousSetting, OpenContextMenu, UndoLibraryAction]
);

pub fn bind_keys(cx: &mut App) {
    // Number shortcuts follow the order drawn by the navigation rail.
    cx.bind_keys([
        KeyBinding::new("escape", DismissSettings, Some("Browser")),
        #[cfg(feature = "mobile")]
        KeyBinding::new("back", DismissSettings, Some("Browser")),
        KeyBinding::new("ctrl--", DecreaseUiFontSize, Some("Browser")),
        KeyBinding::new("ctrl-=", IncreaseUiFontSize, Some("Browser")),
        KeyBinding::new("ctrl-+", IncreaseUiFontSize, Some("Browser")),
        // Ctrl to move between destinations, so plain arrows stay with the grid
        // cursor inside whichever page is open.
        KeyBinding::new("ctrl-up", PreviousDestination, Some("Browser")),
        KeyBinding::new("ctrl-down", NextDestination, Some("Browser")),
        // The two keystrokes that mean "context menu" on a desktop keyboard.
        // Shift-F10 is the one every keyboard has; the dedicated Menu key is
        // reported as "menu" where the platform has it at all.
        KeyBinding::new("ctrl-z", UndoLibraryAction, Some("Browser")),
        KeyBinding::new("shift-f10", OpenContextMenu, Some("Browser")),
        KeyBinding::new("menu", OpenContextMenu, Some("Browser")),
        KeyBinding::new("enter", ActivateAuthor, Some("BrowserAuthorList")),
        KeyBinding::new("space", ActivateAuthor, Some("BrowserAuthorList")),
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod directory_selection_tests {
    use super::*;
    use futures_util::FutureExt as _;
    use std::io::Read as _;

    #[test]
    fn local_root_is_enumerated_only_when_importing_contents() {
        let path = std::env::temp_dir().join(format!("selected-root-{}", uuid::Uuid::new_v4()));
        // Root selection itself does no directory I/O.
        let mut selected = gpui::SelectedDirectory::root(path.clone()).unwrap();
        selected.name = "Selected folder".into();
        assert!(selected.files.is_empty());
        assert!(selected.directories.is_empty());
        std::fs::create_dir_all(path.join("Shelf")).unwrap();
        std::fs::create_dir(path.join("Empty")).unwrap();
        std::fs::write(path.join("Shelf/book.epub"), b"book contents").unwrap();
        std::fs::write(path.join("notes.txt"), b"not a book").unwrap();
        let dispatcher = std::sync::Arc::new(gpui::TestDispatcher::new(0));
        let executor = gpui::BackgroundExecutor::new(dispatcher.clone());
        let foreground = gpui::ForegroundExecutor::new(dispatcher);
        let imported = foreground.block_test(prepare_directory_import(selected, &executor)).unwrap();
        assert_eq!(imported.name, "Selected folder");
        assert_eq!(imported.directories, vec![vec!["Empty"], vec!["Shelf"]]);
        assert_eq!(imported.files.len(), 1);
        let file = imported.files.into_iter().next().unwrap();
        assert_eq!(file.path, vec!["Shelf", "book.epub"]);
        let mut reader = file.source.open().now_or_never().unwrap().unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"book contents");
        drop(reader);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn missing_local_root_does_not_become_an_empty_import() {
        let path = std::env::temp_dir().join(format!("missing-root-{}", uuid::Uuid::new_v4()));
        let selected = gpui::SelectedDirectory::root(path.clone()).unwrap();
        let dispatcher = std::sync::Arc::new(gpui::TestDispatcher::new(0));
        let executor = gpui::BackgroundExecutor::new(dispatcher.clone());
        let foreground = gpui::ForegroundExecutor::new(dispatcher);
        let error = foreground.block_test(prepare_directory_import(selected, &executor)).err().unwrap();
        assert!(error.contains(path.to_str().unwrap()));
    }
}

#[cfg(target_os = "android")]
pub mod android_authentication;
