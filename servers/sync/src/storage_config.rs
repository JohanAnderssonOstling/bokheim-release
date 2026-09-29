use std::error::Error;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::{Component, Path, PathBuf};

const ASSETS_DIRECTORY: &str = "BOKHEIM_ASSETS_DIR";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoragePaths {
    books: PathBuf,
    thumbnails: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
pub struct StorageConfigError(String);

impl StorageConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl Display for StorageConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for StorageConfigError {}

impl StoragePaths {
    pub fn from_env() -> Result<Self, StorageConfigError> {
        Self::from_values(|name| std::env::var_os(name))
    }

    pub fn into_parts(self) -> (PathBuf, PathBuf) {
        (self.books, self.thumbnails)
    }

    fn from_values(mut value: impl FnMut(&str) -> Option<OsString>) -> Result<Self, StorageConfigError> {
        let Some(root) = value(ASSETS_DIRECTORY) else {
            return Ok(Self { books: PathBuf::from("./books"), thumbnails: PathBuf::from("./thumbnails") });
        };
        let root = PathBuf::from(root);
        validate_production_path(ASSETS_DIRECTORY, &root)?;
        Ok(Self { books: root.join("books"), thumbnails: root.join("thumbnails") })
    }
}

fn validate_production_path(name: &str, path: &Path) -> Result<(), StorageConfigError> {
    if !path.is_absolute() {
        return Err(StorageConfigError::new(format!("{name} must be an absolute path")));
    }
    if path.components().any(|component| matches!(component, Component::CurDir | Component::ParentDir)) {
        return Err(StorageConfigError::new(format!("{name} must be normalized and may not contain . or .. components")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn paths(values: &[(&str, &str)]) -> Result<StoragePaths, StorageConfigError> {
        let values: HashMap<_, _> = values.iter().map(|(key, value)| ((*key).to_owned(), OsString::from(value))).collect();
        StoragePaths::from_values(|name| values.get(name).cloned())
    }

    #[test]
    fn local_development_has_isolated_relative_defaults() {
        assert_eq!(paths(&[]).unwrap().into_parts(), (PathBuf::from("./books"), PathBuf::from("./thumbnails")));
    }

    #[test]
    fn production_root_must_be_absolute_and_normalized() {
        assert!(paths(&[(ASSETS_DIRECTORY, "assets")]).is_err());
        assert!(paths(&[(ASSETS_DIRECTORY, "/srv/bokheim/../assets")]).is_err());
    }

    #[test]
    fn derives_storage_directories_from_the_production_root() {
        let configured = paths(&[(ASSETS_DIRECTORY, "/var/lib/bokheim/assets")]).unwrap();
        assert_eq!(configured.into_parts(), (PathBuf::from("/var/lib/bokheim/assets/books"), PathBuf::from("/var/lib/bokheim/assets/thumbnails")));
    }
}
