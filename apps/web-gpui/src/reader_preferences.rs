//! Local reader display preferences. Backend patches remain the only sync path.

use reader_ui::ReaderPreferences;

const STORAGE_KEY: &str = "bokheim.reader-preferences.v1";

fn storage() -> Result<web_sys::Storage, String> {
    web_sys::window().ok_or("browser window is unavailable")?.local_storage().map_err(|error| format!("{error:?}"))?.ok_or_else(|| "local storage is unavailable".into())
}

pub(super) fn load() -> ReaderPreferences {
    let load = || -> Result<Option<ReaderPreferences>, String> { storage()?.get_item(STORAGE_KEY).map_err(|error| format!("{error:?}"))?.map(|value| serde_json::from_str(&value).map_err(|error| error.to_string())).transpose() };
    match load() {
        Ok(preferences) => preferences.unwrap_or_default(),
        Err(error) => {
            log::warn!("could not restore local reader preferences: {error}");
            ReaderPreferences::default()
        }
    }
}

pub(super) fn save(preferences: &ReaderPreferences) {
    let save = || -> Result<(), String> {
        let value = serde_json::to_string(preferences).map_err(|error| error.to_string())?;
        storage()?.set_item(STORAGE_KEY, &value).map_err(|error| format!("{error:?}"))
    };
    if let Err(error) = save() {
        log::warn!("could not save local reader preferences: {error}");
    }
}
