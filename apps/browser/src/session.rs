//! Device-local navigation restored independently of library contents.

use std::rc::Rc;

use gpui::App;
use library_model::{BookLocator, BrowseLocation};
use serde::{Deserialize, Serialize};
use sync_common::{BookId, LibraryId};

use crate::{LibraryRoute, Route};

pub type SaveNavigationSession = Rc<dyn Fn(NavigationSession, &mut App)>;

/// Requests composition outside the browser so readers and playback split too.
#[derive(Clone)]
pub enum PaneRequest {
    Open { session: NavigationSession, side: ui_components::SplitSide },
    Close,
    Browse,
}

/// Restoration input and persistence policy for one window. Runtime state is
/// owned by its application root and browser, not by application services.
pub struct WindowSession {
    pub initial: NavigationSession,
    pub save: SaveNavigationSession,
}

impl Default for WindowSession {
    fn default() -> Self {
        Self { initial: NavigationSession::default(), save: Rc::new(|_, _| {}) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderSession {
    pub locator: BookLocator,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowsePageSession {
    pub path: Vec<(BrowseLocation, String)>,
    pub detail: Option<BookId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NavigationSession {
    pub library_id: Option<LibraryId>,
    pub route: Route,
    pub pages: Vec<BrowsePageSession>,
    pub reader: Option<ReaderSession>,
    pub pane_id: Option<uuid::Uuid>,
    pub panes: Vec<NavigationSession>,
    pub active_pane: usize,
}

impl Default for NavigationSession {
    fn default() -> Self {
        Self { library_id: None, route: Route::Library(LibraryRoute::Home), pages: Vec::new(), reader: None, pane_id: None, panes: Vec::new(), active_pane: 0 }
    }
}

impl NavigationSession {
    /// A removed library must not restore its pages into a different library.
    pub(crate) fn reconcile(&mut self, libraries: &[LibraryId]) {
        for pane in &mut self.panes {
            pane.reconcile(libraries);
        }
        self.active_pane = self.active_pane.min(self.panes.len().saturating_sub(1));
        if self.library_id.is_some_and(|id| !libraries.contains(&id)) {
            self.library_id = None;
            self.pages.clear();
            if matches!(self.route, Route::Library(_)) {
                self.route = Route::Library(LibraryRoute::Home);
            }
        }
        self.library_id = self.library_id.or_else(|| libraries.first().copied());
        if self.reader.as_ref().is_some_and(|reader| !libraries.contains(reader.locator.library_id())) {
            self.reader = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlobalRoute;

    #[test]
    fn existing_library_and_reader_survive_reconciliation() {
        let first = LibraryId::from_u128(1);
        let selected = LibraryId::from_u128(2);
        let mut session = NavigationSession {
            library_id: Some(selected),
            route: Route::Library(LibraryRoute::Folders),
            pages: vec![BrowsePageSession { path: vec![(BrowseLocation::Folder(LibraryId::from_u128(3)), "Fiction".into())], detail: None }],
            reader: Some(ReaderSession { locator: BookLocator::new(selected, BookId::from_bytes([7; 32])), title: "Book".into() }),
            ..Default::default()
        };
        let expected = session.clone();
        session.reconcile(&[first, selected]);
        assert_eq!(session, expected);
    }

    #[test]
    fn removed_library_cannot_restore_its_pages_or_reader_in_another_library() {
        let removed = LibraryId::from_u128(1);
        let remaining = LibraryId::from_u128(2);
        let mut session = NavigationSession {
            library_id: Some(removed),
            route: Route::Library(LibraryRoute::Folders),
            pages: vec![BrowsePageSession::default()],
            reader: Some(ReaderSession { locator: BookLocator::new(removed, BookId::from_bytes([7; 32])), title: "Book".into() }),
            ..Default::default()
        };
        session.reconcile(&[remaining]);
        assert_eq!(session.library_id, Some(remaining));
        assert_eq!(session.route, Route::Library(LibraryRoute::Home));
        assert!(session.pages.is_empty());
        assert!(session.reader.is_none());
        session.route = Route::Global(GlobalRoute::Settings);
        session.reconcile(&[]);
        assert_eq!(session.library_id, None);
        assert_eq!(session.route, Route::Global(GlobalRoute::Settings));
    }
}
