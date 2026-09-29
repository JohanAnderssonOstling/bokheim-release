use crate::session::{validate_email, validate_identity, AuthError, Session};
use account_contract::{AccountSnapshotResponse, MeResponse};
use sync_common::api::libraries::{DeleteLibraryResponse, LibraryNameRequest, LibrarySummary};

pub async fn current_account(session: &Session) -> Result<MeResponse, AuthError> {
    let account: MeResponse = binary_http::receive(reqwest::Client::builder().build()?.get(session.server_url().endpoint("api/auth/me")).bearer_auth(session.token())).await?;
    validate_identity(&account.user_id, "user id")?;
    validate_email(&account.email)?;
    if account.user_id != session.user_id() || account.email != session.email() {
        return Err(AuthError::InvalidData("session identity does not match the identity authority".to_owned()));
    }
    Ok(account)
}

pub async fn account_snapshot(session: &Session) -> Result<AccountSnapshotResponse, AuthError> {
    Ok(binary_http::receive(reqwest::Client::builder().build()?.get(session.server_url().endpoint("api/account/snapshot")).bearer_auth(session.token())).await?)
}

pub async fn list_account_sessions(session: &Session) -> Result<Vec<account_contract::SessionSummary>, AuthError> {
    Ok(binary_http::receive(reqwest::Client::builder().build()?.get(session.server_url().endpoint("api/auth/sessions")).bearer_auth(session.token())).await?)
}

pub async fn revoke_account_session(session: &Session, session_id: &str) -> Result<account_contract::SessionRevocationResponse, AuthError> {
    validate_identity(session_id, "session id")?;
    let endpoint = format!("api/auth/sessions/{}", urlencoding::encode(session_id));
    Ok(binary_http::receive(reqwest::Client::builder().build()?.delete(session.server_url().endpoint(&endpoint)).bearer_auth(session.token())).await?)
}

pub async fn revoke_other_account_sessions(session: &Session) -> Result<account_contract::OtherSessionsRevocationResponse, AuthError> {
    Ok(binary_http::receive(reqwest::Client::builder().build()?.delete(session.server_url().endpoint("api/auth/sessions")).bearer_auth(session.token())).await?)
}

pub async fn create_library(session: &Session, library_id: sync_common::LibraryId, library_name: String) -> Result<LibrarySummary, AuthError> {
    let request = LibraryNameRequest { library_id, library_name };
    Ok(binary_http::send(reqwest::Client::builder().build()?.post(session.server_url().endpoint("api/libraries")).bearer_auth(session.token()), &request).await?)
}

pub async fn rename_library(session: &Session, library_id: sync_common::LibraryId, library_name: String) -> Result<LibrarySummary, AuthError> {
    let request = LibraryNameRequest { library_id, library_name };
    Ok(binary_http::send(reqwest::Client::builder().build()?.put(session.server_url().endpoint("api/libraries")).bearer_auth(session.token()), &request).await?)
}

pub async fn delete_library(session: &Session, library_id: &sync_common::LibraryId) -> Result<DeleteLibraryResponse, AuthError> {
    Ok(binary_http::receive(reqwest::Client::builder().build()?.delete(session.server_url().endpoint(&format!("api/libraries/{library_id}"))).bearer_auth(session.token())).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerUrl;
    use axum::http::{header, HeaderMap, HeaderValue};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use sync_common::transport as wire;

    const TEST_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[tokio::test]
    async fn account_snapshot_uses_the_authenticated_binary_endpoint() {
        let library_id = sync_common::LibraryId::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let expected_snapshot = AccountSnapshotResponse {
            libraries: vec![LibrarySummary { library_id, library_name: "Classics".to_owned() }],
            deleted_library_ids: Vec::new(),
            storage: account_contract::StorageUsageResponse { used_bytes: 40, reserved_bytes: 2, quota_bytes: 100 },
            library_storage: vec![account_contract::LibraryStorageUsage { library_id: library_id.to_string(), used_bytes: 40 }],
        };
        let response_snapshot = expected_snapshot.clone();
        let app = Router::new().route(
            "/api/account/snapshot",
            get(|headers: HeaderMap| async move {
                let expected = format!("Bearer {TEST_TOKEN}");
                assert_eq!(headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok()), Some(expected.as_str()));
                let encoded = wire::encode(&response_snapshot).unwrap();
                let mut response = encoded.into_response();
                response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(wire::MEDIA_TYPE));
                response
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let session = Session::try_new(ServerUrl::parse(&format!("http://{address}")).unwrap(), "user-1".to_owned(), "reader@example.com".to_owned(), TEST_TOKEN.to_owned()).unwrap();

        assert_eq!(account_snapshot(&session).await.unwrap(), expected_snapshot);
        server.abort();
    }
}

pub async fn cloud_storage(session: &Session) -> Result<Vec<sync_common::api::libraries::LibraryCloudStorage>, AuthError> {
    Ok(binary_http::receive(reqwest::Client::builder().build()?.get(session.server_url().endpoint("api/cloud-storage")).bearer_auth(session.token())).await?)
}

pub async fn set_cloud_storage(session: &Session, id: sync_common::LibraryId, change: sync_common::api::libraries::CloudStorageChange) -> Result<sync_common::api::libraries::LibraryCloudStorage, AuthError> {
    Ok(binary_http::send(reqwest::Client::builder().build()?.put(session.server_url().endpoint(&format!("api/libraries/{id}/cloud-storage"))).bearer_auth(session.token()), &change).await?)
}
