//! Opt-in live checks. Uses only a newly created Drive folder, then trashes it.
//! cargo run --release -p cloud-storage --example google_drive_live -- CLIENT_JSON STATE_DIR TEST_EMAIL

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(native::run())
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use cloud_storage::connections::{CloudConnections, ConnectedDrive, ConnectionPersistence, GoogleConnection};
    use cloud_storage::google_drive::{DriveUpload, UploadProgress};
    use cloud_storage::native_login::NativeGoogleLogin;
    use cloud_storage::oauth::{GoogleOAuth, GoogleOAuthConfig};
    use cloud_storage::scanner::ScanCursor;
    use cloud_storage::{CloudChange, CloudEntry, CloudError};
    use reqwest::Client;
    use serde_json::{Value, json};
    use std::error::Error;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use sync_common::storage::{CloudProvider, ProviderFile, ProviderId, ProviderScope};

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    fn ensure(condition: bool, message: &str) -> Result<()> {
        if condition { Ok(()) } else { Err(io::Error::other(message).into()) }
    }

    fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let temporary = path.with_extension("pending");
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(temporary, path)
    }

    struct Saved {
        path: PathBuf,
        connection: Mutex<Option<GoogleConnection>>,
    }

    impl Saved {
        fn current(&self) -> Result<GoogleConnection> {
            self.connection.lock().unwrap().clone().ok_or_else(|| CloudError::AuthenticationRequired.into())
        }
    }

    impl ConnectionPersistence for Saved {
        fn load_google(&self, account: &ProviderId) -> std::result::Result<Option<GoogleConnection>, CloudError> {
            Ok(self.connection.lock().unwrap().clone().filter(|saved| &saved.account.id == account))
        }

        fn save_google(&self, connection: &GoogleConnection, previous: Option<&str>) -> std::result::Result<bool, CloudError> {
            let mut saved = self.connection.lock().unwrap();
            if previous.is_some_and(|token| !saved.as_ref().is_some_and(|saved| saved.tokens.access_token == token)) {
                return Ok(false);
            }
            let bytes = serde_json::to_vec(connection).map_err(|_| CloudError::Retryable("Could not serialize test connection".into()))?;
            private_write(&self.path, &bytes).map_err(|_| CloudError::Retryable("Could not save test connection".into()))?;
            *saved = Some(connection.clone());
            Ok(true)
        }
    }

    fn pass(results: &mut Vec<String>, name: &str) {
        println!("PASS: {name}");
        results.push(name.into());
    }

    async fn upload(drive: &ConnectedDrive, folder: &ProviderId, name: &str, bytes: &[u8], state: &Path) -> Result<ProviderFile> {
        let id = drive.allocate_file_id().await?;
        let upload = drive.begin_upload(folder, &id, name, bytes.len() as u64).await?;
        ensure(upload.matches_file(drive.scope(), &id, bytes.len() as u64), "Upload lost its reserved identity")?;
        private_write(&state.join("upload.json"), &serde_json::to_vec(&upload)?)?;
        let upload: DriveUpload = serde_json::from_slice(&fs::read(state.join("upload.json"))?)?;
        let mut offset = 0;
        if bytes.len() > 256 * 1024 {
            match drive.upload_chunk(&upload, 0, bytes[..256 * 1024].to_vec()).await? {
                UploadProgress::Incomplete { confirmed_bytes } => ensure(confirmed_bytes == 256 * 1024, "Incorrect upload acknowledgment")?,
                UploadProgress::Complete(_) => return Err(io::Error::other("Upload completed before all bytes were sent").into()),
            }
            match drive.upload_status(&upload).await? {
                UploadProgress::Incomplete { confirmed_bytes } => offset = confirmed_bytes as usize,
                UploadProgress::Complete(_) => return Err(io::Error::other("Upload status reported premature completion").into()),
            }
        }
        let file = match drive.upload_chunk(&upload, offset as u64, bytes[offset..].to_vec()).await? {
            UploadProgress::Complete(file) => file,
            UploadProgress::Incomplete { .. } => return Err(io::Error::other("Final upload was not committed").into()),
        };
        ensure(file.identity.file_id == id && file.size_bytes == bytes.len() as u64, "Committed upload identity or size mismatch")?;
        private_write(&state.join(format!("uploaded-{name}.json")), &serde_json::to_vec(&file)?)?;
        fs::remove_file(state.join("upload.json"))?;
        Ok(file)
    }

    async fn mutate(saved: &Saved, id: &ProviderId, body: Value) -> Result<()> {
        let connection = saved.current()?;
        Client::new().patch(format!("https://www.googleapis.com/drive/v3/files/{}", id.as_str())).bearer_auth(&connection.tokens.access_token).json(&body).timeout(Duration::from_secs(30)).send().await?.error_for_status()?;
        Ok(())
    }

    async fn behavior(drive: &ConnectedDrive, saved: &Saved, folder: &ProviderId, state: &Path, checkpoint: &str, results: &mut Vec<String>) -> Result<()> {
        let nested = drive.create_folder(folder, "Books").await?;
        let internal = drive.create_folder(folder, ".bokheim").await?;
        let folders = drive.list_folder(folder, None, true).await?;
        ensure(folders.entries.len() == 2 && folders.entries.iter().all(|entry| matches!(entry, CloudEntry::Folder(_))), "Folder picker listing was incorrect")?;
        pass(results, "folder creation and folder-only listing");

        let book_bytes = fs::read(state.join("fixtures/book.epub"))?;
        let book = upload(drive, &nested.id, "Book.epub", &book_bytes, state).await?;
        pass(results, "reserved file ID, persisted upload session, acknowledged resume and committed EPUB upload");
        let audio_bytes = fs::read(state.join("fixtures/audio.m4b"))?;
        let audio = upload(drive, folder, "Audiobook.m4b", &audio_bytes, state).await?;
        pass(results, "committed audiobook upload");
        let cover_bytes = fs::read(state.join("fixtures/cover.png"))?;
        let cover = upload(drive, &internal.id, "cover.png", &cover_bytes, state).await?;
        ensure(drive.read_range(&cover, 0, cover_bytes.len()).await? == cover_bytes, "Cover bytes changed")?;
        pass(results, "audiobook upload and provider-backed cover round trip");

        for offset in [0, audio_bytes.len() / 2, audio_bytes.len() - 1] {
            let length = 1024.min(audio_bytes.len() - offset);
            ensure(drive.read_range(&audio, offset as u64, length).await? == audio_bytes[offset..offset + length], "Audiobook seek returned incorrect bytes")?;
        }
        pass(results, "audiobook byte-range reads at start, middle and end");
        let mut local = Vec::new();
        while local.len() < book_bytes.len() {
            let length = (64 * 1024).min(book_bytes.len() - local.len());
            local.extend(drive.read_range(&book, local.len() as u64, length).await?);
        }
        ensure(local == book_bytes, "Downloaded EPUB differs from uploaded source")?;
        private_write(&state.join("offline-book.epub"), &local)?;
        ensure(matches!(drive.describe(&book.identity.file_id).await?, CloudEntry::File(current) if current.file == book), "Downloading modified the Drive book")?;
        pass(results, "complete local download preserves book bytes and provider revision");

        let mut cursor = Some(ScanCursor::new(folder.clone()));
        let mut files = Vec::new();
        while let Some(current) = cursor {
            let page = drive.scan_page(&current).await?;
            files.extend(page.entries.into_iter().filter_map(|entry| match entry {
                CloudEntry::File(file) => Some(file.file.identity.file_id),
                _ => None,
            }));
            cursor = page.next;
        }
        ensure(files.contains(&book.identity.file_id) && files.contains(&audio.identity.file_id) && !files.contains(&cover.identity.file_id), "Recursive scan missed books or exposed internal covers")?;
        pass(results, "recursive scan discovers nested books and excludes internal cover storage");

        mutate(saved, &book.identity.file_id, json!({"name":"Renamed.epub"})).await?;
        let renamed = match drive.describe(&book.identity.file_id).await? {
            CloudEntry::File(file) => file,
            _ => return Err(io::Error::other("Renamed book became a folder").into()),
        };
        ensure(renamed.name == "Renamed.epub" && renamed.file == book, "Rename changed book identity or content revision")?;
        ensure(drive.read_range(&book, 0, 32).await? == book_bytes[..32], "Rename invalidated unchanged book bytes")?;
        pass(results, "rename preserves file identity and cached content revision");
        let mut replacement = book_bytes.clone();
        replacement[book_bytes.len() - 1] ^= 1;
        let connection = saved.current()?;
        Client::new()
            .patch(format!("https://www.googleapis.com/upload/drive/v3/files/{}", book.identity.file_id.as_str()))
            .query(&[("uploadType", "media")])
            .bearer_auth(&connection.tokens.access_token)
            .header("Content-Type", "application/octet-stream")
            .body(replacement.clone())
            .timeout(Duration::from_secs(30))
            .send()
            .await?
            .error_for_status()?;
        ensure(matches!(drive.read_range(&book, 0, 1).await, Err(CloudError::RevisionChanged)), "Stale book revision was accepted")?;
        let current = match drive.describe(&book.identity.file_id).await? {
            CloudEntry::File(file) => file.file,
            _ => return Err(io::Error::other("Replaced book became a folder").into()),
        };
        ensure(drive.read_range(&current, (replacement.len() - 1) as u64, 1).await? == replacement[replacement.len() - 1..], "Refreshed book revision is unreadable")?;
        pass(results, "same-size content replacement rejects stale reads and exposes the new revision");

        let mut page = checkpoint.to_owned();
        let mut seen = Vec::new();
        loop {
            let changes = drive.changes(&page).await?;
            seen.extend(changes.changes.into_iter().filter_map(|change| match change {
                CloudChange::Updated(CloudEntry::File(file)) => Some(file.file.identity.file_id),
                _ => None,
            }));
            match changes.next_page {
                Some(next) => page = next,
                None => break,
            }
        }
        ensure(seen.contains(&book.identity.file_id) && seen.contains(&audio.identity.file_id), "Changes feed missed uploaded books")?;
        pass(results, "incremental changes feed includes committed books");

        let mut expired = saved.current()?;
        ensure(expired.tokens.refresh_token.is_some(), "Google did not issue an offline refresh token")?;
        expired.tokens.expires_at = 0;
        saved.save_google(&expired, None)?;
        drive.describe(folder).await?;
        ensure(saved.current()?.tokens.expires_at > SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 60, "Expired access token was not renewed")?;
        pass(results, "expired access token renews silently using the refresh token");
        Ok(())
    }

    pub async fn run() -> Result<()> {
        let mut args = std::env::args_os().skip(1);
        let credentials = PathBuf::from(args.next().ok_or("Provide the Desktop client JSON path")?);
        let state = PathBuf::from(args.next().ok_or("Provide a private test-state directory")?);
        let expected_email = args.next().ok_or("Provide the expected testing account email")?.into_string().map_err(|_| "Invalid testing account email")?;
        fs::create_dir_all(&state)?;
        #[cfg(unix)]
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let credentials: Value = serde_json::from_slice(&fs::read(credentials)?)?;
        let installed = credentials.get("installed").ok_or("Expected a Desktop client JSON")?;
        let oauth = GoogleOAuth::new(GoogleOAuthConfig { client_id: installed["client_id"].as_str().ok_or("Missing client ID")?.into(), client_secret: Some(installed["client_secret"].as_str().ok_or("Missing client secret")?.into()) })?;
        let path = state.join("connection.json");
        let connection = if path.exists() { Some(serde_json::from_slice(&fs::read(&path)?)?) } else { None };
        let saved = Arc::new(Saved { path, connection: Mutex::new(connection) });
        let connections = CloudConnections::new(oauth.clone(), saved.clone());
        if saved.current().is_err() {
            let login = NativeGoogleLogin::begin(connections.clone()).await?;
            let mut url = login.authorization_url();
            url.query_pairs_mut().append_pair("login_hint", &expected_email);
            println!("WAITING: Complete Google sign-in and consent using your configured testing account.");
            Command::new("xdg-open").arg(url.as_str()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
            login.finish().await?;
        }
        ensure(saved.current()?.account.email.eq_ignore_ascii_case(&expected_email), "Google returned a different account; no Drive tests were performed")?;
        let scope = ProviderScope { provider: CloudProvider::GoogleDrive, account_id: saved.current()?.account.id, container_id: ProviderId::new("my_drive")? };
        let drive = connections.google_drive(scope.clone())?;
        let mut results = Vec::new();
        let root = ProviderId::new("root")?;
        drive.list_folder(&root, None, true).await?;
        pass(&mut results, "real Google account authorization and Drive folder discovery");
        let checkpoint = drive.start_checkpoint().await?;
        let name = format!("Bokheim live test {}", SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos());
        let folder = drive.create_folder(&root, &name).await?;
        private_write(&state.join("test-folder.json"), &serde_json::to_vec(&folder)?)?;
        let outcome = behavior(&drive, &saved, &folder.id, &state, &checkpoint, &mut results).await;
        let cleanup = mutate(&saved, &folder.id, json!({"trashed":true})).await;
        if cleanup.is_ok() {
            pass(&mut results, "temporary test folder moved to Trash");
        }
        private_write(&state.join("report.json"), &serde_json::to_vec_pretty(&json!({"passed":results,"behavior_succeeded":outcome.is_ok(),"cleanup_succeeded":cleanup.is_ok()}))?)?;
        cleanup?;
        outcome?;
        let reloaded: GoogleConnection = serde_json::from_slice(&fs::read(&saved.path)?)?;
        let reloaded = Arc::new(Saved { path: saved.path.clone(), connection: Mutex::new(Some(reloaded)) });
        let restarted = CloudConnections::new(oauth, reloaded);
        restarted.google_drive(scope)?.list_folder(&root, None, true).await?;
        pass(&mut results, "persisted connection works after recreating the connection owner");
        private_write(&state.join("report.json"), &serde_json::to_vec_pretty(&json!({"passed":results,"behavior_succeeded":true,"cleanup_succeeded":true}))?)?;
        Ok(())
    }
}
