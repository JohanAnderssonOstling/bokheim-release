//! Browser route definitions.

use crate::library::BrowseKind;

/// A page whose state belongs to one library session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LibraryRoute {
    Home,
    Books,
    Authors,
    Folders,
    Subjects,
    Catalog,
    Trash,
}

impl LibraryRoute {
    pub(crate) fn browse_kind(self) -> Option<BrowseKind> {
        match self {
            Self::Home => Some(BrowseKind::Home),
            Self::Books => Some(BrowseKind::Books),
            Self::Folders => Some(BrowseKind::Folder),
            Self::Subjects => Some(BrowseKind::Subject),
            Self::Authors => Some(BrowseKind::Author),
            Self::Catalog => Some(BrowseKind::Catalog),
            Self::Trash => None,
        }
    }
}

/// A page shared across library selections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum GlobalRoute {
    Settings,
}

/// The currently displayed browser destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Route {
    Library(LibraryRoute),
    Global(GlobalRoute),
}

impl Route {
    pub(crate) fn presentation(self) -> (&'static str, ui_components::NavigationIcon) {
        use ui_components::NavigationIcon;
        match self {
            Self::Library(LibraryRoute::Home) => ("Home", NavigationIcon::House),
            Self::Library(LibraryRoute::Books) => ("Books", NavigationIcon::BookOpen),
            Self::Library(LibraryRoute::Authors) => ("Authors", NavigationIcon::User),
            Self::Library(LibraryRoute::Folders) => ("Folders", NavigationIcon::Folder),
            Self::Library(LibraryRoute::Subjects) => ("Subjects", NavigationIcon::Tags),
            Self::Library(LibraryRoute::Catalog) => ("Catalog", NavigationIcon::Building2),
            Self::Library(LibraryRoute::Trash) => ("Trash", NavigationIcon::Trash2),
            Self::Global(GlobalRoute::Settings) => ("Settings", NavigationIcon::Settings),
        }
    }
}

impl From<LibraryRoute> for Route {
    fn from(route: LibraryRoute) -> Self {
        Self::Library(route)
    }
}

impl From<GlobalRoute> for Route {
    fn from(route: GlobalRoute) -> Self {
        Self::Global(route)
    }
}
