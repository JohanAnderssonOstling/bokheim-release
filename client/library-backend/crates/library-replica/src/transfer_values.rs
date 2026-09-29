use std::fmt;
use sync_common::ContentHash;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RelativeBookPath(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelativeBookPathError;

impl fmt::Display for RelativeBookPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("book path must be a non-empty library-relative path without traversal")
    }
}
impl std::error::Error for RelativeBookPathError {}

impl RelativeBookPath {
    pub fn parse(value: &str) -> Result<Self, RelativeBookPathError> {
        if value.is_empty() || value.contains('\\') || value.chars().any(char::is_control) {
            return Err(RelativeBookPathError);
        }
        let relative = value.strip_prefix('/').unwrap_or(value);
        if relative.is_empty() || relative.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
            return Err(RelativeBookPathError);
        }
        Ok(Self(format!("/{relative}")))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for RelativeBookPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookPlacement {
    pub content_hash: ContentHash,
    pub rel_path: RelativeBookPath,
}
impl BookPlacement {
    pub const fn new(content_hash: ContentHash, rel_path: RelativeBookPath) -> Self {
        Self { content_hash, rel_path }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BookUploadIntent {
    pub id: i64,
    pub content_hash: ContentHash,
    pub checksum: ContentHash,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RelativeDirPath(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelativeDirPathError;

impl fmt::Display for RelativeDirPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("directory path must be a non-empty library-relative path without traversal")
    }
}
impl std::error::Error for RelativeDirPathError {}

impl RelativeDirPath {
    pub fn parse(value: &str) -> Result<Self, RelativeDirPathError> {
        if value.is_empty() || value.contains('\\') || value.chars().any(char::is_control) {
            return Err(RelativeDirPathError);
        }
        let relative = value.strip_prefix('/').unwrap_or(value);
        if relative.is_empty() || relative.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
            return Err(RelativeDirPathError);
        }
        Ok(Self(relative.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn from_components<'a>(components: impl IntoIterator<Item = &'a str>) -> Result<Self, RelativeDirPathError> {
        Self::parse(&components.into_iter().collect::<Vec<_>>().join("/"))
    }
    pub fn join_file(&self, file_name: &sync_common::FileName) -> Result<RelativeBookPath, RelativeBookPathError> {
        RelativeBookPath::parse(&format!("{}/{}", self.0, file_name.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_reject_escape_and_normalize_valid_inputs() {
        assert_eq!(RelativeBookPath::parse("Shelf/book.epub").unwrap().as_str(), "/Shelf/book.epub");
        assert!(RelativeBookPath::parse("../book.epub").is_err());
        assert!(RelativeBookPath::parse("/Shelf/../book.epub").is_err());
        assert!(RelativeBookPath::parse("").is_err());
        assert!(RelativeBookPath::parse("Shelf//book.epub").is_err());
        assert!(RelativeBookPath::parse("Shelf/./book.epub").is_err());
        assert!(RelativeBookPath::parse("Shelf/book.epub/").is_err());
        assert!(RelativeBookPath::parse("Shelf\\book.epub").is_err());

        assert_eq!(RelativeDirPath::parse("Shelf/Series").unwrap().as_str(), "Shelf/Series");
        assert_eq!(RelativeDirPath::parse("/Shelf/Series").unwrap().as_str(), "Shelf/Series");
        assert!(RelativeDirPath::parse("../Shelf").is_err());
        assert!(RelativeDirPath::parse("Shelf/../Series").is_err());
        assert!(RelativeDirPath::parse("").is_err());
    }
}
