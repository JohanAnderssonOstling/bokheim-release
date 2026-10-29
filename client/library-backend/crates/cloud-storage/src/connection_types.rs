//! Credential persistence contracts shared by enabled and disabled builds.

use crate::oauth::{GoogleAccessGrant, GoogleAccount, GoogleTokens};
use crate::{CloudError, ProviderRuntime, StorageFuture};
use serde::{Deserialize, Serialize};
use sync_common::storage::ProviderId;

pub trait GoogleAuthorization: ProviderRuntime {
    fn grant<'a>(&'a self, account: Option<&'a GoogleAccount>, interactive: bool, rejected: Option<&'a str>) -> StorageFuture<'a, GoogleAccessGrant>;
}

#[derive(Clone, Serialize, Deserialize)]
pub struct GoogleConnection {
    pub account: GoogleAccount,
    pub tokens: GoogleTokens,
}

/// Hosts persist these values in device-owned account storage. Refresh commits
/// compare the previous access token so signing out during refresh cannot
/// recreate a removed connection or overwrite a more recent explicit login.
pub trait ConnectionPersistence: ProviderRuntime {
    fn load_google(&self, account: &ProviderId) -> Result<Option<GoogleConnection>, CloudError>;
    fn save_google(&self, connection: &GoogleConnection, previous_access_token: Option<&str>) -> Result<bool, CloudError>;
}
