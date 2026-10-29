//! Account-backed remote reading owned by one library session.

pub(crate) use super::{playback::DirectPlayback, LibrarySession, RemoteFile};
pub(crate) use crate::BackendError;
pub(crate) use crate::BookId;
use crate::LibraryId;
use book_access::remote_file::range_byte_count;
use book_access::{AsyncBookSource, FetchFuture, RemoteFileError};
use cloud_storage::connections::ConnectedDrive;
use std::future::Future;
use sync_common::storage::ProviderFile;

#[derive(Clone)]
pub(crate) enum RemoteBookAccess {
    Bokheim { client: reqwest::Client, account: library_runtime::account::LibraryAccount, library: LibraryId, source: RemoteFile },
    GoogleDrive { connection: ConnectedDrive, file: ProviderFile },
}

impl RemoteBookAccess {
    pub(crate) fn fetch(self) -> impl Fn(u64, usize) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> {
        move |offset, length| {
            let access = self.clone();
            Box::pin(async move { access.read(offset, length).await })
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn batch_fetch(self) -> impl Fn(Vec<(u64, usize)>) -> FetchFuture<'static, Result<Vec<u8>, RemoteFileError>> {
        move |ranges| {
            let access = self.clone();
            Box::pin(async move { access.read_ranges(ranges).await })
        }
    }

    pub(crate) fn async_source(self, length: u64, prefix: Vec<u8>) -> Result<AsyncBookSource, BackendError> {
        let fetch = self.fetch();
        #[cfg(not(target_arch = "wasm32"))]
        let source = AsyncBookSource::remote(length, prefix, fetch);
        #[cfg(target_arch = "wasm32")]
        let source = AsyncBookSource::browser(length, prefix, move |offset, length| {
            let response = fetch(offset, length);
            Box::pin(async move { response.await.map_err(|error| error.to_string()) })
        });
        source.map_err(BackendError::operation)
    }

    /// A logical range batch. Only the Bokheim transport uses multipart HTTP;
    /// external providers retain their existing revision-pinned reads.
    pub(crate) async fn read_ranges(&self, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
        match self {
            Self::Bokheim { client, account, library, source } => {
                authenticated_file_request(account, |session| {
                    let ranges = ranges.clone();
                    async move {
                        let url = sync_transport::blob_endpoint(session.server_url(), library, &source.hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
                        book_access::remote_file::fetch_ranges(client, url, source, session.token(), ranges).await
                    }
                })
                .await
            }
            Self::GoogleDrive { file, .. } => {
                let total = range_byte_count(&ranges, file.size_bytes, 64 * 1024 * 1024).ok_or("invalid cloud range bundle")?;
                let mut bytes = Vec::with_capacity(total);
                for (offset, count) in ranges {
                    bytes.extend(self.read(offset, count).await?);
                }
                Ok(bytes)
            }
        }
    }
    pub(crate) async fn read_bundle(&self, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
        match self {
            Self::Bokheim { client, account, library, source } => {
                authenticated_file_request(account, |session| {
                    let ranges = ranges.clone();
                    async move {
                        let url = sync_transport::blob_endpoint(session.server_url(), library, &source.hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
                        book_access::remote_file::fetch_bundle(client, url, source, session.token(), ranges).await
                    }
                })
                .await
            }
            Self::GoogleDrive { .. } => self.read_ranges(ranges).await,
        }
    }

    pub(crate) async fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>, RemoteFileError> {
        match self {
            Self::Bokheim { client, account, library, source } => read_remote_file(client, account, *library, source, offset, length).await,
            Self::GoogleDrive { connection, file } => connection.read_range(file, offset, length).await.map_err(cloud_read_error),
        }
    }
}

fn cloud_read_error(error: cloud_storage::CloudError) -> RemoteFileError {
    match error {
        cloud_storage::CloudError::AuthenticationRequired => RemoteFileError::Unauthorized,
        cloud_storage::CloudError::Retryable(message) => RemoteFileError::Retryable(message),
        cloud_storage::CloudError::Transport { message, .. } => RemoteFileError::Retryable(message),
        cloud_storage::CloudError::RateLimited => RemoteFileError::Retryable(error.to_string()),
        error => RemoteFileError::Failed(error.to_string()),
    }
}

pub(crate) fn range_backend_error(error: RemoteFileError) -> BackendError {
    match error {
        RemoteFileError::Unauthorized => BackendError::classified(client_runtime::Failure::SignInRequired, "reconnect the storage account before retrying"),
        RemoteFileError::Retryable(message) => BackendError::classified(client_runtime::Failure::ServiceUnavailable, message),
        error => BackendError::operation(error),
    }
}

impl LibrarySession {
    pub(crate) fn account(&self) -> Result<library_runtime::account::LibraryAccount, BackendError> {
        if self.db.storage_policy()?.external().is_some() {
            return Err(BackendError::message("this library's book bytes require its external storage connection"));
        }
        self.sync.clone().account().ok_or_else(|| BackendError::message("library account access is unavailable"))
    }

    pub(crate) fn downloaded_books_only(&self) -> Result<bool, BackendError> {
        let storage = self.db.storage_policy()?;
        if storage.external().is_some() {
            return Ok(false);
        }
        Ok(!storage.uses_bokheim_assets() || self.account()?.current().map_err(BackendError::operation)?.is_none())
    }

    pub(crate) fn remote_book_access(&self, source: &RemoteFile) -> Result<RemoteBookAccess, BackendError> {
        if let Some(root) = self.db.storage_policy()?.external() {
            let file = self.db.provider_file(&source.hash)?.ok_or_else(|| BackendError::message("cloud file reference is unavailable; refresh this library"))?;
            if file.identity.scope != root.scope || file.identity.book_id() != source.hash || file.size_bytes != source.length || source.revision.as_deref() != Some(file.revision.as_str()) {
                return Err(BackendError::message("cloud file reference does not match this library and revision"));
            }
            let connection = self.cloud.google_drive(root.scope.clone()).map_err(BackendError::operation)?;
            return Ok(RemoteBookAccess::GoogleDrive { connection, file });
        }
        Ok(RemoteBookAccess::Bokheim { client: self.sync.http_client.clone(), account: self.account()?, library: self.id, source: source.clone() })
    }

    pub(crate) async fn direct_audio_source(&self, hash: BookId) -> Result<DirectPlayback, String> {
        let account = self.account().map_err(|error| error.to_string())?;
        let library = self.id;
        crate::executor::timeout(
            std::time::Duration::from_secs(30),
            authenticated_file_request(&account, |session| async move {
                let mut endpoint = sync_transport::blob_endpoint(session.server_url(), &library, &hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
                endpoint.set_path(&format!("{}/playback", endpoint.path()));
                let response = self.sync.http_client.post(endpoint).bearer_auth(session.token()).header(reqwest::header::ACCEPT, sync_common::transport::MEDIA_TYPE).send().await.map_err(RemoteFileError::transport)?;
                if !response.status().is_success() {
                    return Err(RemoteFileError::status(response.status()));
                }
                let bytes = response.bytes().await.map_err(RemoteFileError::transport)?;
                let grant: sync_common::api::assets::PlaybackGrant = sync_common::transport::decode(&bytes, 16 * 1024).map_err(|_| RemoteFileError::Failed("Invalid playback grant".into()))?;
                let ticket = grant.path.strip_prefix("/api/playback/").ok_or("Invalid playback grant path")?;
                if ticket.len() != 64 || !ticket.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')) || grant.length == 0 || grant.expires_in_seconds == 0 {
                    return Err("Invalid playback grant".into());
                }
                let url = session.server_url().join(&grant.path).map_err(|_| RemoteFileError::Failed("Invalid playback grant path".into()))?;
                Ok(DirectPlayback { url: url.to_string(), expires_in_seconds: grant.expires_in_seconds, content_hash: grant.content_hash, length: grant.length })
            }),
        )
        .await
        .map_err(|_| "Playback authorization timed out".to_owned())?
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn read_file_range(&self, source: &RemoteFile, offset: u64, length: usize) -> Result<Vec<u8>, RemoteFileError> {
        if self.db.book_format(&source.hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?.is_none() {
            return Err("file is not in this library".into());
        }
        self.remote_book_access(source).map_err(|error| RemoteFileError::Failed(error.to_string()))?.read(offset, length).await
    }

    pub(crate) async fn read_file_ranges(&self, source: &RemoteFile, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
        if self.db.book_format(&source.hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?.is_none() {
            return Err("file is not in this library".into());
        }
        // Validate the worker command before it can reach any provider.
        range_byte_count(&ranges, source.length, 4 * 1024 * 1024).ok_or("invalid book ranges")?;
        self.remote_book_access(source).map_err(|error| RemoteFileError::Failed(error.to_string()))?.read_ranges(ranges).await
    }

    pub(crate) async fn read_file_bundle(&self, source: &RemoteFile, ranges: Vec<(u64, usize)>) -> Result<Vec<u8>, RemoteFileError> {
        if self.db.book_format(&source.hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?.is_none() {
            return Err("file is not in this library".into());
        }
        self.remote_book_access(source).map_err(|error| RemoteFileError::Failed(error.to_string()))?.read_bundle(ranges).await
    }

    pub(crate) async fn describe_remote_file(&self, hash: BookId) -> Result<(RemoteFile, Vec<u8>), RemoteFileError> {
        if self.db.storage_policy().map_err(|error| RemoteFileError::Failed(error.to_string()))?.external().is_some() {
            let file = self.db.provider_file(&hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?.ok_or("cloud file reference is unavailable; refresh this library")?;
            let source = RemoteFile { hash, length: file.size_bytes, revision: Some(file.revision.as_str().to_owned()) };
            let access = self.remote_book_access(&source).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
            let prefix = access.read(0, file.size_bytes.min(64 * 1024) as usize).await?;
            return Ok((source, prefix));
        }
        describe_remote_file(&self.sync.http_client, &self.account().map_err(|error| RemoteFileError::Failed(error.to_string()))?, self.id, hash).await
    }
}

pub(crate) fn file_auth_error(error: library_runtime::account::AccountSessionError) -> RemoteFileError {
    use library_runtime::account::AccountSessionError;
    match error {
        AccountSessionError::Authentication(account_client::AuthError::Http(error)) => match error.status() {
            Some(status) => RemoteFileError::status(status),
            None => RemoteFileError::transport(error),
        },
        error => RemoteFileError::Failed(error.to_string()),
    }
}

/// Opening and reading books is interactive and always allowed on metered
/// connections. Automatic transfer callers apply their own transfer policy.
pub(crate) async fn authenticated_file_request<T, F: Future<Output = Result<T, RemoteFileError>>>(account: &library_runtime::account::LibraryAccount, mut request: impl FnMut(account_client::Session) -> F) -> Result<T, RemoteFileError> {
    let session = account.refresh_if_needed().await.map_err(file_auth_error)?.ok_or(RemoteFileError::Unauthorized)?;
    let token = session.token().to_owned();
    match request(session).await {
        Err(RemoteFileError::Unauthorized) => {
            let refreshed = account.refresh_after_unauthorized(&token).await.map_err(file_auth_error)?.ok_or(RemoteFileError::Unauthorized)?;
            request(refreshed).await
        }
        result => result,
    }
}

pub(crate) async fn describe_remote_file(client: &reqwest::Client, account: &library_runtime::account::LibraryAccount, library: LibraryId, hash: BookId) -> Result<(RemoteFile, Vec<u8>), RemoteFileError> {
    authenticated_file_request(account, |session| async move {
        let url = sync_transport::blob_endpoint(session.server_url(), &library, &hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
        book_access::remote_file::describe(client, url, session.token(), hash).await
    })
    .await
}

pub(crate) async fn read_remote_file(client: &reqwest::Client, account: &library_runtime::account::LibraryAccount, library: LibraryId, source: &RemoteFile, offset: u64, length: usize) -> Result<Vec<u8>, RemoteFileError> {
    authenticated_file_request(account, |session| async move {
        let url = sync_transport::blob_endpoint(session.server_url(), &library, &source.hash).map_err(|error| RemoteFileError::Failed(error.to_string()))?;
        book_access::remote_file::fetch_range(client, url, source, session.token(), offset, length).await
    })
    .await
}
