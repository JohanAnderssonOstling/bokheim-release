//! Folder-scoped commands and modal state for folder browsing.
//!
//! "Folder-scoped" covers both the folders a listing holds and the books placed
//! in it: copying, moving and discarding are placement changes either way, and
//! keeping them together gives one picker, one confirmation and one error path
//! for a selection that mixes the two.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{Anchor, AnyElement, App, Context, Entity, EventEmitter, FileDialogFilter, Focusable, IntoElement, MouseButton, PathPromptOptions, SharedString, Subscription, Window, div, px, rems};
use gpui_component::button::ButtonVariants;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenu};
use gpui_component::{Disableable, IconName};
use sync_common::{ContentHash, DirId, ROOT_DIR_ID};
use ui_components as components;

use super::folder_picker::{FolderPicker, FolderPickerEvent};
use crate::library::services::{LatestRequest, LibraryContext, UndoEntry, UndoStep};
use crate::library::widgets::{BookPlacement, PlaceBook, RemoveBook};

fn book_import_extensions() -> Vec<String> {
    crate::BOOK_IMPORT_EXTENSIONS.into_iter().map(str::to_owned).collect()
}

/// Names a set of targets the way a notification would: "3 books", "1 folder
/// and 2 books".
pub(super) fn describe(targets: &FolderTargets) -> String {
    [(targets.folders.len(), "folder"), (targets.books.len(), "book")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| if count == 1 { format!("1 {noun}") } else { format!("{count} {noun}s") })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// What moving `targets` to Trash will do, for its confirmation.
fn trash_description(targets: &FolderTargets) -> String {
    let mut description = format!("{} will be moved to Trash.", describe(targets));
    if !targets.folders.is_empty() {
        description.push_str(" A folder goes with everything inside it.");
    }
    if !targets.books.is_empty() {
        description.push_str(" A book leaves this folder, and reaches Trash only if this was the last folder holding it.");
    }
    description
}

/// What taking the books in `targets` out of this folder will do, for its
/// confirmation.
fn remove_description(targets: &FolderTargets) -> String {
    let books = FolderTargets { folders: Vec::new(), folder_parents: Default::default(), books: targets.books.clone(), source: targets.source };
    format!("{} will leave this folder but stay in the library. A book reaches Trash only if this was the last folder holding it.", describe(&books))
}

/// The folder commands for a menu opened over a selection, worked out when the
/// menu opens and laid into it by [`FolderActions::item_menu_items`].
#[derive(Clone)]
pub(crate) struct ItemCommands {
    targets: FolderTargets,
    /// A lone folder and the name it would be renamed from. Renaming is a
    /// single-folder command: there is one name to type.
    rename: Option<(DirId, String)>,
    /// Whether anything selected still holds downloaded bytes.
    can_evict: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderActionsEvent {
    Reload,
    Renamed(String),
    OpenRoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FolderNameMode {
    Create,
    Rename,
}

struct FolderNameDialog {
    mode: FolderNameMode,
    /// The folder being renamed, or the parent a new folder is created under.
    /// Held per dialog rather than read from `current_id`, because a chip's
    /// context menu acts on a folder the page is not inside.
    target_id: DirId,
    /// The name a rename is replacing, which is what undoing it puts back.
    previous_name: Option<String>,
    input: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
    _subscription: Subscription,
}

/// What a chosen destination does to whatever opened the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FolderPlacement {
    Copy,
    Move,
}

/// What a command acts on: one item from a context menu, or everything a
/// rubber band picked up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FolderTargets {
    pub(crate) folders: Vec<DirId>,
    pub(crate) folder_parents: std::collections::HashMap<DirId, DirId>,
    pub(crate) books: Vec<ContentHash>,
    /// The holder of listed books and fallback parent for ordinary folder chips.
    /// Promoted chips record their actual parents separately.
    pub(crate) source: DirId,
}

impl FolderTargets {
    /// Whether dropping these into `destination` would do anything: a folder
    /// cannot go into itself, and a drop that leaves everything where it is
    /// has nothing to ask about.
    pub(crate) fn accepts(&self, destination: DirId) -> bool {
        !self.folders.contains(&destination) && !self.all_in_folder(destination)
    }

    pub(crate) fn all_in_folder(&self, directory: DirId) -> bool {
        (self.books.is_empty() || self.source == directory) && self.folders.iter().all(|folder| self.folder_parents.get(folder).copied().unwrap_or(self.source) == directory)
    }

    pub(crate) fn len(&self) -> usize {
        self.folders.len() + self.books.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

struct MoveFolderDialog {
    placement: FolderPlacement,
    targets: FolderTargets,
    picker: Entity<FolderPicker>,
    _subscription: Subscription,
}

/// Owns commands that only exist for physical folders. The shared browse page
/// observes high-level outcomes instead of carrying backend handles and modal
/// implementation state for every adapter.
pub(crate) struct FolderActions {
    ui: LibraryContext,
    current_id: DirId,
    current_name: Option<String>,
    name_dialog: Option<FolderNameDialog>,
    move_dialog: Option<MoveFolderDialog>,
    move_request: LatestRequest,
}

impl EventEmitter<FolderActionsEvent> for FolderActions {}

impl FolderActions {
    pub(crate) fn new(ui: LibraryContext, current_id: DirId, current_name: Option<String>) -> Self {
        Self { ui, current_id, current_name, name_dialog: None, move_dialog: None, move_request: LatestRequest::default() }
    }

    pub(crate) fn set_location(&mut self, location: &str, name: Option<String>, cx: &mut Context<Self>) {
        let Ok(id) = DirId::parse_str(location) else {
            log::warn!("ignored invalid folder browse location {location}");
            return;
        };
        if self.current_id != id {
            self.cancel_move_selection(cx);
        }
        self.current_id = id;
        self.current_name = name;
        cx.notify();
    }

    pub(crate) fn has_modal(&self) -> bool {
        self.name_dialog.is_some() || self.move_dialog.is_some()
    }

    pub(crate) fn library_loading(&self, cx: &App) -> bool {
        self.ui.updates().read(cx).loading()
    }

    pub(crate) fn scanner_running(&self, cx: &App) -> bool {
        self.ui.updates().read(cx).scanning()
    }

    pub(crate) fn dismiss_modal(&mut self, cx: &mut Context<Self>) {
        if self.name_dialog.as_ref().is_some_and(|dialog| dialog.busy) {
            return;
        }
        self.name_dialog = None;
        self.cancel_move_selection(cx);
        cx.notify();
    }

    pub(crate) fn cancel_move_selection(&mut self, cx: &mut Context<Self>) {
        self.move_request.begin();
        self.move_dialog = None;
        cx.notify();
    }

    fn choose_book(&mut self, cx: &mut Context<Self>) {
        self.ui.choose_and_import_book(self.current_id, cx);
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let parent_id = self.current_id;
        let extensions = book_import_extensions();
        let selection = cx.prompt_for_directories_with_filters(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Add folder".into()) }, vec![FileDialogFilter { name: "Books".to_owned(), extensions }]);
        let library = self.ui.backend().clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |actions, cx| {
            let result = match selection.await {
                Ok(Ok(Some(directories))) => match directories.into_iter().next() {
                    Some(directory) => {
                        // Native imports expose progress for one activity. Browser imports
                        // are owned and reported by the shared import coordinator instead.
                        #[cfg(not(target_arch = "wasm32"))]
                        let activity = uuid::Uuid::new_v4();
                        #[cfg(not(target_arch = "wasm32"))]
                        let _progress = actions.update_in(cx, |_, window, cx| crate::services::ImportProgressToast::start_for_activity(library.clone(), activity, window, cx)).ok();
                        match crate::prepare_directory_import(directory, &executor).await {
                            Ok(directory) => {
                                #[cfg(not(target_arch = "wasm32"))]
                                let result = library.import_directory_with_activity(parent_id, directory, activity).await;
                                #[cfg(target_arch = "wasm32")]
                                {
                                    let result = library.import_directory(parent_id, directory).await;
                                    let _ = result;
                                    Ok(Vec::<app::ImportFailure>::new())
                                }
                                #[cfg(not(target_arch = "wasm32"))]
                                result
                            }
                            Err(error) => Err(error),
                        }
                    }
                    None => return,
                },
                Ok(Ok(None)) => return,
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            let _ = actions.update_in(cx, |_, window, cx| match result {
                Ok(failures) => {
                    cx.emit(FolderActionsEvent::Reload);
                    if !failures.is_empty() {
                        crate::services::notify_warning("folder-add-folder-warning", format!("Folder added, but {} item(s) could not be imported. {}", failures.len(), crate::services::import_failure_message(&failures[0])), window, cx);
                    }
                }
                Err(error) => crate::services::notify_error("folder-add-folder-error", format!("Could not add folder: {error}"), window, cx),
            });
        })
        .detach();
    }

    fn open_name_dialog(&mut self, mode: FolderNameMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == FolderNameMode::Rename && self.current_name.is_none() {
            return;
        }
        self.open_name_dialog_for(mode, self.current_id, self.current_name.clone(), window, cx);
    }

    /// Renames one listed folder, which need not be the folder the page is
    /// inside.
    pub(crate) fn open_rename(&mut self, directory_id: DirId, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if directory_id == ROOT_DIR_ID {
            return;
        }
        self.open_name_dialog_for(FolderNameMode::Rename, directory_id, Some(name), window, cx);
    }

    /// Renames a named folder, or creates a folder under `target_id`. The chip
    /// context menu passes the chip's folder; the toolbar passes the folder the
    /// page is currently inside.
    fn open_name_dialog_for(&mut self, mode: FolderNameMode, target_id: DirId, target_name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.name_dialog.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));
        if let Some(name) = target_name.as_ref().filter(|_| mode == FolderNameMode::Rename) {
            input.update(cx, |input, cx| input.set_value(name, window, cx));
        }
        let subscription = cx.subscribe_in(&input, window, |actions, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                actions.submit_name_dialog(window, cx);
            }
        });
        input.read(cx).focus_handle(cx).focus(window, cx);
        let previous_name = target_name.filter(|_| mode == FolderNameMode::Rename);
        self.name_dialog = Some(FolderNameDialog { mode, target_id, previous_name, input, error: None, busy: false, _subscription: subscription });
        cx.notify();
    }

    fn close_name_dialog(&mut self, cx: &mut Context<Self>) {
        if self.name_dialog.as_ref().is_some_and(|dialog| dialog.busy) {
            return;
        }
        self.name_dialog = None;
        cx.notify();
    }

    fn submit_name_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.name_dialog.as_mut() else { return };
        if dialog.busy {
            return;
        }
        let name = dialog.input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            dialog.error = Some("Enter a folder name".into());
            cx.notify();
            return;
        }
        dialog.busy = true;
        dialog.error = None;
        let target_id = dialog.target_id;
        let previous_name = dialog.previous_name.clone();
        match dialog.mode {
            FolderNameMode::Create => {
                let parent_id = target_id;
                let library = self.ui.backend().clone();
                let operation = async move { library.create_directory(parent_id, name).await };
                cx.spawn(async move |actions, cx| {
                    let result = operation.await;
                    let _ = actions.update(cx, |actions, cx| match result {
                        Ok(()) => {
                            actions.name_dialog = None;
                            cx.emit(FolderActionsEvent::Reload);
                            cx.notify();
                        }
                        Err(error) => {
                            if let Some(dialog) = &mut actions.name_dialog {
                                dialog.busy = false;
                                dialog.error = Some(error.into());
                            }
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            FolderNameMode::Rename => {
                let directory_id = target_id;
                let library = self.ui.backend().clone();
                if let Some(previous) = previous_name {
                    self.ui.undo().push(UndoEntry::new(format!("Renamed “{previous}”")).with(UndoStep::RenameDirectory { directory: directory_id, name: previous }));
                }
                let operation = async move { library.rename_directory(directory_id, name).await };
                cx.spawn(async move |actions, cx| {
                    let result = operation.await;
                    let _ = actions.update(cx, |actions, cx| match result {
                        Ok(name) => {
                            actions.name_dialog = None;
                            // Renaming a chip's folder leaves the page where it
                            // is: only the crumb for the folder being browsed
                            // needs the new name.
                            if actions.current_id == directory_id {
                                actions.current_name = Some(name.clone());
                                cx.emit(FolderActionsEvent::Renamed(name));
                            } else {
                                cx.emit(FolderActionsEvent::Reload);
                            }
                            cx.notify();
                        }
                        Err(error) => {
                            if let Some(dialog) = &mut actions.name_dialog {
                                dialog.busy = false;
                                dialog.error = Some(error.into());
                            }
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Chooses a new home for everything in `targets`.
    pub(crate) fn open_move_targets(&mut self, targets: FolderTargets, cx: &mut Context<Self>) {
        self.open_placement_picker(FolderPlacement::Move, targets, cx);
    }

    /// Chooses where to duplicate everything in `targets`. Unlike a move, the
    /// folder they are already in is a valid destination: copies land beside
    /// their originals under free names.
    pub(crate) fn open_copy_targets(&mut self, targets: FolderTargets, cx: &mut Context<Self>) {
        self.open_placement_picker(FolderPlacement::Copy, targets, cx);
    }

    fn open_placement_picker(&mut self, placement: FolderPlacement, targets: FolderTargets, cx: &mut Context<Self>) {
        if targets.is_empty() || targets.folders.contains(&ROOT_DIR_ID) || self.name_dialog.is_some() || self.move_dialog.is_some() {
            return;
        }
        let generation = self.move_request.begin();
        let library = self.ui.backend().clone();
        let load = async move { library.folder_destinations().await };
        let task = cx.spawn(async move |actions, cx| {
            let result = load.await;
            let _ = actions.update(cx, |actions, cx| {
                if !actions.move_request.is_current(generation) {
                    return;
                }
                match result {
                    Ok(mut destinations) => {
                        // No folder can land inside itself, either way, so every
                        // selected subtree is dropped. Only a move is barred
                        // from the folder the items are already in — a copy
                        // there is a legitimate duplicate.
                        for folder in &targets.folders {
                            let Some(path) = destinations.iter().find(|destination| destination.id == *folder).map(|destination| destination.path.clone()) else { continue };
                            let prefix = format!("{path} / ");
                            destinations.retain(|destination| destination.id != *folder && destination.path != path && !destination.path.starts_with(&prefix));
                        }
                        let picker = cx.new(|cx| {
                            let picker = FolderPicker::new(ROOT_DIR_ID, destinations, cx);
                            if placement == FolderPlacement::Move && targets.all_in_folder(targets.source) { picker.disabling(targets.source) } else { picker }
                        });
                        let subscription = cx.subscribe(&picker, |actions, _, event, cx| actions.move_destination_selected(*event, cx));
                        actions.move_dialog = Some(MoveFolderDialog { placement, targets, picker, _subscription: subscription });
                        cx.notify();
                    }
                    Err(error) => log::error!("failed to load folder destinations: {error}"),
                }
            });
        });
        self.move_request.install(task);
    }

    fn move_destination_selected(&mut self, event: FolderPickerEvent, cx: &mut Context<Self>) {
        let Some(dialog) = self.move_dialog.take() else { return };
        let FolderPickerEvent::Selected(destination_id) = event else {
            cx.notify();
            return;
        };
        self.place_targets(dialog.placement, dialog.targets, destination_id, None, cx);
    }

    /// Runs a placement whose destination is already known — a drop onto a
    /// folder, where the folder under the pointer is the answer the picker
    /// would otherwise have asked for.
    ///
    /// `destination_name` names the folder in the confirmation, where it is
    /// known.
    pub(crate) fn place_targets(&mut self, placement: FolderPlacement, targets: FolderTargets, destination_id: DirId, destination_name: Option<SharedString>, cx: &mut Context<Self>) {
        if targets.is_empty() || targets.folders.contains(&destination_id) {
            return;
        }
        // Moving the folder the page is inside invalidates its path, so the
        // page returns to the root. A copy leaves its source where it is, and
        // moving anything else only changes what this listing holds.
        let left_current = placement == FolderPlacement::Move && targets.folders.contains(&self.current_id);
        let source = targets.source;
        let folder_parents = targets.folder_parents.clone();
        let label = format!("{} {}", if placement == FolderPlacement::Copy { "Copied" } else { "Moved" }, describe(&targets));
        let confirmation = match destination_name {
            Some(name) => format!("{label} to {name}"),
            None => label.clone(),
        };
        let library = self.ui.backend().clone();
        let undo = self.ui.undo().clone();
        // Steps are recorded as each item lands, so a run that fails half way
        // leaves an undo for exactly the half that happened.
        let operation = async move {
            let mut entry = UndoEntry::new(label);
            let mut result = Ok(());
            'placing: {
                for folder in targets.folders {
                    match placement {
                        FolderPlacement::Copy => match library.copy_directory(folder, destination_id).await {
                            Ok(copy) => entry.push(UndoStep::TrashDirectory { directory: copy }),
                            Err(error) => {
                                result = Err(error);
                                break 'placing;
                            }
                        },
                        FolderPlacement::Move => match library.move_directory(folder, destination_id).await {
                            Ok(_) => entry.push(UndoStep::MoveDirectory { directory: folder, parent: folder_parents.get(&folder).copied().unwrap_or(source) }),
                            Err(error) => {
                                result = Err(error);
                                break 'placing;
                            }
                        },
                    }
                }
                for book in targets.books {
                    let placed = match placement {
                        FolderPlacement::Copy => library.copy_book_to_directory(book, source, destination_id).await.map(|_| UndoStep::RemoveBookFromDirectory { book, directory: destination_id }),
                        FolderPlacement::Move => library.move_book_to_directory(book, source, destination_id).await.map(|_| UndoStep::MoveBook { book, from: destination_id, to: source }),
                    };
                    match placed {
                        Ok(step) => entry.push(step),
                        Err(error) => {
                            result = Err(error);
                            break 'placing;
                        }
                    }
                }
            }
            (result, entry)
        };
        cx.spawn(async move |actions, cx| {
            let (result, entry) = operation.await;
            undo.push(entry);
            let _ = actions.update_in(cx, |_, window, cx| match result {
                Ok(()) => {
                    cx.emit(if left_current { FolderActionsEvent::OpenRoot } else { FolderActionsEvent::Reload });
                    crate::services::notify_undoable("folder-placement", confirmation, window, cx);
                }
                Err(error) => {
                    // Items are placed one at a time, so a failure part-way
                    // leaves the earlier ones placed. Reloading shows what
                    // actually happened rather than what was asked for.
                    cx.emit(FolderActionsEvent::Reload);
                    crate::services::notify_error(
                        "folder-placement-error",
                        format!(
                            "Could not {} everything selected: {error}",
                            match placement {
                                FolderPlacement::Copy => "copy",
                                FolderPlacement::Move => "move",
                            }
                        ),
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Discards everything in `targets` after one confirmation. Folders go to
    /// Trash whole; a book leaves the folder it was listed in, which sends it
    /// to Trash only if that was the last place holding it.
    pub(crate) fn trash_targets(&mut self, targets: FolderTargets, description: String, window: &mut Window, cx: &mut Context<Self>) {
        if targets.is_empty() || targets.folders.contains(&ROOT_DIR_ID) {
            return;
        }
        let answer = window.prompt(gpui::PromptLevel::Warning, if targets.len() == 1 { "Move this to Trash?" } else { "Move these to Trash?" }, Some(&description), &["Move to Trash", "Cancel"], cx);
        let source = targets.source;
        let folder_parents = targets.folder_parents.clone();
        let label = format!("Trashed {}", describe(&targets));
        let library = self.ui.backend().clone();
        let undo = self.ui.undo().clone();
        cx.spawn(async move |actions, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let mut entry = UndoEntry::new(label);
            let result = async {
                for folder in targets.folders {
                    library.move_directory_to_trash(folder).await?;
                    entry.push(UndoStep::RestoreDirectory { directory: folder, parent: Some(folder_parents.get(&folder).copied().unwrap_or(source)) });
                }
                for book in targets.books {
                    library.remove_book_from_directory(book, source).await.map(|_| ())?;
                    entry.push(UndoStep::RestoreBookPlacement { book, directory: source });
                }
                Ok::<(), String>(())
            }
            .await;
            undo.push(entry);
            let _ = actions.update_in(cx, |_, window, cx| {
                cx.emit(FolderActionsEvent::Reload);
                if let Err(error) = result {
                    crate::services::notify_error("folder-trash-error", format!("Could not move everything selected to Trash: {error}"), window, cx);
                }
            });
        })
        .detach();
    }

    /// Takes the selected books out of the folder they are listed in without
    /// moving anything to Trash directly: a book stays in the library while
    /// another folder holds it, and only reaches Trash when this was its last
    /// folder. Folders in the selection are left alone.
    pub(crate) fn remove_books_from_folder(&mut self, targets: FolderTargets, description: String, window: &mut Window, cx: &mut Context<Self>) {
        if targets.books.is_empty() {
            return;
        }
        let answer = window.prompt(gpui::PromptLevel::Warning, "Remove from folder?", Some(&description), &["Remove", "Cancel"], cx);
        let source = targets.source;
        let label = format!("Removed {}", describe(&FolderTargets { folders: Vec::new(), folder_parents: Default::default(), books: targets.books.clone(), source }));
        let library = self.ui.backend().clone();
        let undo = self.ui.undo().clone();
        cx.spawn(async move |actions, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let mut entry = UndoEntry::new(label);
            let result = async {
                for book in targets.books {
                    library.remove_book_from_directory(book, source).await.map(|_| ())?;
                    entry.push(UndoStep::RestoreBookPlacement { book, directory: source });
                }
                Ok::<(), String>(())
            }
            .await;
            undo.push(entry);
            let _ = actions.update_in(cx, |_, window, cx| {
                cx.emit(FolderActionsEvent::Reload);
                if let Err(error) = result {
                    crate::services::notify_error("folder-remove-error", format!("Could not remove everything selected from the folder: {error}"), window, cx);
                }
            });
        })
        .detach();
    }

    /// Drops the downloaded copies for everything selected while keeping every
    /// entry. Books the server does not hold keep their bytes, and surface as
    /// an error naming the first one that could not be evicted.
    pub(crate) fn evict_downloads(&mut self, targets: FolderTargets, _window: &mut Window, cx: &mut Context<Self>) {
        if targets.is_empty() {
            return;
        }
        let library = self.ui.backend().clone();
        cx.spawn(async move |actions, cx| {
            let mut result = Ok(());
            for book in targets.books {
                if let Err(error) = library.remove_local_book_copy(book).await {
                    result = Err(error);
                    break;
                }
            }
            if result.is_ok() {
                for folder in targets.folders {
                    if let Err(error) = library.evict_folder_downloads(folder).await {
                        result = Err(error);
                        break;
                    }
                }
            }
            let _ = actions.update_in(cx, |_, window, cx| {
                cx.emit(FolderActionsEvent::Reload);
                if let Err(error) = result {
                    crate::services::notify_error("folder-evict-error", format!("Could not remove every download: {error}"), window, cx);
                }
            });
        })
        .detach();
    }

    fn render_name_dialog(actions: Entity<Self>, cx: &mut App) -> Option<AnyElement> {
        let (mode, input, error, busy) = {
            let actions = actions.read(cx);
            let dialog = actions.name_dialog.as_ref()?;
            (dialog.mode, dialog.input.clone(), dialog.error.clone(), dialog.busy)
        };
        let theme = components::browser_theme(cx);
        let cancel_actions = actions.clone();
        let dismiss_actions = actions.clone();
        let submit_actions = actions.clone();
        let (title, idle_label, busy_label, id_suffix) = match mode {
            FolderNameMode::Create => ("Create folder", "Create", "Creating…", "create-folder"),
            FolderNameMode::Rename => ("Rename folder", "Rename", "Renaming…", "rename-folder"),
        };
        let cancel = components::dialog_cancel_button(format!("cancel-{id_suffix}"), "Cancel").disabled(busy).on_click(move |_, _, cx| {
            cancel_actions.update(cx, |actions, cx| actions.close_name_dialog(cx));
        });
        let submit = components::base_button(format!("confirm-{id_suffix}"))
            .primary()
            .label(if busy { busy_label } else { idle_label })
            .disabled(busy || input.read(cx).value().trim().is_empty())
            .on_click(move |_, window, cx| submit_actions.update(cx, |actions, cx| actions.submit_name_dialog(window, cx)));
        let panel = components::modal_surface(theme)
            .w(px(360.0))
            .max_w_full()
            .min_h(px(180.0))
            .p(px(components::SPACE_MD))
            .flex()
            .flex_col()
            .gap(px(components::SPACE_MD))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(rems(1.125)).text_color(theme.text).child(title))
            .child(Input::new(&input).h(gpui::rems(components::TOPBAR_ACTION_HEIGHT_REM)))
            .children(error.map(|error| components::error_text(theme).child(error)))
            .child(div().flex_1())
            .child(div().flex().items_center().justify_end().gap(px(components::SPACE_SM)).child(cancel).child(submit));
        Some(
            gpui::deferred(
                components::modal_overlay()
                    .occlude()
                    .child(components::modal_scrim(format!("{id_suffix}-scrim"), theme).on_click(move |_, _, cx| {
                        dismiss_actions.update(cx, |actions, cx| actions.close_name_dialog(cx));
                    }))
                    .child(panel),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// What a book card's own "Copy to" and "Move to" run: the folder picker,
    /// over that one book.
    pub(crate) fn place_book(actions: &Entity<Self>) -> PlaceBook {
        let actions = actions.clone();
        Rc::new(move |book, source, placement, cx| {
            let targets = FolderTargets { folders: Vec::new(), folder_parents: Default::default(), books: vec![book], source };
            actions.update(cx, |actions, cx| match placement {
                BookPlacement::Copy => actions.open_copy_targets(targets, cx),
                BookPlacement::Move => actions.open_move_targets(targets, cx),
            });
        })
    }

    /// What a book card's own "Remove from folder" runs.
    pub(crate) fn remove_book(actions: &Entity<Self>) -> RemoveBook {
        let actions = actions.clone();
        Rc::new(move |book, source, description, window, cx| {
            let targets = FolderTargets { folders: Vec::new(), folder_parents: Default::default(), books: vec![book], source };
            actions.update(cx, |actions, cx| actions.remove_books_from_folder(targets, description, window, cx));
        })
    }

    /// Works out the commands over `targets`. The listing supplies what only it
    /// knows about its folders: their names, and whether they hold downloaded
    /// books.
    pub(crate) fn item_commands(&self, targets: FolderTargets, folder_name: impl Fn(DirId) -> Option<String>, folder_has_downloads: impl Fn(DirId) -> bool, cx: &App) -> ItemCommands {
        let rename = match (targets.folders.as_slice(), targets.books.is_empty()) {
            ([id], true) => Some((*id, folder_name(*id).unwrap_or_default())),
            _ => None,
        };
        let bridge = self.ui.updates().read(cx);
        let can_evict = targets.books.iter().any(|hash| matches!(bridge.download_state(*hash), Some(app::DownloadState::Downloaded))) || targets.folders.iter().any(|id| folder_has_downloads(*id));
        ItemCommands { targets, rename, can_evict }
    }

    /// Lays the commands over a selection into a menu: rename, copy and move,
    /// taking books out of this folder, dropping downloads, and Trash.
    pub(crate) fn item_menu_items(actions: Entity<Self>, commands: ItemCommands, mut menu: PopupMenu) -> PopupMenu {
        let ItemCommands { targets, rename, can_evict } = commands;
        let folder_icon = || components::navigation_icon(components::NavigationIcon::Folder);
        if let Some((id, name)) = rename {
            let actions = actions.clone();
            menu = menu.item(components::menu_item_with_icon("Rename…", IconName::Replace, move |_, window, cx| actions.update(cx, |actions, cx| actions.open_rename(id, name.clone(), window, cx))));
        }
        let (copy_actions, copy_targets) = (actions.clone(), targets.clone());
        let (move_actions, move_targets) = (actions.clone(), targets.clone());
        menu = menu
            .item(components::menu_item_with_icon("Copy to folder…", IconName::Copy, move |_, _, cx| copy_actions.update(cx, |actions, cx| actions.open_copy_targets(copy_targets.clone(), cx))))
            .item(components::menu_item_with_icon("Move to folder…", folder_icon(), move |_, _, cx| move_actions.update(cx, |actions, cx| actions.open_move_targets(move_targets.clone(), cx))));
        // Leaving a folder and leaving the library are separate commands: the
        // first keeps the books in their other folders, the second moves the
        // whole selection to Trash.
        if !targets.books.is_empty() {
            let (remove_actions, remove_targets, description) = (actions.clone(), targets.clone(), remove_description(&targets));
            menu = menu.item(components::menu_item_with_icon("Remove from folder", folder_icon(), move |_, window, cx| {
                remove_actions.update(cx, |actions, cx| actions.remove_books_from_folder(remove_targets.clone(), description.clone(), window, cx))
            }));
        }
        menu = menu.separator();
        if can_evict {
            let (evict_actions, evict_targets) = (actions.clone(), targets.clone());
            menu = menu.item(components::menu_item_with_icon("Remove download", IconName::HardDrive, move |_, window, cx| evict_actions.update(cx, |actions, cx| actions.evict_downloads(evict_targets.clone(), window, cx))));
        }
        let description = trash_description(&targets);
        menu.item(components::menu_item_with_icon("Move to Trash", components::navigation_icon(components::NavigationIcon::Trash2), move |_, window, cx| {
            actions.update(cx, |actions, cx| actions.trash_targets(targets.clone(), description.clone(), window, cx))
        }))
    }

    /// Asks whether a drop onto the folder `destination`, called `name`, moves
    /// what was dragged or copies it.
    pub(crate) fn drop_menu_items(actions: Entity<Self>, targets: FolderTargets, destination: DirId, name: &str, menu: PopupMenu) -> PopupMenu {
        let (move_actions, move_targets) = (actions.clone(), targets.clone());
        let (move_name, copy_name) = (SharedString::from(name.to_owned()), SharedString::from(name.to_owned()));
        menu.item(components::menu_item_with_icon(format!("Move to {name}"), components::navigation_icon(components::NavigationIcon::Folder), move |_, _, cx| {
            move_actions.update(cx, |actions, cx| actions.place_targets(FolderPlacement::Move, move_targets.clone(), destination, Some(move_name.clone()), cx))
        }))
        .item(components::menu_item_with_icon(format!("Copy to {name}"), IconName::Copy, move |_, _, cx| actions.update(cx, |actions, cx| actions.place_targets(FolderPlacement::Copy, targets.clone(), destination, Some(copy_name.clone()), cx))))
    }

    /// The commands for adding content to the folder being browsed, as menu
    /// items. Shared by the toolbar's Add button and by a secondary click on
    /// the listing's empty space.
    pub(crate) fn add_menu_items(actions: Entity<Self>, menu: PopupMenu) -> PopupMenu {
        let add_book_actions = actions.clone();
        let add_folder_actions = actions.clone();
        let create_actions = actions;
        menu.item(components::menu_item_with_icon("Add book", IconName::BookOpen, move |_, _, cx| add_book_actions.update(cx, |actions, cx| actions.choose_book(cx))))
            .item(components::menu_item_with_icon("Add folder", IconName::FolderOpen, move |_, _, cx| add_folder_actions.update(cx, |actions, cx| actions.choose_folder(cx))))
            .item(components::menu_item_with_icon("Create folder", IconName::Plus, move |_, window, cx| create_actions.update(cx, |actions, cx| actions.open_name_dialog(FolderNameMode::Create, window, cx))))
    }

    pub(crate) fn render_trigger(actions: Entity<Self>, menu_anchor: Anchor, cx: &mut App) -> AnyElement {
        let trigger = components::outlined_icon_button("folder-add", "Add", IconName::Plus, components::browser_theme(cx));
        let menu = trigger.dropdown_menu_with_anchor(menu_anchor, move |menu, _, _| Self::add_menu_items(actions.clone(), menu));
        div().flex().items_center().child(menu).into_any_element()
    }

    pub(crate) fn render_overlay(actions: Entity<Self>, cx: &mut App) -> Option<AnyElement> {
        if let Some(dialog) = Self::render_name_dialog(actions.clone(), cx) {
            return Some(dialog);
        }
        actions.read(cx).move_dialog.as_ref().map(|dialog| dialog.picker.clone().into_any_element())
    }
}
