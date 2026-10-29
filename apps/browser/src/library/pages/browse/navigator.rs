//! Where a browse page is and what it lists there: the location, the sort and
//! filter choices, the installed listing, and the request that replaces it.
//! The page's views read it and redraw when it loads; none of them load
//! anything themselves.

use ui_components::user_facing_error;

use gpui::{Context, EventEmitter, SharedString, Subscription, Task};
use library_backend::{BackendError, LibraryBrowseData};
use library_model::{BrowseBookPage, BrowseLocation, BrowsePageRequest, BrowsePathSegment, LibraryBrowseQuery};
use std::{future::Future, ops::Range};

use super::controls::{BrowseOptionsEvent, BrowseOptionsSelection};
use super::listing::{BrowseDestination, BrowseItem, BrowseKind, BrowseListing, chip_id};
use super::profile::Stage;
use crate::library::services::{LibraryContentsChanged, LibraryContext, LibraryDownloadChanged};

/// A listing was installed.
pub(super) struct Loaded {
    /// The location the page was on before, when this load moved it.
    pub(super) left: Option<BrowseLocation>,
    /// Whether this load refreshed the same contents rather than replacing them.
    pub(super) refresh: bool,
}

pub(super) struct BrowseNavigator {
    ui: LibraryContext,
    pub(super) kind: BrowseKind,
    pub(super) destination: BrowseDestination,
    pub(super) listing: BrowseListing,
    pub(super) selection: BrowseOptionsSelection,
    pub(super) error: Option<SharedString>,
    /// The load in flight. Replacing it drops the one before, which cancels it.
    request: Option<Task<()>>,
    /// A feed's next page being fetched, to append. A load drops it.
    more: Option<Task<()>>,
    /// Infer and save an initial selection only when no preference is stored.
    catalog_languages_pending: bool,
    page_failed: bool,
    loaded_query: Option<LibraryBrowseQuery>,
    refresh_pending: bool,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<Loaded> for BrowseNavigator {}

impl BrowseNavigator {
    pub(super) fn library_id(&self) -> uuid::Uuid {
        *self.ui.backend().id()
    }

    /// A displayed section belongs to the installed response, even while a
    /// different filter or sort is still being loaded in the background.
    pub(super) fn section_request(&self, section: BrowseLocation, offset: usize, limit: usize) -> Option<impl Future<Output = Result<Option<BrowseBookPage>, BackendError>> + use<>> {
        let mut query = self.loaded_query.clone()?;
        let revision = self.listing.page.as_ref().and_then(|page| page.result_revision.clone());
        let library = self.ui.backend().clone();
        Some(async move {
            if let Some(revision) = revision {
                library.browse_result_books(query, revision, section, offset, limit).await
            } else {
                query.location = section;
                library.browse_section_books(query, offset, limit).await.map(Some)
            }
        })
    }

    pub(super) fn new(ui: LibraryContext, kind: BrowseKind, listing: BrowseListing, cx: &mut Context<Self>) -> Self {
        let mut selection = BrowseOptionsSelection::for_controls(&listing.controls);
        // The catalog opens grouped as it was last left.
        selection.by_author = kind == BrowseKind::Catalog && crate::stores::current_catalog_grouping(cx) == app_preferences::CatalogGrouping::Author;
        let stored_languages = crate::stores::current_catalog_languages(cx);
        let catalog_languages_pending = kind == BrowseKind::Catalog && stored_languages.is_none();
        if kind == BrowseKind::Catalog {
            if let Some(languages) = stored_languages {
                selection.languages = languages;
            }
        }
        let updates = ui.updates();
        let subscriptions = [
            cx.subscribe(&updates, |navigator, _, _: &LibraryContentsChanged, cx| navigator.invalidate(cx)),
            cx.subscribe(&updates, |navigator, _, _: &LibraryDownloadChanged, cx| navigator.invalidate(cx)),
        ];
        let root = if selection.by_author { BrowseLocation::CatalogAuthor(None) } else { kind.root_location() };
        let mut navigator = Self {
            ui,
            kind,
            destination: BrowseDestination::from_path(root, Vec::new()),
            listing,
            selection,
            error: None,
            request: None,
            more: None,
            catalog_languages_pending,
            loaded_query: None,
            refresh_pending: false,
            page_failed: false,
            _subscriptions: subscriptions,
        };
        navigator.reload(cx);
        navigator
    }

    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.load(self.destination.clone(), false, cx);
    }

    /// Refreshes scanner-driven content without replacing the current page by
    /// a loading frame. Newly discovered covers can prepare in place while the
    /// already-visible book cards remain painted.
    /// A feed page is fetched from the feed, which a library change does not
    /// touch, and a refresh would drop the pages appended to it.
    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.destination.location, BrowseLocation::Opds { .. }) {
            self.load(self.destination.clone(), true, cx);
        }
    }

    /// Hidden cached pages retain one invalidation instead of issuing queries
    /// for every scan batch or completed download. The displayed page consumes
    /// it from render, after any request already in flight has finished.
    fn invalidate(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.destination.location, BrowseLocation::Opds { .. }) && !self.refresh_pending {
            self.refresh_pending = true;
            cx.notify();
        }
    }

    pub(super) fn refresh_if_needed(&mut self, cx: &mut Context<Self>) {
        if self.refresh_pending && self.request.is_none() && self.more.is_none() {
            self.refresh(cx);
        }
    }

    pub(super) fn open_path(&mut self, path: Vec<BrowsePathSegment>, cx: &mut Context<Self>) {
        self.load(BrowseDestination::from_path(self.root(), path), false, cx);
    }

    /// Where the page's empty path leads: the catalog shown by author has
    /// its author index there instead of its subjects.
    pub(super) fn root(&self) -> BrowseLocation {
        if self.selection.by_author { BrowseLocation::CatalogAuthor(None) } else { self.kind.root_location() }
    }

    /// Searches for `query`, or drops the search for an empty one.
    pub(super) fn search(&mut self, query: String, cx: &mut Context<Self>) {
        self.selection.search = query;
        self.reload(cx);
    }

    pub(super) fn select_option(&mut self, event: BrowseOptionsEvent, cx: &mut Context<Self>) {
        // A feed's own choice is another page of the feed, in place of this one.
        if let BrowseOptionsEvent::OpenFeedPage(location) = event {
            let mut path = self.destination.path.clone();
            if let Some(crumb) = path.last_mut() {
                crumb.id = location;
            }
            self.open_path(path, cx);
            return;
        }
        if self.selection.apply(event.clone(), &self.listing.controls) {
            if self.kind == BrowseKind::Catalog {
                match &event {
                    BrowseOptionsEvent::ToggleLanguage(_) => {
                        self.catalog_languages_pending = false;
                        crate::stores::save_catalog_languages(Some(self.selection.languages.clone()), cx);
                    }
                    _ => {}
                }
            }
            // A grouping is another root, not a filter of this page, and the
            // catalog opens with it next time.
            if let BrowseOptionsEvent::ShowByAuthor(by_author) = event {
                crate::stores::save_catalog_grouping(if by_author { app_preferences::CatalogGrouping::Author } else { app_preferences::CatalogGrouping::Subject }, cx);
                self.open_path(Vec::new(), cx);
            } else {
                self.reload(cx);
            }
        }
    }

    /// Fetches the feed's next page and appends it to the listing, unless one
    /// is already on its way.
    pub(super) fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.listing.next else { return };
        if self.more.is_some() {
            return;
        }
        // The next page's address already carries any search.
        let mut query = self.selection.backend_query(next, true);
        query.search.clear();
        let ui = self.ui.clone();
        self.more = Some(cx.spawn(async move |navigator, cx| {
            let result = fetch(&ui, query).await;
            let _ = navigator.update(cx, |navigator, cx| {
                navigator.more = None;
                match result {
                    // A load since has replaced the listing this page continued.
                    Ok(_) if navigator.listing.next != Some(next) => {}
                    Ok(data) => {
                        navigator.listing.append(data, cx);
                        cx.emit(Loaded { left: None, refresh: true });
                    }
                    Err(error) => {
                        log::warn!("Loading the next feed page failed: {error}");
                        navigator.error = Some(user_facing_error(&error, "Could not load this page. Please try again.").into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Load visible outer panels or direct covers using the installed query.
    pub(super) fn load_page_range(&mut self, range: Range<usize>, compact: usize, cx: &mut Context<Self>) {
        if self.more.is_some() || self.request.is_some() || self.page_failed {
            return;
        }
        let Some(page) = self.listing.missing_page(range, compact) else { return };
        let Some(query) = self.loaded_query.clone() else { return };
        let revision = self.listing.revision;
        let ui = self.ui.clone();
        let result_revision = self.listing.page.as_ref().and_then(|page| page.result_revision.clone());
        self.more = Some(cx.spawn(async move |navigator, cx| {
            let request_stage = Stage::new("outer page request including transport");
            let result = match result_revision {
                Some(revision) => ui.backend().browse_result_page(query, revision, page).await,
                None => ui.backend().browse_page(query, page).await.map(Some),
            };
            drop(request_stage);
            let _ = navigator.update(cx, |navigator, cx| {
                let _stage = Stage::new("outer page UI merge");
                if navigator.listing.revision != revision {
                    return;
                }
                navigator.more = None;
                match result {
                    Ok(Some(data)) => {
                        if navigator.listing.append_page(data, cx) {
                            cx.emit(Loaded { left: None, refresh: true });
                        } else {
                            navigator.refresh(cx);
                        }
                    }
                    Ok(None) => navigator.refresh(cx),
                    Err(error) => {
                        navigator.page_failed = true;
                        navigator.error = Some(user_facing_error(&error, "Could not load this page. Please try again.").into());
                    }
                }
                cx.notify();
            });
        }));
    }

    /// The trail to the child `child_id`: under the current path, through its
    /// group where the group is a real folder. The backend supplies the full
    /// displayed breadcrumb trail after the destination loads.
    pub(super) fn child_path(&self, child_id: BrowseLocation) -> Option<Vec<BrowsePathSegment>> {
        let item = self.listing.item(&chip_id(child_id))?;
        let child = item.child()?;
        let mut path = self.destination.path.clone();
        if let BrowseItem::Child { parent: Some(parent), .. } = item {
            let parent = self.listing.child(BrowseLocation::Folder(*parent))?;
            path.push(BrowsePathSegment { id: parent.id, name: parent.name.clone() });
        }
        path.push(BrowsePathSegment { id: child.id, name: child.name.clone() });
        Some(path)
    }

    /// Starts a load in place of any still running; the installed frame
    /// remains painted.
    fn load(&mut self, destination: BrowseDestination, refresh: bool, cx: &mut Context<Self>) {
        self.refresh_pending = false;
        let location = destination.location;
        let query = self.selection.backend_query(location, true);
        #[cfg(target_os = "android")]
        log::info!("Browse request: location={} formats={:?} languages={:?} search_length={}", location, query.file_types, query.languages, query.search.len());
        // Navigation keeps the current frame painted until the next one is
        // ready. A browse query is a local read that returns inside a frame,
        // so covering the grid in the meantime only ever produced a flash —
        // one that appeared going back up the tree and not going down it, for
        // the same query against the same database.
        let ui = self.ui.clone();
        let searched = !query.search.is_empty();
        let infer_languages = self.catalog_languages_pending && matches!(location, BrowseLocation::Catalog(_) | BrowseLocation::CatalogAuthor(_));
        let system_languages = if infer_languages { sys_locale::get_locales().collect::<Vec<_>>() } else { Vec::new() };
        self.error = None;
        // A next page belongs to the listing this load replaces.
        self.more = None;
        self.page_failed = false;
        self.request = Some(cx.spawn(async move |navigator, cx| {
            let request_stage = Stage::new("initial browse request including transport");
            let result = fetch(&ui, query.clone()).await;
            drop(request_stage);
            let library_languages = if infer_languages && result.is_ok() {
                ui.backend().library_language_counts().await.unwrap_or_else(|error| {
                    log::warn!("could not infer catalog languages from the library: {error}");
                    Vec::new()
                })
            } else {
                Vec::new()
            };
            let _ = navigator.update(cx, |navigator, cx| {
                if infer_languages && navigator.catalog_languages_pending {
                    if let Ok(data) = &result {
                        // Another window may already have saved a choice while
                        // this request was in flight. Keep that choice too.
                        let stored = crate::stores::current_catalog_languages(cx);
                        let needs_save = stored.is_none();
                        navigator.selection.languages = super::languages::initial_selection(stored, &system_languages, &library_languages, &data.language_counts);
                        navigator.catalog_languages_pending = false;
                        if needs_save {
                            crate::stores::save_catalog_languages(Some(navigator.selection.languages.clone()), cx);
                        }
                        navigator.load(destination, refresh, cx);
                        return;
                    }
                }
                navigator.finish_request(destination, refresh, searched, query, result, cx);
            });
        }));
    }

    fn finish_request(&mut self, mut destination: BrowseDestination, refresh: bool, searched: bool, query: LibraryBrowseQuery, result: Result<LibraryBrowseData, BackendError>, cx: &mut Context<Self>) {
        self.request = None;
        match result {
            Ok(data) => {
                let left = (self.destination.location != destination.location).then_some(self.destination.location);
                let path = {
                    let _stage = Stage::new("initial browse UI installation");
                    self.listing.install(data, searched, cx)
                };
                // A feed page knows nothing of how it was reached; its trail is
                // the chips followed to it.
                if !matches!(destination.location, BrowseLocation::Opds { .. }) {
                    destination.path = path;
                }
                self.destination = destination;
                self.loaded_query = Some(query);
                self.error = None;
                #[cfg(target_os = "android")]
                log::info!("Browse loaded: location={} children={} books={}", self.destination.location, self.listing.children.len(), self.listing.books.len());
                cx.emit(Loaded { left, refresh });
            }
            Err(error) => {
                log::warn!("Browse failed for {}: {}", destination.location, error);
                self.error = Some(user_facing_error(&error, "Could not load this page. Please try again.").into());
            }
        }
        cx.notify();
    }
}

/// A page from where it lives: a feed page from the feed, through the app,
/// and anything else from the library. The catalog's root also lists the
/// account's OPDS feeds, each as a chip without a section.
async fn fetch(ui: &LibraryContext, query: LibraryBrowseQuery) -> Result<LibraryBrowseData, BackendError> {
    if matches!(query.location, BrowseLocation::Opds { .. }) {
        return ui.app().browse_opds(query).await;
    }
    let feeds_listed = matches!(query.location, BrowseLocation::Catalog(library_model::ROOT_SUBJECT_ROUTE_ID) | BrowseLocation::CatalogAuthor(None)) && query.search.is_empty();
    let mut data = ui.backend().browse_page(query, BrowsePageRequest::default()).await?;
    if feeds_listed {
        // Without its feeds the catalog still lists; the failure is logged.
        let feeds = ui.app().opds_feeds().await.unwrap_or_else(|error| {
            log::warn!("could not list OPDS feeds: {error}");
            Vec::new()
        });
        for feed in feeds {
            data.contents.children.push(library_model::BrowseRow { id: BrowseLocation::Opds { source: feed.id, page: None }, name: feed.name, path: None, book_count: 0, downloaded_book_count: 0 });
        }
        let stores = ui.app().stores().await.unwrap_or_else(|error| {
            log::warn!("could not list stores: {error}");
            Vec::new()
        });
        for store in stores {
            data.contents.children.push(library_model::BrowseRow { id: BrowseLocation::Store(store.id), name: store.name, path: None, book_count: 0, downloaded_book_count: 0 });
        }
    }
    Ok(data)
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use super::super::controls::BrowseControls;
    use crate::navigation::LibrarySession;
    use gpui::{AppContext, TestAppContext};
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    async fn section_requests_capture_the_installed_query_and_outlive_the_navigator(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        let root = sync_common::ROOT_DIR_ID;
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/library-backend/crates/book-inspection/tests/fixtures/back-matter-evidence.epub");
        assert!(library.backend().import_paths(root, vec![fixture], false).await.unwrap().is_empty());
        let query = BrowseOptionsSelection::for_controls(&BrowseControls::library()).backend_query(BrowseLocation::Books, true);
        let hash = library.backend().browse_section_books(query, 0, 1).await.unwrap().books[0].library_file().unwrap();
        library.backend().create_directory(root, "Section".into()).await.unwrap();
        let folder = library.backend().folder_destinations().await.unwrap().into_iter().find(|folder| folder.label == "Section").unwrap().id;
        library.backend().move_book_placement(hash, root, folder).await.unwrap();
        let section = BrowseLocation::Folder(folder);
        let listing = BrowseListing::new(library.clone(), BrowseKind::Folder, Rc::new(|_, _, _| {}), false);
        let navigator = cx.new(|cx| BrowseNavigator::new(library, BrowseKind::Folder, listing, cx));
        assert!(navigator.read_with(cx, |navigator, _| navigator.section_request(section, 0, 10).is_none()));
        cx.condition(&navigator, |navigator, _| navigator.request.is_none()).await;
        let (snapshot, direct) = navigator.update(cx, |navigator, _| {
            assert!(navigator.listing.page.as_ref().unwrap().result_revision.is_some());
            // Editing controls must not change the query of the installed listing.
            navigator.selection.search = "Nothing should match these pending controls".into();
            let snapshot = navigator.section_request(section, 0, 10).unwrap();
            navigator.listing.page.as_mut().unwrap().result_revision = None;
            let direct = navigator.section_request(section, 0, 10).unwrap();
            navigator.loaded_query = None;
            assert!(navigator.section_request(section, 0, 10).is_none());
            (snapshot, direct)
        });
        drop(navigator);
        for page in [snapshot.await.unwrap().unwrap(), direct.await.unwrap().unwrap()] {
            assert_eq!(page.total, 1);
            assert_eq!(page.books[0].library_file(), Some(hash));
        }
    }

    #[gpui::test]
    async fn hidden_pages_defer_and_coalesce_refreshes_without_losing_inflight_changes(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let updates = library.updates();
        cx.condition(&updates, |updates, _| !updates.loading() && !updates.scanning()).await;
        let listing = BrowseListing::new(library.clone(), BrowseKind::Books, Rc::new(|_, _, _| {}), false);
        let navigator = cx.new(|cx| BrowseNavigator::new(library, BrowseKind::Books, listing, cx));
        cx.condition(&navigator, |navigator, _| navigator.request.is_none()).await;
        let loads = Rc::new(Cell::new(0));
        let completed = loads.clone();
        let _subscription = cx.update(|cx| cx.subscribe(&navigator, move |_, _: &Loaded, _| completed.set(completed.get() + 1)));

        for _ in 0..10 {
            updates.update(cx, |_, cx| cx.emit(LibraryContentsChanged));
        }
        cx.run_until_parked();
        cx.read_entity(&navigator, |navigator, _| {
            assert!(navigator.refresh_pending);
            assert!(navigator.request.is_none(), "hidden pages must not issue background browse requests");
        });
        assert_eq!(loads.get(), 0);

        // Rendering the page consumes the whole burst as a single refresh.
        navigator.update(cx, |navigator, cx| navigator.refresh_if_needed(cx));
        cx.condition(&navigator, |navigator, _| navigator.request.is_none()).await;
        assert_eq!(loads.get(), 1);

        updates.update(cx, |_, cx| cx.emit(LibraryContentsChanged));
        navigator.update(cx, |navigator, cx| navigator.refresh_if_needed(cx));
        // A later change must survive the response of the request already sent.
        updates.update(cx, |_, cx| cx.emit(LibraryContentsChanged));
        navigator.update(cx, |navigator, cx| {
            assert!(navigator.request.is_some());
            navigator.refresh_if_needed(cx);
            assert!(navigator.refresh_pending);
        });
        cx.condition(&navigator, |navigator, _| navigator.request.is_none()).await;
        assert_eq!(loads.get(), 2);
        navigator.update(cx, |navigator, cx| navigator.refresh_if_needed(cx));
        cx.condition(&navigator, |navigator, _| navigator.request.is_none()).await;
        assert_eq!(loads.get(), 3);
    }
}
