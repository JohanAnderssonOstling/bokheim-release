//! Book references on the platform clipboard, shared across browse pages and windows.

use gpui::ClipboardItem;
use serde::{Deserialize, Serialize};
use sync_common::{BookId, DirId};
use uuid::Uuid;

const PREFIX: &str = "bokheim-books:v1:";

/// A book with its own keyboard focus, rather than the outer grid's cursor.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(no_json)]
pub(super) struct FocusedBookClipboard {
    pub book: BookId,
    pub source: Option<DirId>,
    pub cut: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BookClipboard {
    pub library_id: Uuid,
    pub books: Vec<BookId>,
    pub source: Option<DirId>,
    #[serde(default)]
    pub cut: bool,
}

impl BookClipboard {
    pub(super) fn to_item(&self) -> Option<ClipboardItem> {
        let data = serde_json::to_string(self).ok()?;
        Some(ClipboardItem::new_string(format!("{PREFIX}{data}")))
    }

    pub(super) fn from_item(item: &ClipboardItem) -> Option<Self> {
        let text = item.text()?;
        let clipboard: Self = serde_json::from_str(text.strip_prefix(PREFIX)?).ok()?;
        (!clipboard.books.is_empty()).then_some(clipboard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_multiple_books_keep_their_library_and_source() {
        for count in [1, 3] {
            for source in [None, Some(Uuid::from_u128(2))] {
                for cut in [false, true] {
                    let clipboard = BookClipboard { library_id: Uuid::from_u128(1), books: (0..count).map(|index| BookId::from_bytes([index; 32])).collect(), source, cut };
                    assert_eq!(BookClipboard::from_item(&clipboard.to_item().unwrap()), Some(clipboard));
                }
            }
        }
    }

    #[test]
    fn ordinary_text_malformed_data_and_empty_selections_are_ignored() {
        for text in ["text copied from a search field", "bokheim-books:v1:{", "bokheim-books:v2:{}"] {
            assert!(BookClipboard::from_item(&ClipboardItem::new_string(text.into())).is_none());
        }
        let empty = BookClipboard { library_id: Uuid::nil(), books: Vec::new(), source: None, cut: false };
        assert!(BookClipboard::from_item(&empty.to_item().unwrap()).is_none());
    }
}
