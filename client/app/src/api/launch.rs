//! Platform startup configuration; running operations go through client handles.
use crate::app::{AppDataLocation, BackendContext};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct BackendLaunch {
    data_dir: PathBuf,
    default_library_root: PathBuf,
    discovery: Option<(String, PathBuf)>,
}

impl BackendLaunch {
    pub fn initialize(location: AppDataLocation) -> Result<Self, String> {
        let default_library_root = location.default_library_root()?;
        let data_dir = location.data_dir()?;
        Ok(Self { data_dir, default_library_root, discovery: None })
    }

    /// Location for platform-owned files such as desktop instance locks and UI sessions.
    pub fn app_data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Use this root for discovery and future libraries, registering a fallback
    /// only when none exist. Saved locations continue to take precedence.
    pub fn with_library_discovery(mut self, root: PathBuf, fallback_name: String, fallback_path: PathBuf) -> Self {
        self.default_library_root = root;
        self.discovery = Some((fallback_name, fallback_path));
        self
    }

    pub(crate) fn into_context(self) -> Result<BackendContext, String> {
        let context = BackendContext::open(self.data_dir, &self.default_library_root)?;
        if let Some((name, fallback)) = self.discovery {
            context.registry.discover(&self.default_library_root).map_err(|error| error.to_string())?;
            if context.registry.entries().map_err(|error| error.to_string())?.is_empty() && context.registry.pending_local_purges().map_err(|error| error.to_string())?.is_empty() {
                context.registry.create(&name, Some(fallback.to_string_lossy().into_owned())).map_err(|error| error.to_string())?;
            }
        }
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_startup_preserves_documents_default_and_saved_override() {
        let root = tempfile::tempdir().unwrap();
        let mut launch = BackendLaunch::initialize(AppDataLocation::SharedDesktop).unwrap();
        // Exercise desktop startup without opening the user's registry or
        // creating libraries in their Documents directory.
        launch.data_dir = root.path().join("app");
        let expected = client_platform_desktop::default_library_root().unwrap();
        let context = launch.clone().into_context().unwrap();
        assert_eq!(context.registry.default_library_save_location().unwrap(), Some(expected.to_string_lossy().into_owned()));
        let chosen = root.path().join("Chosen");
        context.registry.set_default_library_save_location(chosen.to_str().unwrap()).unwrap();
        let restarted = launch.into_context().unwrap();
        let created = restarted.registry.create("Created", None).unwrap();
        let remote = restarted.registry.attach_remote(&crate::LibraryId::new_v4(), "Remote").unwrap();
        assert_eq!(Path::new(created.storage_locator()), chosen.join("Created"));
        assert_eq!(Path::new(remote.storage_locator()), chosen.join("Remote"));
    }

    #[test]
    fn discovery_root_places_new_and_remote_libraries_visibly_without_moving_existing_libraries() {
        let root = tempfile::tempdir().unwrap();
        let location = AppDataLocation::native_path(root.path().join("private"));
        let original = BackendContext::initialize(location.clone()).unwrap();
        let existing = original.registry.create("Existing", None).unwrap();
        drop(original);
        let visible = root.path().join("Bokheim/Libraries");
        let launch = BackendLaunch::initialize(location).unwrap().with_library_discovery(visible.clone(), "Kobo".into(), visible.join("default"));
        let context = launch.clone().into_context().unwrap();
        let created = context.registry.create("Created", None).unwrap();
        let remote = context.registry.attach_remote(&crate::LibraryId::new_v4(), "Remote").unwrap();
        assert_eq!(Path::new(created.storage_locator()), visible.join("Created"));
        assert_eq!(Path::new(remote.storage_locator()), visible.join("Remote"));
        let restarted = launch.clone().into_context().unwrap();
        let entries = restarted.registry.entries().unwrap();
        assert_eq!(entries.iter().find(|entry| entry.library_id() == existing.library_id()).unwrap().storage_locator(), existing.storage_locator());
        assert_eq!(restarted.registry.attach_remote(remote.library_id(), "Remote").unwrap().storage_locator(), remote.storage_locator());

        let chosen = root.path().join("User chosen");
        context.registry.set_default_library_save_location(chosen.to_str().unwrap()).unwrap();
        let restarted = launch.into_context().unwrap();
        let next = restarted.registry.create("Next", None).unwrap();
        assert_eq!(Path::new(next.storage_locator()), chosen.join("Next"));
    }

    #[test]
    fn discovery_registers_a_fallback_once_and_preserves_it_on_restart() {
        let root = tempfile::tempdir().unwrap();
        let location = AppDataLocation::native_path(root.path().join("app"));
        let libraries = root.path().join("libraries");
        std::fs::create_dir_all(&libraries).unwrap();
        let fallback = libraries.join("default");
        let launch = BackendLaunch::initialize(location).unwrap().with_library_discovery(libraries, "Device library".into(), fallback.clone());
        assert!(!fallback.exists(), "configuration must not mutate the library registry");
        let first = launch.clone().into_context().unwrap();
        let entries = first.registry.entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].library_name(), "Device library");
        let restarted = launch.into_context().unwrap().registry.entries().unwrap();
        assert_eq!(restarted.len(), 1);
        assert_eq!(restarted[0].library_id(), entries[0].library_id());
    }

    #[test]
    fn discovery_does_not_recreate_a_library_awaiting_local_purge() {
        let root = tempfile::tempdir().unwrap();
        let location = AppDataLocation::native_path(root.path().join("app"));
        let libraries = root.path().join("libraries");
        let fallback = libraries.join("default");
        let launch = BackendLaunch::initialize(location).unwrap().with_library_discovery(libraries, "Device library".into(), fallback);
        let context = launch.clone().into_context().unwrap();
        let id = *context.registry.entries().unwrap()[0].library_id();
        context.registry.begin_local_removal(&id).unwrap();
        drop(context);

        let restarted = launch.into_context().unwrap();
        assert!(restarted.registry.entries().unwrap().is_empty());
        assert_eq!(restarted.registry.pending_local_purges().unwrap(), [id]);
    }
}
