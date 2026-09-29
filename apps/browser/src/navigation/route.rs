//! Browser route definitions.

use sync_common::LibraryId;

/// Library identity and platform-specific location displayed by the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LibrarySummary {
    library_id: LibraryId,
    library_name: String,
    #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    local_path: Option<String>,
}

impl LibrarySummary {
    pub(crate) fn new(library_id: LibraryId, library_name: impl Into<String>) -> Self {
        Self {
            library_id,
            library_name: library_name.into(),
            #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            local_path: None,
        }
    }

    pub(crate) fn from_entry(entry: &app::LibraryEntry) -> Self {
        Self {
            library_id: *entry.library_id(),
            library_name: entry.library_name().to_owned(),
            #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            local_path: Some(entry.storage_locator().to_owned()),
        }
    }

    #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub(crate) fn local_path(&self) -> Option<&str> {
        self.local_path.as_deref()
    }

    pub(crate) fn library_id(&self) -> &LibraryId {
        &self.library_id
    }

    pub(crate) fn library_name(&self) -> &str {
        &self.library_name
    }
}

/// A page whose state belongs to one library session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryRoute {
    Home,
    Authors,
    Folders,
    Subjects,
    Trash,
}

/// A page shared across library selections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobalRoute {
    Settings,
}

/// The currently displayed browser destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    Library(LibraryRoute),
    Global(GlobalRoute),
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
