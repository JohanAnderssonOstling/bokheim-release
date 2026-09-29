//! Device and synchronized application preferences.

pub use app_preferences::*;
use std::io;

const PREFERENCES_FILE: &str = "reader_preferences.json";
const BROWSING_PREFERENCES_FILE: &str = "browsing_preferences.json";

impl AppState {
    fn read_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> io::Result<Option<T>> {
        self.registry.read_value(key).map_err(|error| io::Error::other(error.to_string()))?.map(|bytes| serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))).transpose()
    }

    /// Browsing preferences, or fresh ones if what is on disk cannot be read.
    ///
    /// Unlike the reader preferences below, this discards rather than fails
    /// closed. The two files hold different kinds of thing: a reader profile is
    /// someone's place in their books and their typography, and silently
    /// resetting it would lose work, so it stays and the load errors. This file
    /// is a theme, a font size and a view mode — a minute to set again — and it
    /// is the one whose shape changes when a theme is renamed or a preference
    /// retired. Failing closed on it means one stale enum value takes every
    /// other browsing preference down with it, and there is nothing here worth
    /// that.
    pub fn load_browsing_preferences(&self) -> io::Result<BrowsingPreferences> {
        match self.read_json(BROWSING_PREFERENCES_FILE) {
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                log::warn!("discarding unreadable browsing preferences: {error}");
                self.registry.remove_value(BROWSING_PREFERENCES_FILE).map_err(|error| io::Error::other(error.to_string()))?;
                Ok(BrowsingPreferences::default())
            }
            result => Ok(result?.unwrap_or_default()),
        }
    }

    pub fn apply_browsing_preference_patch(&self, patch: BrowsingPreferencePatch) -> io::Result<BrowsingPreferences> {
        let mut preferences = self.load_browsing_preferences()?;
        patch.apply(&mut preferences).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.registry.write_value(BROWSING_PREFERENCES_FILE, &bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(preferences)
    }

    pub fn load_reader_preferences(&self) -> io::Result<ReaderPreferences> {
        self.registry
            .read_value(PREFERENCES_FILE)
            .map_err(|error| io::Error::other(error.to_string()))?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)))
            .transpose()
            .map(|preferences| preferences.unwrap_or_default())
    }

    pub fn apply_reader_preference_patches(&self, patches: Vec<ReaderPreferencePatch>) -> io::Result<ReaderPreferences> {
        let mut preferences = self.load_reader_preferences()?;
        for patch in patches {
            patch.apply(&mut preferences).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        }
        let bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.registry.write_value(PREFERENCES_FILE, &bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(preferences)
    }
}
use super::core::*;

impl AppBackend {
    pub(crate) fn default_library_save_location(&self) -> Result<Option<String>, BackendError> {
        self.state.registry.default_library_save_location().map_err(BackendError::operation)
    }
    pub(crate) fn set_default_library_save_location(&self, path: String) -> Result<String, BackendError> {
        self.state.registry.set_default_library_save_location(&path).map_err(BackendError::operation)
    }

    pub(crate) fn reader_preferences(&self) -> Result<ReaderPreferences, BackendError> {
        self.state.load_reader_preferences().map_err(BackendError::operation)
    }

    pub(crate) fn browsing_preferences(&self) -> Result<BrowsingPreferences, BackendError> {
        self.state.load_browsing_preferences().map_err(BackendError::operation)
    }

    pub(crate) fn update_browsing_preference(&self, patch: BrowsingPreferencePatch) -> Result<BrowsingPreferences, BackendError> {
        self.state.apply_browsing_preference_patch(patch).map_err(BackendError::operation)
    }

    pub(crate) fn update_reader_preferences(&self, patches: Vec<ReaderPreferencePatch>) -> Result<ReaderPreferences, BackendError> {
        self.state.apply_reader_preference_patches(patches).map_err(BackendError::operation)
    }
}
