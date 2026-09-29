pub use library_registry::LibraryEntry;
pub(crate) use library_registry::LibraryRegistry;
use std::path::{Path, PathBuf};

const ACCOUNT_SESSION_KEY: &str = "account_session";

/// Selects where device-local application state is stored.
///
/// Desktop frontends share `Bokheim`'s conventional data directory. Sandboxed
/// platforms such as Android provide the private directory assigned by the OS.
#[derive(Clone, Debug)]
pub enum AppDataLocation {
    SharedDesktop,
    PlatformLocal(String),
}

impl AppDataLocation {
    pub(crate) fn data_dir(&self) -> Result<PathBuf, String> {
        match self {
            Self::SharedDesktop => client_platform_desktop::data_dir(),
            Self::PlatformLocal(locator) => Ok(locator.into()),
        }
    }

    pub(crate) fn default_library_root(&self) -> Result<std::path::PathBuf, String> {
        match self {
            Self::SharedDesktop => client_platform_desktop::default_library_root(),
            Self::PlatformLocal(locator) => Ok(Path::new(locator).join("libraries")),
        }
    }

    /// Converts a path at a native host boundary. Backend state retains only
    /// the resulting storage locator.
    pub fn native_path(path: impl AsRef<Path>) -> Self {
        Self::PlatformLocal(path.as_ref().to_string_lossy().into_owned())
    }
}

/// Validated startup and account-persistence capability.
///
/// Live application state owns the registry directly after startup; this value
/// exists only while opening that state and as the cloneable persistence
/// capability retained by `AccountSession`.
#[derive(Clone, Debug)]
pub(crate) struct BackendContext {
    pub(crate) registry: LibraryRegistry,
}

impl BackendContext {
    pub fn initialize(location: AppDataLocation) -> Result<Self, String> {
        Self::open(location.data_dir()?, &location.default_library_root()?)
    }

    pub(crate) fn open(app_data_dir: PathBuf, library_root: &Path) -> Result<Self, String> {
        let registry = LibraryRegistry::open_with_library_root(app_data_dir.to_string_lossy(), library_root.to_string_lossy()).map_err(|error| format!("failed to initialize library registry: {error}"))?;
        crate::asset_store::configure_book_leases(&app_data_dir).map_err(|error| error.to_string())?;
        Ok(Self { registry })
    }

    pub(super) fn read_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> std::io::Result<Option<T>> {
        self.registry.read_value(key).map_err(|error| std::io::Error::other(error.to_string()))?.map(|bytes| serde_json::from_slice(&bytes).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))).transpose()
    }

    pub(super) fn write_json(&self, key: &str, value: &impl serde::Serialize) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        self.registry.write_value(key, &bytes).map_err(|error| std::io::Error::other(error.to_string()))
    }

    pub(crate) fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
        self.read_json(ACCOUNT_SESSION_KEY)
    }

    pub(crate) fn save_account_session(&self, session: &account_client::Session) -> std::io::Result<()> {
        self.write_json(ACCOUNT_SESSION_KEY, session)
    }

    pub(crate) fn clear_account_session(&self) -> std::io::Result<()> {
        self.registry.remove_value(ACCOUNT_SESSION_KEY).map_err(|error| std::io::Error::other(error.to_string()))
    }
}

impl account_client::SessionPersistence for BackendContext {
    fn load_account_session(&self) -> std::io::Result<Option<account_client::Session>> {
        BackendContext::load_account_session(self)
    }

    fn save_account_session(&self, session: &account_client::Session) -> std::io::Result<()> {
        BackendContext::save_account_session(self, session)
    }

    fn clear_account_session(&self) -> std::io::Result<()> {
        BackendContext::clear_account_session(self)
    }
}
