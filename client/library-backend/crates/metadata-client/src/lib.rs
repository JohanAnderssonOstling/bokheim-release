use metadata_contract::{EditionIdentityRequest, EditionIdentityResponse, MetadataEnrichmentRequest, RichMetadataEnrichmentResponse};
use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use url::Url;

#[cfg(target_arch = "wasm32")]
async fn browser_delay(milliseconds: u32) {
    use wasm_bindgen::prelude::*;
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(catch, js_namespace = globalThis, js_name = setTimeout)]
        fn set_timeout(callback: &js_sys::Function, milliseconds: u32) -> Result<i32, JsValue>;
    }
    let delay = js_sys::Promise::new(&mut |resolve, reject| {
        if let Err(error) = set_timeout(&resolve, milliseconds) {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(delay).await;
}

#[derive(Clone)]
pub struct MetadataClient {
    http: reqwest::Client,
    base_url: Url,
    cover_origin: Url,
}

static LAST_COVER_REQUEST: tokio::sync::Mutex<Option<web_time::Instant>> = tokio::sync::Mutex::const_new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataClientError(String);

impl MetadataClientError {
    fn new(message: impl fmt::Display) -> Self {
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
        Ok(Self { http, base_url, cover_origin: Url::parse("https://covers.openlibrary.org/").expect("valid cover origin") })
    }

    /// Override the cover origin for a proxy or a local test server.
    pub fn with_cover_origin(mut self, origin: &str) -> Result<Self, MetadataClientError> {
        self.cover_origin = Self::parse_origin(origin)?;
        Ok(self)
    }

    fn parse_origin(origin: &str) -> Result<Url, MetadataClientError> {
        let url = Url::parse(origin).map_err(MetadataClientError::new)?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(MetadataClientError::new("server URL must be an HTTP origin or base path without credentials, query, or fragment"));
        }
        Ok(url)
    }

    /// Fetch artwork for an already-resolved ISBN; identity is owned by enrichment.
    pub async fn cover(&self, isbn: &str) -> Result<Option<Vec<u8>>, MetadataClientError> {
        if !matches!(isbn.len(), 10 | 13) || !isbn.bytes().all(|byte| byte.is_ascii_digit() || byte == b'X') {
            return Err(MetadataClientError::new("invalid cover ISBN"));
        }
        self.artwork(&format!("b/isbn/{isbn}-L.jpg?default=false")).await
    }

    /// Fetch artwork only after the library has selected this recording.
    pub async fn audible_cover(&self, asin: &str, region: &str) -> Result<Option<Vec<u8>>, MetadataClientError> {
        if asin.len() != 10 || !asin.bytes().all(|b| b.is_ascii_alphanumeric()) || !matches!(region, "us" | "uk") {
            return Err(MetadataClientError::new("invalid audiobook artwork identity"));
        }
        Self::with_deadline(35_000, async {
            let response = self.http.get(self.endpoint(&format!("v2/audiobooks/audible?region={region}&asin={asin}"))?).send().await.map_err(MetadataClientError::new)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            Self::response_bytes(response.error_for_status().map_err(MetadataClientError::new)?, 4 * 1024 * 1024).await.map(Some)
        })
        .await
    }

    async fn artwork(&self, path: &str) -> Result<Option<Vec<u8>>, MetadataClientError> {
        let mut last = LAST_COVER_REQUEST.lock().await;
        // Keep the anonymous Covers API at no more than one request per second.
        if let Some(previous) = *last {
            let remaining = std::time::Duration::from_secs(1).saturating_sub(previous.elapsed());
            #[cfg(not(target_arch = "wasm32"))]
            tokio::time::sleep(remaining).await;
            #[cfg(target_arch = "wasm32")]
            browser_delay(remaining.as_millis() as u32).await;
        }
        *last = Some(web_time::Instant::now());
        drop(last);
        Self::with_timeout(async {
            let url = self.cover_origin.join(path).map_err(MetadataClientError::new)?;
            let request = self.http.get(url);
            #[cfg(not(target_arch = "wasm32"))]
            let request = request.header(reqwest::header::USER_AGENT, "Bokheim/0.1 (cover enrichment)");
            let response = request.send().await.map_err(MetadataClientError::new)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            let response = response.error_for_status().map_err(MetadataClientError::new)?;
            Self::response_bytes(response, 4 * 1024 * 1024).await.map(Some)
        })
        .await
    }

    pub async fn enrichment(&self, request: &MetadataEnrichmentRequest) -> Result<RichMetadataEnrichmentResponse, MetadataClientError> {
        let body = metadata_contract::encode_v2_request(request).map_err(MetadataClientError::new)?;
        let bytes = self.post("v2/enrichment", body).await?;
        metadata_contract::decode_v2_response(&bytes, metadata_contract::MAX_RESPONSE_BYTES).map_err(MetadataClientError::new)
    }

    pub async fn resolve_edition_identities(&self, request: &EditionIdentityRequest) -> Result<EditionIdentityResponse, MetadataClientError> {
        let body = metadata_contract::encode_v2_edition_identity_request(request).map_err(MetadataClientError::new)?;
        let bytes = self.post("v2/edition-identities", body).await?;
        metadata_contract::decode_v2_edition_identity_response(&bytes, metadata_contract::MAX_RESPONSE_BYTES).map_err(MetadataClientError::new)
    }

    pub async fn audible_lookup(&self, request: &metadata_contract::audible::LookupRequest) -> Result<metadata_contract::audible::LookupResponse, MetadataClientError> {
        let body = serde_json::to_vec(request).map_err(MetadataClientError::new)?;
        if body.len() > metadata_contract::audible::MAX_LOOKUP_REQUEST_BYTES {
            return Err(MetadataClientError::new("audiobook request exceeds size limit"));
        }
        Self::with_deadline(60_000, async {
            let response = self
                .http
                .post(self.endpoint("v2/audiobooks/audible")?)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .map_err(MetadataClientError::new)?
                .error_for_status()
                .map_err(MetadataClientError::new)?;
            let bytes = Self::response_bytes(response, 8 * 1024 * 1024).await?;
            serde_json::from_slice(&bytes).map_err(MetadataClientError::new)
        })
        .await
    }

    async fn with_timeout<T>(operation: impl std::future::Future<Output = Result<T, MetadataClientError>>) -> Result<T, MetadataClientError> {
        Self::with_deadline(30_000, operation).await
    }

    async fn with_deadline<T>(milliseconds: u32, operation: impl std::future::Future<Output = Result<T, MetadataClientError>>) -> Result<T, MetadataClientError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            tokio::time::timeout(Duration::from_millis(u64::from(milliseconds)), operation).await.map_err(|_| MetadataClientError::new("metadata request timed out"))?
        }
        #[cfg(target_arch = "wasm32")]
        {
            use futures_util::future::{select, Either};
            match select(Box::pin(operation), Box::pin(browser_delay(milliseconds))).await {
                Either::Left((result, _)) => result,
                Either::Right(_) => Err(MetadataClientError::new("metadata request timed out")),
            }
        }
    }

    async fn post(&self, path: &str, body: Vec<u8>) -> Result<Vec<u8>, MetadataClientError> {
        Self::with_timeout(async {
            let media_type = metadata_contract::MEDIA_TYPE_V2;
            let response = self.http.post(self.endpoint(path)?).header(reqwest::header::ACCEPT, media_type).header(reqwest::header::CONTENT_TYPE, media_type).body(body).send().await.map_err(MetadataClientError::new)?;
            if !response.status().is_success() {
                let status = response.status();
                let mut detail = Vec::new();
                const MAX_ERROR_DETAIL_BYTES: usize = 4 * 1024;
                let mut stream = response.bytes_stream();
                while detail.len() < MAX_ERROR_DETAIL_BYTES {
                    let Some(chunk) = stream.next().await else { break };
                    let chunk = chunk.map_err(MetadataClientError::new)?;
                    let remaining = MAX_ERROR_DETAIL_BYTES - detail.len();
                    detail.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                }
                let detail = String::from_utf8_lossy(&detail).split_whitespace().collect::<Vec<_>>().join(" ");
                let message = if detail.is_empty() { format!("metadata server returned HTTP {status}") } else { format!("metadata server returned HTTP {status}: {detail}") };
                return Err(MetadataClientError::new(message));
            }
            Self::response_bytes(response, metadata_contract::MAX_RESPONSE_BYTES).await
        })
        .await
    }

    async fn response_bytes(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, MetadataClientError> {
        if response.content_length().is_some_and(|length| length > max_bytes as u64) {
            return Err(MetadataClientError::new("metadata response is too large"));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(MetadataClientError::new)?;
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(MetadataClientError::new("metadata response is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn endpoint(&self, path: &str) -> Result<Url, MetadataClientError> {
        self.base_url.join(path).map_err(MetadataClientError::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_origins_and_embedded_credentials() {
        assert!(MetadataClient::new("file:///tmp/metadata.sqlite").is_err());
        assert!(MetadataClient::new("https://user@example.test/").is_err());
    }
}
use futures_util::StreamExt;
