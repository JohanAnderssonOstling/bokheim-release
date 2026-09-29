use super::AppClient;
use crate::runtime::AppCommand;

impl AppClient {
    pub async fn default_library_save_location(&self) -> Result<Option<String>, String> {
        self.request(AppCommand::DefaultLibrarySaveLocation).await
    }
    pub async fn set_default_library_save_location(&self, path: String) -> Result<String, String> {
        self.request(AppCommand::SetDefaultLibrarySaveLocation { path }).await
    }

    pub async fn update_browsing_preference(&self, patch: app_preferences::BrowsingPreferencePatch) -> Result<app_preferences::BrowsingPreferences, String> {
        self.request(AppCommand::UpdateBrowsingPreference { patch }).await
    }

    pub async fn update_reader_preferences(&self, patches: Vec<app_preferences::ReaderPreferencePatch>) -> Result<(), String> {
        self.request(AppCommand::UpdateReaderPreferences { patches }).await
    }
}
