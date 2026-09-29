use crate::api::{send, SyncCredentials, TransportError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sync_common::{LibraryId, ReplicaId, StateInventoryRequest, StateInventoryResponse, SyncCursor, SyncExchangeRequest, SyncExchangeResponse};

/// Control-flow relevant failures from synchronization endpoints. Account
/// Account APIs retain their own errors; sync callers should never need to inspect an HTTP
/// status or downcast an erased error to decide whether to refresh or recover.
#[derive(Debug)]
pub enum SyncRequestError {
    AuthenticationRequired,
    CursorRejected,
    Other(TransportError),
}

impl std::fmt::Display for SyncRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthenticationRequired => formatter.write_str("synchronization authentication is required"),
            Self::CursorRejected => formatter.write_str("the synchronization cursor was rejected"),
            Self::Other(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SyncRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Other(error) => Some(error),
            Self::AuthenticationRequired | Self::CursorRejected => None,
        }
    }
}

impl From<TransportError> for SyncRequestError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::BadStatus(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN, _) => Self::AuthenticationRequired,
            TransportError::BadStatus(reqwest::StatusCode::GONE, _) => Self::CursorRejected,
            error => Self::Other(error),
        }
    }
}

pub async fn exchange_changes(
    client: &reqwest::Client, credentials: &SyncCredentials, library_id: LibraryId, replica_id: ReplicaId, mutations: Vec<sync_common::WireMutation>, cursor: SyncCursor,
) -> Result<SyncExchangeResponse, SyncRequestError> {
    let request = SyncExchangeRequest { library_id, replica_id, mutations, cursor };
    let mut trace = crate::PerformanceTrace::new("exchange_http", "encode_request_response_decode");
    log::debug!(target: "sync_performance", "trace_id={} library_id={} push_count={} cursor={:?}", trace.id(), request.library_id, request.mutations.len(), request.cursor);
    Ok(crate::api::send_with_trace(client.post(credentials.endpoint("api/sync/exchange")).bearer_auth(credentials.access_token()), &request, &mut trace).await?)
}

pub async fn compare_state_inventory(client: &reqwest::Client, credentials: &SyncCredentials, library_id: LibraryId, cells: Vec<sync_common::StateCell>) -> Result<Option<StateInventoryResponse>, SyncRequestError> {
    let request = StateInventoryRequest { library_id, cells };
    match post(client, credentials, "api/sync/inventory", &request).await {
        Ok(response) => Ok(Some(response)),
        Err(TransportError::BadStatus(reqwest::StatusCode::NOT_FOUND, _)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn post<T: Serialize, R: DeserializeOwned>(client: &reqwest::Client, credentials: &SyncCredentials, path: &str, value: &T) -> Result<R, TransportError> {
    send(client.post(credentials.endpoint(path)).bearer_auth(credentials.access_token()), value).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;
    use axum::http::{header, HeaderMap, HeaderValue};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use sync_common::{PullStateResponse, PushMutationsResponse};
    use test_support::fixture_content_hash;

    const TEST_TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn only_failures_that_change_control_flow_are_promoted() {
        assert!(matches!(SyncRequestError::from(TransportError::BadStatus(reqwest::StatusCode::UNAUTHORIZED, String::new())), SyncRequestError::AuthenticationRequired));
        assert!(matches!(SyncRequestError::from(TransportError::BadStatus(reqwest::StatusCode::GONE, String::new())), SyncRequestError::CursorRejected));
        assert!(matches!(SyncRequestError::from(TransportError::BadStatus(reqwest::StatusCode::TOO_MANY_REQUESTS, String::new())), SyncRequestError::Other(_)));
    }

    #[tokio::test]
    async fn missing_inventory_endpoint_is_an_absent_capability() {
        let app = Router::new().route("/api/sync/inventory", post(|| async { axum::http::StatusCode::NOT_FOUND }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let response = compare_state_inventory(&reqwest::Client::new(), &session(address), test_support::fixture_library_id("library"), Vec::new()).await;

        assert!(matches!(response, Ok(None)));
        server.abort();
    }

    fn mutations(count: usize) -> Vec<sync_common::WireMutation> {
        (1..=count)
            .map(|sequence| sync_common::WireMutation {
                origin: None,
                mutation_id: sync_common::MutationId::new(),
                kind: sync_common::mutation_kind::BOOK_LIFECYCLE.to_owned(),
                entity_key: fixture_content_hash(sequence as u64).to_string(),
                entity_subkey: String::new(),
                value: vec![sequence as u8; 1024],
                conflict_rank: 0,
                blob_reference: Some(sync_common::DeclaredBlobReference { present: true, content_hash: Some(fixture_content_hash(sequence as u64)) }),
                changed_at: 1,
                replica_seq: sync_common::ReplicaSeq::new(sequence as u64).unwrap(),
            })
            .collect()
    }

    fn session(address: std::net::SocketAddr) -> SyncCredentials {
        SyncCredentials::new(binary_http::ServerUrl::parse(&format!("http://{address}")).unwrap(), TEST_TOKEN)
    }

    #[tokio::test]
    async fn sends_and_receives_versioned_binary() {
        let observed_binary = Arc::new(AtomicBool::new(false));
        let binary = observed_binary.clone();
        let app = Router::new().route(
            "/api/sync/exchange",
            post(move |headers: HeaderMap, body: Bytes| {
                let binary = binary.clone();
                async move {
                    assert!(uuid::Uuid::parse_str(headers.get("x-bokheim-trace-id").unwrap().to_str().unwrap()).is_ok());
                    assert!(headers.get(header::CONTENT_ENCODING).is_none());
                    binary.store(headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.starts_with(sync_common::transport::MEDIA_TYPE_BASE)), Ordering::SeqCst);
                    let request: SyncExchangeRequest = sync_common::transport::decode(&body, sync_common::transport::MAX_DECODED_REQUEST_BYTES).unwrap();
                    let response = SyncExchangeResponse {
                        push: PushMutationsResponse { accepted: request.mutations.iter().map(|mutation| mutation.mutation_id).collect(), rejected: Vec::new() },
                        pull: PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: SyncCursor::default(), has_more: false },
                    };
                    let encoded = sync_common::transport::encode(&response).unwrap();
                    let mut response = encoded.into_response();
                    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(sync_common::transport::MEDIA_TYPE));
                    response
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let changes = mutations(100);

        let response = exchange_changes(&reqwest::Client::new(), &session(address), test_support::fixture_library_id("library"), test_support::fixture_replica_id("replica"), changes, SyncCursor::default()).await.unwrap();

        assert!(observed_binary.load(Ordering::SeqCst));
        assert_eq!(response.push.accepted.len(), 100);
        server.abort();
    }

    #[tokio::test]
    async fn does_not_retry_with_json_after_media_rejection() {
        use axum::extract::State;
        use axum::http::StatusCode;
        use std::sync::atomic::AtomicUsize;

        let attempts = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/api/sync/exchange",
                post(|State(attempts): State<Arc<AtomicUsize>>, _body: Bytes| async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response()
                }),
            )
            .with_state(attempts.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let response = exchange_changes(&reqwest::Client::new(), &session(address), test_support::fixture_library_id("library"), test_support::fixture_replica_id("replica"), mutations(1), SyncCursor::default()).await;

        assert!(matches!(response, Err(SyncRequestError::Other(TransportError::BadStatus(reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE, _)))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn rejects_a_successful_json_response() {
        let app = Router::new().route(
            "/api/sync/exchange",
            post(|| async {
                Json(SyncExchangeResponse { push: PushMutationsResponse { accepted: Vec::new(), rejected: Vec::new() }, pull: PullStateResponse { book_creations: Vec::new(), mutations: Vec::new(), next_cursor: SyncCursor::default(), has_more: false } })
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let response = exchange_changes(&reqwest::Client::new(), &session(address), test_support::fixture_library_id("library"), test_support::fixture_replica_id("replica"), Vec::new(), SyncCursor::default()).await;

        assert!(matches!(response, Err(SyncRequestError::Other(TransportError::InvalidData(message))) if message.contains("unsupported API response content type: application/json")));
        server.abort();
    }
}
