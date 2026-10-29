use http_response::BodyReadError;
use library_runtime::executor;
use std::fmt;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub struct MetadataClient {
    http: reqwest::Client,
    base_url: Url,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataClientError(String);

impl MetadataClientError {
    pub(crate) fn new(message: impl fmt::Display) -> Self {
        Self(message.to_string())
    }
}

impl fmt::Display for MetadataClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MetadataClientError {}

impl MetadataClient {
    pub fn new(base_url: &str) -> Result<Self, MetadataClientError> {
        let base_url = Self::parse_origin(base_url)?;
        let http = reqwest::Client::new();
        Ok(Self { http, base_url })
    }

    fn parse_origin(origin: &str) -> Result<Url, MetadataClientError> {
        let url = Url::parse(origin).map_err(MetadataClientError::new)?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(MetadataClientError::new("server URL must be an HTTP origin or base path without credentials, query, or fragment"));
        }
        Ok(url)
    }

    /// Every lookup and merge rule for a batch of books runs on the server.
    pub async fn books(&self, request: &metadata_contract::books::BookEnrichmentRequest) -> Result<metadata_contract::books::BookEnrichmentResponse, MetadataClientError> {
        let body = serde_json::to_vec(request).map_err(MetadataClientError::new)?;
        if body.len() > metadata_contract::books::MAX_BOOKS_REQUEST_BYTES {
            return Err(MetadataClientError::new("book request exceeds size limit"));
        }
        // The server resolves books concurrently, but one may wait on Audible
        // and the Library of Congress fallback.
        Self::with_deadline(120_000, async {
            let response =
                self.http.post(self.endpoint("v3/books")?).header(reqwest::header::CONTENT_TYPE, "application/json").body(body).send().await.map_err(MetadataClientError::new)?.error_for_status().map_err(MetadataClientError::new)?;
            let bytes = Self::response_bytes(response, metadata_contract::MAX_RESPONSE_BYTES).await?;
            serde_json::from_slice(&bytes).map_err(MetadataClientError::new)
        })
        .await
    }

    pub(crate) async fn with_deadline<T>(milliseconds: u32, operation: impl std::future::Future<Output = Result<T, MetadataClientError>>) -> Result<T, MetadataClientError> {
        executor::timeout(Duration::from_millis(u64::from(milliseconds)), operation).await.map_err(|_| MetadataClientError::new("metadata request timed out"))?
    }

    pub(crate) async fn response_bytes(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, MetadataClientError> {
        http_response::read(response, max_bytes).await.map_err(|error| match error {
            BodyReadError::Http(error) => MetadataClientError::new(error.without_url()),
            BodyReadError::TooLarge { .. } => MetadataClientError::new("metadata response is too large"),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, MetadataClientError> {
        self.base_url.join(path).map_err(MetadataClientError::new)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::{cell::Cell, future::pending};

    struct ResponseBody<'a>(&'a Cell<bool>);
    impl Drop for ResponseBody<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_deadline_releases_the_pending_response_body() {
        let released = Cell::new(false);
        let error = MetadataClient::with_deadline(10, async {
            let _body = ResponseBody(&released);
            pending::<Result<(), MetadataClientError>>().await
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "metadata request timed out");
        assert!(released.get());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_lookup_releases_its_pending_response_body() {
        let released = Cell::new(false);
        let mut lookup = Box::pin(MetadataClient::with_deadline(120_000, async {
            let _body = ResponseBody(&released);
            pending::<Result<(), MetadataClientError>>().await
        }));
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(lookup);
        assert!(released.get());
    }
}
