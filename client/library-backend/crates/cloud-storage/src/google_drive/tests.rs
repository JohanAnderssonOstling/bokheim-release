use super::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self { status: 200, headers: vec![("Content-Type", "application/json".into())], body: serde_json::to_vec(&value).unwrap() }
    }
}

fn scope() -> ProviderScope {
    ProviderScope { provider: CloudProvider::GoogleDrive, account_id: ProviderId::new("account").unwrap(), container_id: ProviderId::new("my_drive").unwrap() }
}
fn drive_file(version: &str) -> Value {
    json!({"id": "audio", "name": "Book.m4b", "mimeType": "audio/mp4", "parents": ["folder"], "size": "4", "version": version, "capabilities": {"canDownload": true}})
}

async fn server(replies: Vec<Reply>) -> (GoogleDrive, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let mut drive = GoogleDrive::new(scope()).unwrap();
    drive.api = base.join("drive/v3/").unwrap();
    drive.uploads = base.join("upload/drive/v3/").unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let header_end = loop {
                let mut buffer = [0; 8192];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "request ended before its headers");
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec()).unwrap();
            let length = headers.lines().filter_map(|line| line.split_once(':')).find(|(name, _)| name.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
            while request.len() - header_end < length {
                let mut buffer = [0; 8192];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            requests.push(String::from_utf8(request).unwrap());
            let mut response = format!("HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n", reply.status, reply.body.len());
            for (name, value) in reply.headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str("\r\n");
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&reply.body).await.unwrap();
        }
        requests
    });
    (drive, server)
}

async fn requests(server: JoinHandle<Vec<String>>) -> Vec<String> {
    tokio::time::timeout(std::time::Duration::from_secs(10), server).await.unwrap().unwrap()
}

#[tokio::test]
async fn content_revision_survives_background_metadata_changes_during_a_read() {
    let mut before = drive_file("1");
    before["headRevisionId"] = "content-one".into();
    let mut after = before.clone();
    after["version"] = "3".into();
    after["name"] = "Renamed.m4b".into();
    let (drive, task) = server(vec![Reply::json(before.clone()), Reply { status: 206, headers: vec![("Content-Range", "bytes 1-2/4".into())], body: b"bc".to_vec() }, Reply::json(after)]).await;
    let CloudEntry::File(file) = serde_json::from_value::<DriveFile>(before).unwrap().entry(&scope()).unwrap().unwrap() else { panic!("expected file") };
    assert_eq!(file.file.revision.as_str(), "head:content-one");
    assert_eq!(drive.read_range("token", &file.file, 1, 2).await.unwrap(), b"bc");
    assert_eq!(requests(task).await.len(), 3);
}

#[tokio::test]
async fn replacement_content_revision_is_rejected_before_downloading_bytes() {
    let mut original = drive_file("1");
    original["headRevisionId"] = "content-one".into();
    let mut replacement = original.clone();
    replacement["headRevisionId"] = "content-two".into();
    let (drive, task) = server(vec![Reply::json(replacement)]).await;
    let CloudEntry::File(file) = serde_json::from_value::<DriveFile>(original).unwrap().entry(&scope()).unwrap().unwrap() else { panic!("expected file") };
    assert!(matches!(drive.read_range("token", &file.file, 1, 2).await, Err(CloudError::RevisionChanged)));
    assert_eq!(requests(task).await.len(), 1);
}

#[tokio::test]
async fn folder_picker_uses_folder_ids_and_follows_pages_without_reading_book_bytes() {
    let (drive, task) = server(vec![Reply::json(json!({"nextPageToken":"second", "files":[{"id":"child", "name":"Audio", "mimeType":FOLDER_MIME, "parents":["folder"]}]})), Reply::json(json!({"files":[]}))]).await;
    let folder = ProviderId::new("folder'\\").unwrap();
    let first = drive.list_folder("secret", &folder, None, true).await.unwrap();
    assert_eq!(first.next_page.as_deref(), Some("second"));
    assert!(matches!(&first.entries[0], CloudEntry::Folder(folder) if folder.id.as_str() == "child"));
    drive.list_folder("secret", &folder, first.next_page.as_deref(), true).await.unwrap();
    let captured = requests(task).await;
    for request in &captured {
        assert!(request.to_lowercase().contains("authorization: bearer secret"));
        let path = request.lines().next().unwrap().split_whitespace().nth(1).unwrap();
        assert!(!path.contains("secret"));
        let query = Url::parse(&format!("http://test{path}")).unwrap();
        let filter = query.query_pairs().find(|(key, _)| key == "q").unwrap().1.into_owned();
        assert!(filter.contains("'folder\\'\\\\' in parents"));
        assert!(filter.contains(FOLDER_MIME));
    }
    assert!(captured[1].contains("pageToken=second"));
}

#[tokio::test]
async fn app_created_folders_keep_their_library_directory_identity() {
    let identity = CloudDirectoryIdentity { library_id: sync_common::LibraryId::from_u128(1), directory_id: sync_common::DirId::from_u128(2) };
    let properties = json!({"bokheimLibrary": identity.library_id.to_string(), "bokheimDirectory": identity.directory_id.to_string()});
    let (drive, task) = server(vec![Reply::json(json!({"id":"nested", "name":"Audio", "mimeType":FOLDER_MIME, "parents":["folder"], "appProperties":properties}))]).await;
    let folder = drive.create_library_folder("secret", &ProviderId::new("folder").unwrap(), "Audio", Some(&identity)).await.unwrap();
    assert_eq!(folder.directory_hint, Some(identity));
    let captured = requests(task).await;
    let metadata: Value = serde_json::from_str(captured[0].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(metadata["appProperties"], properties);
    assert_eq!(metadata["parents"], json!(["folder"]));
}

#[tokio::test]
async fn seekable_reads_check_revision_and_exact_range() {
    let range = Reply { status: 206, headers: vec![("Content-Range", "bytes 1-2/4".into())], body: b"bc".to_vec() };
    let (drive, task) = server(vec![Reply::json(drive_file("1")), range, Reply::json(drive_file("1"))]).await;
    let CloudEntry::File(file) = serde_json::from_value::<DriveFile>(drive_file("1")).unwrap().entry(&scope()).unwrap().unwrap() else { panic!() };
    assert_eq!(drive.read_range("secret", &file.file, 1, 2).await.unwrap(), b"bc");
    let captured = requests(task).await;
    assert!(captured[1].to_lowercase().contains("range: bytes=1-2"));
    assert!(captured[1].contains("alt=media"));
}

#[tokio::test]
async fn replacement_during_a_range_never_enters_the_cache() {
    let range = Reply { status: 206, headers: vec![("Content-Range", "bytes 0-3/4".into())], body: b"abcd".to_vec() };
    let (drive, task) = server(vec![Reply::json(drive_file("1")), range, Reply::json(drive_file("2"))]).await;
    let CloudEntry::File(file) = serde_json::from_value::<DriveFile>(drive_file("1")).unwrap().entry(&scope()).unwrap().unwrap() else { panic!() };
    assert_eq!(drive.read_range("secret", &file.file, 0, 4).await.unwrap_err(), CloudError::RevisionChanged);
    requests(task).await;
}

#[tokio::test]
async fn rejected_range_does_not_download_the_whole_audiobook() {
    let (drive, task) = server(vec![Reply::json(drive_file("1")), Reply::json(json!("whole file"))]).await;
    let CloudEntry::File(file) = serde_json::from_value::<DriveFile>(drive_file("1")).unwrap().entry(&scope()).unwrap().unwrap() else { panic!() };
    assert!(matches!(drive.read_range("secret", &file.file, 0, 4).await, Err(CloudError::InvalidResponse(_))));
    requests(task).await;
}

#[tokio::test]
async fn upload_is_not_complete_until_drive_commits_matching_file_metadata() {
    let (drive, task) = server(vec![Reply { status: 308, headers: vec![], body: vec![] }, Reply::json(drive_file("1"))]).await;
    let upload = DriveUpload { url: drive.uploads.join("files?upload_id=secret").unwrap(), scope: scope(), file_id: ProviderId::new("audio").unwrap(), size_bytes: 4 };
    assert!(matches!(drive.upload_status("secret", &upload).await.unwrap(), UploadProgress::Incomplete { confirmed_bytes: 0 }));
    let UploadProgress::Complete(file) = drive.upload_chunk("secret", &upload, 0, b"abcd".to_vec()).await.unwrap() else {
        panic!("final upload did not commit");
    };
    assert_eq!(file.identity.file_id.as_str(), "audio");
    let captured = requests(task).await;
    assert!(captured[1].to_lowercase().contains("content-range: bytes 0-3/4"));
}

#[tokio::test]
async fn changes_preserve_checkpoint_and_removed_file_ids() {
    let (drive, task) = server(vec![Reply::json(json!({"newStartPageToken":"next", "changes":[{"fileId":"audio", "file":drive_file("2")}, {"fileId":"deleted", "removed":true}]}))]).await;
    let changes = drive.changes("secret", "previous").await.unwrap();
    assert_eq!(changes.checkpoint.as_deref(), Some("next"));
    assert_eq!(changes.changes[1], CloudChange::Removed(ProviderId::new("deleted").unwrap()));
    assert!(requests(task).await[0].contains("pageToken=previous"));
}

#[test]
fn persisted_upload_sessions_cannot_redirect_credentials_to_another_origin() {
    let base = Url::parse("https://www.googleapis.com/upload/drive/v3/").unwrap();
    for url in ["https://evil.example/upload/drive/v3/files", "https://www.googleapis.com.evil.example/upload/drive/v3/files", "https://user@www.googleapis.com/upload/drive/v3/files", "https://www.googleapis.com/other/files"] {
        assert!(!trusted_upload_url(&Url::parse(url).unwrap(), &base));
    }
    assert!(trusted_upload_url(&base.join("files?upload_id=secret").unwrap(), &base));
}

fn rejected(reason: &str) -> Reply {
    let mut reply = Reply::json(json!({"error":{"code":403,"message":"provider diagnostic","errors":[{"reason":reason}]}}));
    reply.status = 403;
    reply
}

#[tokio::test]
async fn storage_quota_is_distinct_from_permissions_during_upload_creation_and_commit() {
    let (drive, task) = server(vec![rejected("storageQuotaExceeded"), rejected("storageQuotaExceeded")]).await;
    assert!(matches!(drive.begin_upload("secret", &ProviderId::new("folder").unwrap(), &ProviderId::new("audio").unwrap(), "Book.m4b", 4).await, Err(CloudError::StorageFull)));
    let upload = DriveUpload { url: drive.uploads.join("files?upload_id=secret").unwrap(), scope: scope(), file_id: ProviderId::new("audio").unwrap(), size_bytes: 4 };
    let error = match drive.upload_chunk("secret", &upload, 0, b"abcd".to_vec()).await {
        Err(error) => error,
        Ok(_) => panic!("quota-rejected upload cannot be complete"),
    };
    assert_eq!(error, CloudError::StorageFull);
    assert_eq!(std::error::Error::source(&error).unwrap().downcast_ref::<operation_failure::Failure>(), Some(&operation_failure::Failure::CloudStorageFull));
    assert_eq!(requests(task).await.len(), 2);
}

#[tokio::test]
async fn request_limits_are_not_reported_as_full_storage_or_permission_denials() {
    for reason in ["rateLimitExceeded", "userRateLimitExceeded", "dailyLimitExceeded", "quotaExceeded", "insufficientFilePermissions"] {
        let (drive, task) = server(vec![rejected(reason)]).await;
        let error = drive.list_folder("secret", &ProviderId::new("folder").unwrap(), None, false).await.unwrap_err();
        assert_eq!(error, if reason == "insufficientFilePermissions" { CloudError::PermissionDenied } else { CloudError::RateLimited });
        requests(task).await;
    }
}

#[tokio::test]
async fn provider_error_message_text_cannot_misclassify_a_permission_error() {
    let mut reply = Reply::json(json!({"error":{"message":"storageQuotaExceeded", "errors":[{"reason":"insufficientFilePermissions"}]}}));
    reply.status = 403;
    let (drive, task) = server(vec![reply]).await;
    assert!(matches!(drive.list_folder("secret", &ProviderId::new("folder").unwrap(), None, false).await, Err(CloudError::PermissionDenied)));
    requests(task).await;
}
