use futures_util::future::join_all;
use gpui::{App, Context, Entity, FileDialogFilter, PathPromptOptions, Window};
use library_backend::LibraryClient;
use library_model::BookLocator;
use std::rc::Rc;
use sync_common::{ContentHash, DirId};

use super::{LibraryUpdateBridge, UndoStack};
pub(crate) type OpenBook = Rc<dyn Fn(BookLocator, String, Option<String>, &mut Window, &mut App)>;
/// Notifies the owning shell that the cached book detail page just closed, so
/// e.g. an audiobook dock expanded to show it can collapse back down.
pub(crate) type BookDetailClosed = Rc<dyn Fn(&mut App)>;

/// UI state associated with one selected library.
///
/// Keeps the selected concrete backend together with GPUI-specific callbacks
/// and events.
#[derive(Clone)]
pub(crate) struct LibraryContext {
    backend: Rc<LibraryClient>,
    open_book: OpenBook,
    book_detail_closed: BookDetailClosed,
    updates: Entity<LibraryUpdateBridge>,
    undo: UndoStack,
}

impl LibraryContext {
    pub(crate) fn new(backend: Rc<LibraryClient>, open_book: OpenBook, book_detail_closed: BookDetailClosed, updates: Entity<LibraryUpdateBridge>) -> Self {
        Self { backend, open_book, book_detail_closed, updates, undo: UndoStack::default() }
    }

    pub(crate) fn backend(&self) -> &LibraryClient {
        self.backend.as_ref()
    }

    pub(crate) fn undo(&self) -> &UndoStack {
        &self.undo
    }

    pub(crate) fn updates(&self) -> Entity<LibraryUpdateBridge> {
        self.updates.clone()
    }

    /// Prompts for one book and imports it into the requested folder.
    ///
    /// Successful imports are observed through [`LibraryUpdateBridge`], so
    /// callers do not need to reload their page explicitly.
    pub(crate) fn choose_and_import_book<C: 'static>(&self, parent_id: DirId, cx: &mut Context<C>) {
        let extensions = crate::BOOK_IMPORT_EXTENSIONS.into_iter().map(str::to_owned).collect();
        let selection = cx.prompt_for_files_with_filters(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Add book".into()) }, vec![FileDialogFilter { name: "Books".to_owned(), extensions }]);
        let library = self.backend.clone();
        cx.spawn(async move |view, cx| {
            let result = match selection.await {
                Ok(Ok(Some(files))) => match files.into_iter().next() {
                    Some(file) => {
                        // The browser coordinator owns its import notifications; native
                        // imports retain an activity-scoped notification locally.
                        #[cfg(not(target_arch = "wasm32"))]
                        let activity = uuid::Uuid::new_v4();
                        #[cfg(not(target_arch = "wasm32"))]
                        let _progress = view.update_in(cx, |_, window, cx| crate::services::ImportProgressToast::start_for_activity((*library).clone(), activity, window, cx)).ok();
                        let result = {
                            let mut source = file.source;
                            source.path = vec![file.name.clone()];
                            let directory = crate::directory_import(gpui::SelectedDirectory { local_path: None, name: file.name, directories: Vec::new(), files: vec![source] });
                            #[cfg(target_arch = "wasm32")]
                            {
                                let result = library.import_directory_contents(parent_id, directory).await;
                                let _ = result;
                                Ok(())
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                library
                                    .import_directory_contents_with_activity(parent_id, directory, activity)
                                    .await
                                    .and_then(|failures| if failures.is_empty() { Ok(()) } else { Err(failures.iter().map(crate::services::import_failure_message).collect::<Vec<_>>().join("\n")) })
                            }
                        };
                        result.map(|_| ())
                    }
                    None => return,
                },
                Ok(Ok(None)) => return,
                Ok(Err(error)) => Err(error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if let Err(error) = result {
                let _ = view.update_in(cx, |_, window, cx| {
                    crate::services::notify_error("add-book-error", format!("Could not add book: {error}"), window, cx);
                });
            }
        })
        .detach();
    }

    /// Imports paths handed over by the desktop — a drop from the file manager.
    ///
    /// Files are imported as books and directories are imported whole, which is
    /// what the Add book and Add folder commands do with a chosen path; a drop
    /// is the same import with the picker skipped.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn import_dropped_paths<C: 'static>(&self, parent_id: DirId, paths: Vec<std::path::PathBuf>, cx: &mut Context<C>) {
        let started = std::time::Instant::now();
        log::info!(target: "import_timing", "drop_received paths={}", paths.len());
        let library = self.backend.clone();
        let reader = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            let _progress = view.update_in(cx, |_, window, cx| crate::services::ImportProgressToast::start((*library).clone(), window, cx)).ok();
            let mut failures = Vec::new();
            let mut imported = 0usize;
            // Keep all dropped files in one directory-import transaction. The
            // old path imported every file separately, which repeated the
            // prepare/finish protocol and emitted a contents refresh for each
            // file. Folders retain their existing root semantics and are
            // still imported independently.
            let mut dropped_files = app::DirectoryImport { name: "Dropped files".to_owned(), directories: Vec::new(), files: Vec::new() };
            let mut dropped_file_count = 0usize;
            let scanned_paths = join_all(paths.into_iter().map(|path| {
                let reader = reader.clone();
                async move { reader.spawn(async move { scan_dropped_path(&path) }).await }
            }))
            .await;
            log::info!(target: "import_timing", "drop_paths_prepared elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
            for scanned in scanned_paths {
                match scanned {
                    Ok(import) => {
                        if import.create_root {
                            match library.import_directory(parent_id, import.directory).await {
                                Ok(errors) => {
                                    if errors.is_empty() {
                                        imported += 1;
                                    }
                                    failures.extend(errors.iter().map(crate::services::import_failure_message));
                                }
                                Err(error) => failures.push(error),
                            }
                        } else {
                            dropped_file_count += import.directory.files.len();
                            dropped_files.files.extend(import.directory.files);
                        }
                    }
                    Err(error) => failures.push(error),
                }
            }
            if !dropped_files.files.is_empty() {
                log::info!(target: "import_timing", "drop_submitting files={} elapsed_ms={:.3}", dropped_files.files.len(), started.elapsed().as_secs_f64() * 1000.0);
                match library.import_directory_contents(parent_id, dropped_files).await {
                    Ok(errors) => {
                        imported += dropped_file_count.saturating_sub(errors.len());
                        failures.extend(errors.iter().map(crate::services::import_failure_message));
                    }
                    Err(error) => failures.push(error),
                }
                log::info!(target: "import_timing", "drop_import_returned elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
            }
            let _ = view.update_in(cx, |_, window, cx| {
                if failures.is_empty() {
                    return;
                }
                let summary = if imported > 0 { format!("Added {imported}, but {} item(s) could not be added. {}", failures.len(), failures[0]) } else { format!("Could not add dropped items. {}", failures[0]) };
                crate::services::notify_warning("drop-import-warning", summary, window, cx);
            });
            drop(_progress);
            log::info!(target: "import_timing", "drop_finished elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
        })
        .detach();
    }

    pub(crate) fn open_book(&self, content_hash: ContentHash, title: String, window: &mut Window, cx: &mut App) {
        self.open_book_at(content_hash, title, None, window, cx);
    }

    /// Opens the book where the reader asked for it — a contents entry rather
    /// than wherever they left off. The target is the book's own, so the
    /// reader that receives it is the one that knows how to resolve it.
    pub(crate) fn open_book_at(&self, content_hash: ContentHash, title: String, target: Option<String>, window: &mut Window, cx: &mut App) {
        let locator = BookLocator::new(*self.backend.id(), content_hash);
        (self.open_book)(locator, title, target, window, cx);
    }

    pub(crate) fn notify_book_detail_closed(&self, cx: &mut App) {
        (self.book_detail_closed)(cx);
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct DroppedImport {
    directory: app::DirectoryImport,
    create_root: bool,
}

/// Classifies a dropped path and enumerates folders on a background thread.
/// Book bytes are opened by the backend only when the import starts.
#[cfg(not(target_arch = "wasm32"))]
fn scan_dropped_path(path: &std::path::Path) -> Result<DroppedImport, String> {
    let metadata = path.metadata().map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.is_dir() {
        let selected = gpui::SelectedDirectory::from_path(path, crate::BOOK_IMPORT_EXTENSIONS).map_err(|error| format!("{error:#}"))?;
        return Ok(DroppedImport { directory: crate::directory_import(selected), create_root: true });
    }
    if !metadata.is_file() || !is_importable_book(path) {
        return Err(format!("{} is not a book or folder", path.display()));
    }
    let name = path.file_name().ok_or_else(|| format!("{} has no file name", path.display()))?.to_string_lossy().into_owned();
    let path = path.to_path_buf();
    let file = app::DirectoryImportFile {
        path: vec![name.clone()],
        source: app::ImportSource::new(Box::new(move || Box::pin(async move { std::fs::File::open(&path).map(|file| Box::new(file) as app::DirectoryImportReader).map_err(|error| error.to_string()) }))).with_size_bytes(Some(metadata.len())),
    };
    Ok(DroppedImport { directory: app::DirectoryImport { name, directories: Vec::new(), files: vec![file] }, create_root: false })
}

#[cfg(not(target_arch = "wasm32"))]
fn is_importable_book(path: &std::path::Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| crate::BOOK_IMPORT_EXTENSIONS.iter().any(|allowed| allowed.eq_ignore_ascii_case(extension)))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use futures_util::FutureExt as _;
    use std::io::Read as _;

    #[test]
    fn dropped_book_is_opened_lazily_and_keeps_the_source() {
        let path = std::env::temp_dir().join(format!("drop-import-{}.epub", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"before selection").unwrap();
        let selected = scan_dropped_path(&path).unwrap();
        assert!(!selected.create_root);
        assert!(selected.directory.directories.is_empty());
        assert_eq!(selected.directory.files.len(), 1);
        std::fs::write(&path, b"when import starts").unwrap();
        let file = selected.directory.files.into_iter().next().unwrap();
        let mut reader = file.source.open().now_or_never().unwrap().unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"when import starts");
        drop(reader);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropped_folder_uses_picker_filters_and_skips_symlink_cycles() {
        let root = std::env::temp_dir().join(format!("drop-folder-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Shelf")).unwrap();
        std::fs::create_dir(root.join("Empty")).unwrap();
        std::fs::write(root.join("Shelf/book.EPUB"), b"before selection").unwrap();
        std::fs::write(root.join("notes.txt"), b"not a book").unwrap();
        std::os::unix::fs::symlink(&root, root.join("Shelf/loop")).unwrap();
        std::os::unix::fs::symlink(root.join("Shelf/book.EPUB"), root.join("linked.epub")).unwrap();
        let selected = scan_dropped_path(&root).unwrap();
        assert!(selected.create_root);
        assert_eq!(selected.directory.directories, vec![vec!["Empty"], vec!["Shelf"]]);
        assert_eq!(selected.directory.files.len(), 1);
        let file = selected.directory.files.into_iter().next().unwrap();
        assert_eq!(file.path, vec!["Shelf", "book.EPUB"]);
        std::fs::write(root.join("Shelf/book.EPUB"), b"when import starts").unwrap();
        let mut reader = file.source.open().now_or_never().unwrap().unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"when import starts");
        drop(reader);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_drop_reports_the_path() {
        let path = std::env::temp_dir().join(format!("missing-drop-{}.epub", uuid::Uuid::new_v4()));
        let error = scan_dropped_path(&path).err().unwrap();
        assert!(error.contains(path.to_str().unwrap()));
    }
}
