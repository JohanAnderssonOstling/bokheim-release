//! Google Drive v3 file storage. Authorization is supplied by the device's
//! connection owner, independently of the Bokheim metadata sync session.

use crate::REQUEST_TIMEOUT;
use crate::{ChangesPage, CloudBookFile, CloudChange, CloudEntry, CloudError, CloudFolder, FolderPage, MAX_RANGE_BYTES, bounded_bytes, metadata};
use crate::{CloudDirectoryIdentity, CloudStorage, StorageFuture};
use reqwest::{Client, StatusCode, Url, header};
use serde::Deserialize;
use sync_common::storage::{CloudProvider, ProviderFile, ProviderFileIdentity, ProviderId, ProviderScope};

const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const FILE_FIELDS: &str = "id,name,mimeType,parents,size,version,headRevisionId,trashed,appProperties,capabilities(canDownload)";

#[derive(Clone)]
pub struct GoogleDrive {
    client: Client,
    api: Url,
    uploads: Url,
    scope: ProviderScope,
}

pub use crate::google_drive_types::{DriveUpload, UploadProgress};

impl GoogleDrive {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn for_test(scope: ProviderScope, base: Url) -> Result<Self, CloudError> {
        if base.scheme() != "http" || base.host_str() != Some("127.0.0.1") || !base.username().is_empty() || base.password().is_some() || base.query().is_some() || base.fragment().is_some() {
            return Err(CloudError::InvalidRequest("test Drive endpoint must be a local HTTP server".into()));
        }
        let mut drive = Self::new(scope)?;
        drive.api = base.join("drive/v3/").map_err(|_| CloudError::InvalidRequest("invalid test endpoint".into()))?;
        drive.uploads = base.join("upload/drive/v3/").map_err(|_| CloudError::InvalidRequest("invalid test endpoint".into()))?;
        Ok(drive)
    }

    pub fn new(scope: ProviderScope) -> Result<Self, CloudError> {
        if scope.provider != CloudProvider::GoogleDrive {
            return Err(CloudError::InvalidRequest("Expected a Google Drive account".into()));
        }
        let builder = Client::builder();
        #[cfg(not(target_arch = "wasm32"))]
        let builder = builder.redirect(reqwest::redirect::Policy::none());
        Ok(Self {
            client: builder.build().map_err(CloudError::transport)?,
            api: Url::parse("https://www.googleapis.com/drive/v3/").expect("static Google Drive URL"),
            uploads: Url::parse("https://www.googleapis.com/upload/drive/v3/").expect("static Google Drive upload URL"),
            scope,
        })
    }

    fn endpoint(&self, segments: &[&str]) -> Url {
        let mut url = self.api.clone();
        url.path_segments_mut().expect("HTTP base URL").pop_if_empty().extend(segments);
        url
    }

    pub fn scope(&self) -> &ProviderScope {
        &self.scope
    }

    /// Used both by the folder picker and by the recursive library scanner.
    pub async fn list_folder(&self, token: &str, folder: &ProviderId, page: Option<&str>, folders_only: bool) -> Result<FolderPage, CloudError> {
        let parent = folder.as_str().replace('\\', "\\\\").replace('\'', "\\'");
        let mut query = format!("'{parent}' in parents and trashed = false");
        if folders_only {
            query.push_str(&format!(" and mimeType = '{FOLDER_MIME}'"));
        }
        let mut request = self.client.get(self.endpoint(&["files"])).bearer_auth(token).query(&[
            ("q", query),
            ("fields", format!("nextPageToken,incompleteSearch,files({FILE_FIELDS})")),
            ("pageSize", "1000".into()),
            ("supportsAllDrives", "true".into()),
            ("includeItemsFromAllDrives", "true".into()),
        ]);
        if let Some(page) = page {
            request = request.query(&[("pageToken", page)]);
        }
        let result: FileList = metadata(request.timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?).await?;
        if result.incomplete_search {
            return Err(CloudError::Retryable("Google Drive returned an incomplete folder listing".into()));
        }
        let mut entries = Vec::new();
        for file in result.files {
            if let Some(entry) = file.entry(&self.scope)? {
                entries.push(entry);
            }
        }
        Ok(FolderPage { entries, next_page: result.next_page_token })
    }

    pub async fn describe(&self, token: &str, id: &ProviderId) -> Result<CloudEntry, CloudError> {
        let response = self.client.get(self.endpoint(&["files", id.as_str()])).bearer_auth(token).query(&[("fields", FILE_FIELDS), ("supportsAllDrives", "true")]).timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?;
        let file: DriveFile = metadata(response).await?;
        file.entry(&self.scope)?.ok_or(CloudError::NotFound)
    }

    /// Drive's metadata version is separate from the file identity. Check it
    /// on both sides of each range so cached chunks cannot mix replacements.
    pub async fn read_range(&self, token: &str, file: &ProviderFile, offset: u64, length: usize) -> Result<Vec<u8>, CloudError> {
        if file.identity.scope != self.scope || length == 0 || length > MAX_RANGE_BYTES || offset.checked_add(length as u64).is_none_or(|end| end > file.size_bytes) {
            return Err(CloudError::InvalidRequest("Invalid cloud file range or account".into()));
        }
        self.check_revision(token, file).await?;
        let end = offset + length as u64 - 1;
        let response = self
            .client
            .get(self.endpoint(&["files", file.identity.file_id.as_str()]))
            .bearer_auth(token)
            .query(&[("alt", "media"), ("supportsAllDrives", "true")])
            .header(header::RANGE, format!("bytes={offset}-{end}"))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(CloudError::transport)?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            // A server ignoring Range must not cause a full-book download.
            return Err(if response.status().is_success() { CloudError::status(response.status()) } else { CloudError::response(response).await });
        }
        let expected = format!("bytes {offset}-{end}/{}", file.size_bytes);
        if response.headers().get(header::CONTENT_RANGE).and_then(|header| header.to_str().ok()) != Some(expected.as_str()) {
            return Err(CloudError::InvalidResponse("Cloud file range bounds changed".into()));
        }
        let bytes = bounded_bytes(response, length).await?;
        if bytes.len() != length {
            return Err(CloudError::Retryable("Incomplete cloud file range".into()));
        }
        self.check_revision(token, file).await?;
        Ok(bytes)
    }

    async fn check_revision(&self, token: &str, file: &ProviderFile) -> Result<(), CloudError> {
        match self.describe(token, &file.identity.file_id).await? {
            CloudEntry::File(current) if current.file.revision == file.revision && current.file.size_bytes == file.size_bytes => Ok(()),
            _ => Err(CloudError::RevisionChanged),
        }
    }

    /// Capture before starting a full listing, then replay changes from this
    /// checkpoint after enumeration to account for concurrent moves/uploads.
    pub async fn start_checkpoint(&self, token: &str) -> Result<String, CloudError> {
        let response = self.client.get(self.endpoint(&["changes", "startPageToken"])).bearer_auth(token).query(&[("supportsAllDrives", "true")]).timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Checkpoint {
            start_page_token: String,
        }
        let result: Checkpoint = metadata(response).await?;
        if result.start_page_token.is_empty() {
            return Err(CloudError::InvalidResponse("Missing Drive scan checkpoint".into()));
        }
        Ok(result.start_page_token)
    }

    /// Drive changes are account-wide. The scanner applies them to its tracked
    /// folder tree and follows newly discovered directories before committing.
    pub async fn changes(&self, token: &str, checkpoint: &str) -> Result<ChangesPage, CloudError> {
        let response = self
            .client
            .get(self.endpoint(&["changes"]))
            .bearer_auth(token)
            .query(&[("pageToken", checkpoint), ("pageSize", "1000"), ("supportsAllDrives", "true"), ("includeItemsFromAllDrives", "true")])
            .query(&[("fields", format!("nextPageToken,newStartPageToken,changes(fileId,removed,file({FILE_FIELDS}))"))])
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(CloudError::transport)?;
        let result: DriveChanges = metadata(response).await?;
        if result.next_page_token.is_none() && result.new_start_page_token.is_none() {
            return Err(CloudError::InvalidResponse("Missing Drive changes checkpoint".into()));
        }
        let mut changes = Vec::new();
        for change in result.changes {
            if change.removed {
                changes.push(CloudChange::Removed(change.file_id));
            } else if let Some(file) = change.file {
                let id = file.id.clone();
                match file.entry(&self.scope)? {
                    Some(entry) => changes.push(CloudChange::Updated(entry)),
                    None => changes.push(CloudChange::Removed(id)),
                }
            } else {
                return Err(CloudError::InvalidResponse("Missing Drive change file metadata".into()));
            }
        }
        Ok(ChangesPage { changes, next_page: result.next_page_token, checkpoint: result.new_start_page_token })
    }

    pub async fn create_folder(&self, token: &str, parent: &ProviderId, name: &str) -> Result<CloudFolder, CloudError> {
        self.create_library_folder(token, parent, name, None).await
    }

    pub async fn create_library_folder(&self, token: &str, parent: &ProviderId, name: &str, identity: Option<&CloudDirectoryIdentity>) -> Result<CloudFolder, CloudError> {
        validate_name(name)?;
        let mut folder_metadata = serde_json::json!({"name": name, "mimeType": FOLDER_MIME, "parents": [parent.as_str()]});
        if let Some(identity) = identity {
            folder_metadata["appProperties"] = serde_json::json!({"bokheimLibrary": identity.library_id.to_string(), "bokheimDirectory": identity.directory_id.to_string()});
        }
        let response =
            self.client.post(self.endpoint(&["files"])).bearer_auth(token).query(&[("fields", FILE_FIELDS), ("supportsAllDrives", "true")]).json(&folder_metadata).timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?;
        let file: DriveFile = metadata(response).await?;
        match file.entry(&self.scope)? {
            Some(CloudEntry::Folder(folder)) => Ok(folder),
            _ => Err(CloudError::InvalidResponse("Drive did not create a folder".into())),
        }
    }

    /// Generate a stable ID before creating an upload. Retrying creation with
    /// the same ID cannot accidentally create a second book after a crash.
    pub async fn allocate_file_id(&self, token: &str) -> Result<ProviderId, CloudError> {
        #[derive(Deserialize)]
        struct Ids {
            ids: Vec<ProviderId>,
        }
        let response = self.client.get(self.endpoint(&["files", "generateIds"])).bearer_auth(token).query(&[("count", "1"), ("space", "drive"), ("type", "files")]).timeout(REQUEST_TIMEOUT).send().await.map_err(CloudError::transport)?;
        let mut result: Ids = metadata(response).await?;
        if result.ids.len() != 1 {
            return Err(CloudError::InvalidResponse("Drive did not allocate one file ID".into()));
        }
        Ok(result.ids.remove(0))
    }

    pub async fn begin_upload(&self, token: &str, parent: &ProviderId, file_id: &ProviderId, name: &str, size_bytes: u64) -> Result<DriveUpload, CloudError> {
        validate_name(name)?;
        if size_bytes == 0 || size_bytes > 9_007_199_254_740_991 {
            return Err(CloudError::InvalidRequest("Invalid upload size".into()));
        }
        let url = self.uploads.join("files").expect("static upload path");
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .query(&[("uploadType", "resumable"), ("fields", FILE_FIELDS), ("supportsAllDrives", "true")])
            .header("X-Upload-Content-Type", "application/octet-stream")
            .header("X-Upload-Content-Length", size_bytes)
            .json(&serde_json::json!({"id": file_id.as_str(), "name": name, "parents": [parent.as_str()]}))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(CloudError::transport)?;
        if !response.status().is_success() {
            return Err(CloudError::response(response).await);
        }
        let url = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Url::parse(value).ok())
            .filter(|url| trusted_upload_url(url, &self.uploads))
            .ok_or_else(|| CloudError::InvalidResponse("Invalid Drive upload session URL".into()))?;
        Ok(DriveUpload { url, scope: self.scope.clone(), file_id: file_id.clone(), size_bytes })
    }

    pub async fn upload_chunk(&self, token: &str, upload: &DriveUpload, offset: u64, bytes: Vec<u8>) -> Result<UploadProgress, CloudError> {
        self.validate_upload(upload)?;
        let end = offset.checked_add(bytes.len() as u64).filter(|&end| end <= upload.size_bytes).ok_or_else(|| CloudError::InvalidRequest("Upload chunk exceeds file size".into()))?;
        if bytes.is_empty() || bytes.len() > MAX_RANGE_BYTES || (end != upload.size_bytes && bytes.len() % (256 * 1024) != 0) {
            return Err(CloudError::InvalidRequest("Invalid Drive upload chunk size".into()));
        }
        let response = self
            .client
            .put(upload.url.clone())
            .bearer_auth(token)
            .header(header::CONTENT_RANGE, format!("bytes {offset}-{}/{}", end - 1, upload.size_bytes))
            .body(bytes)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(CloudError::transport)?;
        self.upload_progress(upload, response).await
    }

    /// Probe after an interrupted request; only the provider's acknowledged
    /// offset determines which bytes may be skipped on retry.
    pub async fn upload_status(&self, token: &str, upload: &DriveUpload) -> Result<UploadProgress, CloudError> {
        self.validate_upload(upload)?;
        let response = self
            .client
            .put(upload.url.clone())
            .bearer_auth(token)
            .header(header::CONTENT_RANGE, format!("bytes */{}", upload.size_bytes))
            .header(header::CONTENT_LENGTH, 0)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(CloudError::transport)?;
        self.upload_progress(upload, response).await
    }

    fn validate_upload(&self, upload: &DriveUpload) -> Result<(), CloudError> {
        if upload.scope != self.scope || !trusted_upload_url(&upload.url, &self.uploads) || upload.size_bytes == 0 {
            return Err(CloudError::InvalidRequest("Upload session belongs to another account or endpoint".into()));
        }
        Ok(())
    }

    async fn upload_progress(&self, upload: &DriveUpload, response: reqwest::Response) -> Result<UploadProgress, CloudError> {
        if response.status() == StatusCode::PERMANENT_REDIRECT {
            let confirmed_bytes = match response.headers().get(header::RANGE) {
                None => 0,
                Some(value) => value
                    .to_str()
                    .ok()
                    .and_then(|range| range.strip_prefix("bytes=0-"))
                    .and_then(|end| end.parse::<u64>().ok())
                    .and_then(|end| end.checked_add(1))
                    .ok_or_else(|| CloudError::InvalidResponse("Invalid Drive upload acknowledgement".into()))?,
            };
            if confirmed_bytes >= upload.size_bytes {
                return Err(CloudError::InvalidResponse("Drive acknowledged bytes without committing the file".into()));
            }
            return Ok(UploadProgress::Incomplete { confirmed_bytes });
        }
        let file: DriveFile = metadata(response).await?;
        match file.entry(&self.scope)? {
            Some(CloudEntry::File(file)) if file.file.identity.file_id == upload.file_id && file.file.size_bytes == upload.size_bytes => Ok(UploadProgress::Complete(file.file)),
            _ => Err(CloudError::InvalidResponse("Drive upload completion does not match the file".into())),
        }
    }
}

fn trusted_upload_url(url: &Url, base: &Url) -> bool {
    url.origin() == base.origin() && url.username().is_empty() && url.password().is_none() && url.fragment().is_none() && url.path().starts_with(base.path())
}

impl CloudStorage for GoogleDrive {
    fn list_folder<'a>(&'a self, token: &'a str, folder: &'a ProviderId, page: Option<&'a str>, folders_only: bool) -> StorageFuture<'a, FolderPage> {
        Box::pin(GoogleDrive::list_folder(self, token, folder, page, folders_only))
    }
    fn describe<'a>(&'a self, token: &'a str, id: &'a ProviderId) -> StorageFuture<'a, CloudEntry> {
        Box::pin(GoogleDrive::describe(self, token, id))
    }
    fn read_range<'a>(&'a self, token: &'a str, file: &'a ProviderFile, offset: u64, length: usize) -> StorageFuture<'a, Vec<u8>> {
        Box::pin(GoogleDrive::read_range(self, token, file, offset, length))
    }
    fn start_checkpoint<'a>(&'a self, token: &'a str) -> StorageFuture<'a, String> {
        Box::pin(GoogleDrive::start_checkpoint(self, token))
    }
    fn changes<'a>(&'a self, token: &'a str, checkpoint: &'a str) -> StorageFuture<'a, ChangesPage> {
        Box::pin(GoogleDrive::changes(self, token, checkpoint))
    }
    fn create_folder<'a>(&'a self, token: &'a str, parent: &'a ProviderId, name: &'a str) -> StorageFuture<'a, CloudFolder> {
        Box::pin(GoogleDrive::create_folder(self, token, parent, name))
    }
}

fn validate_name(name: &str) -> Result<(), CloudError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(CloudError::InvalidRequest("Enter a folder or file name".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveFile {
    id: ProviderId,
    name: String,
    mime_type: String,
    #[serde(default)]
    app_properties: std::collections::HashMap<String, String>,
    #[serde(default)]
    parents: Vec<ProviderId>,
    size: Option<String>,
    version: Option<ProviderId>,
    head_revision_id: Option<ProviderId>,
    #[serde(default)]
    trashed: bool,
    capabilities: Option<Capabilities>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    can_download: Option<bool>,
}

impl DriveFile {
    fn entry(self, scope: &ProviderScope) -> Result<Option<CloudEntry>, CloudError> {
        if self.trashed {
            return Ok(None);
        }
        if self.mime_type == FOLDER_MIME {
            let directory_hint = self
                .app_properties
                .get("bokheimLibrary")
                .and_then(|library| library.parse().ok())
                .zip(self.app_properties.get("bokheimDirectory").and_then(|directory| directory.parse().ok()))
                .map(|(library_id, directory_id)| CloudDirectoryIdentity { library_id, directory_id });
            return Ok(Some(CloudEntry::Folder(CloudFolder { id: self.id, name: self.name, parent_ids: self.parents, directory_hint })));
        }
        // Workspace documents and shortcuts have no raw book bytes.
        if self.mime_type.starts_with("application/vnd.google-apps.") {
            return Ok(None);
        }
        if self.capabilities.is_some_and(|capabilities| capabilities.can_download == Some(false)) {
            return Ok(None);
        }
        let size_bytes = self.size.as_deref().and_then(|size| size.parse::<u64>().ok()).filter(|&size| size <= 9_007_199_254_740_991).ok_or_else(|| CloudError::InvalidResponse("Invalid Drive file size".into()))?;
        if size_bytes == 0 {
            return Ok(None);
        }
        // Drive's version includes metadata and background server changes.
        // Binary content revisions remain stable when thumbnails are generated
        // or a file is renamed, so those changes must not invalidate book bytes.
        let revision = match self.head_revision_id {
            Some(head) => ProviderId::new(format!("head:{}", head.as_str())).map_err(|_| CloudError::InvalidResponse("Invalid Drive content revision".into()))?,
            None => self.version.ok_or_else(|| CloudError::InvalidResponse("Missing Drive file revision".into()))?,
        };
        Ok(Some(CloudEntry::File(CloudBookFile {
            file: ProviderFile { identity: ProviderFileIdentity { scope: scope.clone(), file_id: self.id }, revision, size_bytes, content_hash: None },
            name: self.name,
            parent_ids: self.parents,
            mime_type: self.mime_type,
        })))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    #[serde(default)]
    files: Vec<DriveFile>,
    next_page_token: Option<String>,
    #[serde(default)]
    incomplete_search: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveChanges {
    #[serde(default)]
    changes: Vec<DriveChange>,
    next_page_token: Option<String>,
    new_start_page_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveChange {
    file_id: ProviderId,
    #[serde(default)]
    removed: bool,
    file: Option<DriveFile>,
}

#[cfg(test)]
mod tests;
