//! Complete folder lookup shared by imports and derived-asset uploads.

use crate::{CloudEntry, CloudError, CloudFolder, connections::ConnectedDrive};
use sync_common::storage::ProviderId;

impl ConnectedDrive {
    /// Search every page and choose the smallest matching ID under this parent.
    pub async fn find_folder(&self, parent: &ProviderId, matches: impl Fn(&CloudFolder) -> bool) -> Result<Option<CloudFolder>, CloudError> {
        let mut page = None;
        let mut found: Option<CloudFolder> = None;
        loop {
            let listing = self.list_folder(parent, page.as_deref(), true).await?;
            for entry in listing.entries {
                if let CloudEntry::Folder(folder) = entry {
                    if folder.parent_ids.contains(parent) && matches(&folder) && found.as_ref().is_none_or(|previous| folder.id.as_str() < previous.id.as_str()) {
                        found = Some(folder);
                    }
                }
            }
            page = listing.next_page;
            if page.is_none() {
                return Ok(found);
            }
        }
    }
}
