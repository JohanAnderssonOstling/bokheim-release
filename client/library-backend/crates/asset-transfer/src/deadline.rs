//! Deadlines release owned request resources on cancellation.
#[cfg(test)]
use crate::policy::SMALL_REQUEST_TIMEOUT;
use crate::TransferError;

pub async fn transfer_deadline<T>(duration: std::time::Duration, request: impl std::future::Future<Output = T>) -> Result<T, TransferError> {
    client_platform_runtime::executor::timeout(duration, request).await.map_err(|_| TransferError::retryable(format!("asset request timed out after {} seconds", duration.as_secs())))
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn timed_out_request_is_dropped_and_retryable() {
        let (alive, dropped) = tokio::sync::oneshot::channel::<()>();
        let stalled = async move {
            let _alive = alive;
            std::future::pending::<()>().await
        };
        assert!(matches!(transfer_deadline(SMALL_REQUEST_TIMEOUT, stalled).await, Err(TransferError::Retryable(_))));
        assert!(dropped.await.is_err(), "timeout must drop the request's owned resources");
        assert_eq!(transfer_deadline(SMALL_REQUEST_TIMEOUT, async { 42 }).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn thumbnail_deadline_covers_stalled_headers_and_body() {
        use axum::{body::Body, response::Response, routing::post, Router};
        for send_headers in [false, true] {
            let library = test_support::fixture_library_id("timeout");
            let route = format!("/api/libraries/{library}/thumbnail-batch");
            let app = Router::new().route(
                &route,
                post(move || async move {
                    if send_headers {
                        Response::new(Body::from_stream(futures_util::stream::pending::<Result<axum::body::Bytes, std::io::Error>>()))
                    } else {
                        std::future::pending::<Response>().await
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = reqwest::Client::new();
            let result = transfer_deadline(Duration::from_millis(100), crate::download_thumbnail_batch(&client, &url, "token", &library, vec![])).await;
            server.abort();
            assert!(matches!(result, Err(TransferError::Retryable(_))), "send_headers={send_headers}");
        }
    }
}
