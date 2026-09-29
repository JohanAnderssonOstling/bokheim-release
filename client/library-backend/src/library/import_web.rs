//! Adopt browser-staged bytes before entering the shared import workflow.
use super::LibrarySession;
impl LibrarySession {
    pub async fn import_staged_book(&self, parent_id: crate::DirId, file_name: String, physical: String, length: u64, hash: crate::ContentHash) -> Result<crate::ContentHash, String> {
        let validated = self.validate_import(&parent_id, &file_name).map_err(|e| e.to_string())?;
        let prefix = format!("__libraries/{}/imports/", self.id);
        if !physical.starts_with(&prefix) {
            return Err("Staged book belongs to a different library".into());
        }
        let writer = client_platform_web::web_storage::adopt_staged(physical, length).await?;
        let staged = self.assets.finish_staged_write(writer, hash.to_string()).map_err(|error| error.to_string())?;
        self.finish_import(validated, staged).await.map_err(|e| e.to_string())
    }
}
