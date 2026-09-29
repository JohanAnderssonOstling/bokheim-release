//! Storage-independent rules for choosing library folder destinations.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::library_state::portable_name_key;

pub fn numbered_folder_name(base: &str, number: usize) -> String {
    crate::folder_names::numbered(base, number)
}

pub fn numbered_file_name(base: &str, number: usize) -> String {
    let path = Path::new(base);
    match (path.file_stem().and_then(|value| value.to_str()), path.extension().and_then(|value| value.to_str())) {
        (Some(stem), Some(extension)) => format!("{stem} {number}.{extension}"),
        _ => format!("{base} {number}"),
    }
}

fn unique_name<'a>(requested: &str, occupied: impl IntoIterator<Item = &'a str>, numbered: fn(&str, usize) -> String) -> String {
    let occupied = occupied.into_iter().map(portable_name_key).collect::<HashSet<_>>();
    if !occupied.contains(&portable_name_key(requested)) {
        return requested.to_owned();
    }
    for number in 2.. {
        let candidate = numbered(requested, number);
        if !occupied.contains(&portable_name_key(&candidate)) {
            return candidate;
        }
    }
    unreachable!("numeric name suffix space is unbounded")
}

pub fn unique_folder_name<'a>(requested: &str, occupied: impl IntoIterator<Item = &'a str>) -> String {
    unique_name(&crate::project_folder_name(requested), occupied, numbered_folder_name)
}

pub fn unique_file_name<'a>(requested: &str, occupied: impl IntoIterator<Item = &'a str>) -> String {
    unique_name(requested, occupied, numbered_file_name)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FolderDestinationPolicy {
    Book { source_id: String },
    MoveFolder { source_parent_id: String, source_path: String },
}

impl FolderDestinationPolicy {
    pub fn is_visible(&self, destination_path: &str) -> bool {
        match self {
            Self::Book { .. } => true,
            Self::MoveFolder { source_path, .. } => destination_path != source_path && !destination_path.starts_with(&format!("{source_path} / ")),
        }
    }

    pub fn can_select(&self, destination_id: &str, destination_path: &str) -> bool {
        self.is_visible(destination_path)
            && match self {
                Self::Book { source_id } => destination_id != source_id,
                Self::MoveFolder { source_parent_id, .. } => destination_id != source_parent_id,
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_policy_rejects_only_the_source_folder() {
        let policy = FolderDestinationPolicy::Book { source_id: "source".to_owned() };
        assert!(!policy.can_select("source", "Shelf"));
        assert!(policy.can_select("other", "Other"));
        assert!(policy.is_visible("Shelf"));
    }

    #[test]
    fn folder_policy_hides_its_subtree_and_rejects_its_current_parent() {
        let policy = FolderDestinationPolicy::MoveFolder { source_parent_id: "parent".to_owned(), source_path: "Shelf / Child".to_owned() };
        assert!(!policy.can_select("parent", "Shelf"));
        assert!(!policy.is_visible("Shelf / Child"));
        assert!(!policy.is_visible("Shelf / Child / Nested"));
        assert!(policy.can_select("other", "Other"));
    }

    #[test]
    fn collision_names_match_native_library_projection() {
        assert_eq!(unique_folder_name("Shelf", ["shelf", "Shelf 2"]), "Shelf 3");
        assert_eq!(unique_file_name("Book.epub", ["book.epub", "Book 2.epub"]), "Book 3.epub");
    }
}
