//! Device-side artwork lookup through Open Library and selected audiobook
//! ASINs. No metadata-server image proxy.

use std::{collections::BTreeSet, io::Cursor, sync::Arc, time::Duration};

use book_model::{canonical_isbn, BookRecord, Scheme, Scope};
use image::ImageReader;
use library_runtime::executor;
use metadata_contract::matching::{author_credits, matching_author_count, same_author, NormalizedTitle};
use reqwest::StatusCode;
use serde_json::Value;
use tokio::sync::Mutex;
use url::Url;
use web_time::Instant;

use crate::{MetadataClient, MetadataClientError};

const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOOKUP_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct CoverClient {
    http: reqwest::Client,
    openlibrary_origin: Url,
    openlibrary_covers: Url,
    requests: Arc<Mutex<Option<Instant>>>,
    interval_ms: u32,
}

impl Default for CoverClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::new(),
            openlibrary_origin: Url::parse("https://openlibrary.org/").expect("Open Library origin"),
            openlibrary_covers: Url::parse("https://covers.openlibrary.org/").expect("Open Library covers origin"),
            requests: Arc::default(),
            interval_ms: 1000,
        }
    }
}

impl CoverClient {
    /// Download usable artwork in provider order. Provider outages do not
    /// prevent fallback; if every available provider fails, retry later.
    pub async fn book_cover(&self, record: &BookRecord) -> Result<Option<Vec<u8>>, MetadataClientError> {
        MetadataClient::with_deadline(120_000, self.lookup_cover(record)).await
    }

    async fn lookup_cover(&self, record: &BookRecord) -> Result<Option<Vec<u8>>, MetadataClientError> {
        let isbns = isbns(record);
        let authors = record.authors().map(|author| author.name().to_owned()).collect::<Vec<_>>();
        let mut error = None;
        for isbn in &isbns {
            let url = self.openlibrary_covers.join(&format!("b/isbn/{isbn}-L.jpg?default=false")).map_err(MetadataClientError::new)?;
            match self.image(url).await {
                Ok(Some(bytes)) => return Ok(Some(bytes)),
                Ok(None) => {}
                Err(failure) => error = Some(failure),
            }
        }
        if !record.title.trim().is_empty() && !authors.is_empty() {
            match self.openlibrary_cover(record, &authors).await {
                Ok(Some(bytes)) => return Ok(Some(bytes)),
                Ok(None) => {}
                Err(failure) => error = Some(failure),
            }
        }
        // ASINs are retained by recording enrichment even when it skips covers.
        // Resolve their images directly so audiobook cover recovery still works.
        for identifier in record.book.identifiers.iter().filter(|id| id.scheme() == &Scheme::Asin) {
            let asin = identifier.value();
            if asin.len() != 10 || !asin.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) {
                continue;
            }
            match self.audible_cover(asin).await {
                Ok(Some(bytes)) => return Ok(Some(bytes)),
                Ok(None) => {}
                Err(failure) => error = Some(failure),
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    async fn openlibrary_cover(&self, record: &BookRecord, authors: &[String]) -> Result<Option<Vec<u8>>, MetadataClientError> {
        let mut url = self.openlibrary_origin.join("search.json").map_err(MetadataClientError::new)?;
        url.query_pairs_mut().append_pair("title", &record.title).append_pair("author", &authors[0]).append_pair("limit", "20").append_pair("fields", "title,author_name,cover_i");
        let Some(bytes) = self.get(url, MAX_LOOKUP_BYTES).await? else { return Ok(None) };
        let result: Value = serde_json::from_slice(&bytes).map_err(MetadataClientError::new)?;
        let mut candidates = Vec::new();
        for doc in result.get("docs").and_then(Value::as_array).into_iter().flatten() {
            let title = doc.get("title").and_then(Value::as_str).unwrap_or_default();
            let credits = doc.get("author_name").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
            if !NormalizedTitle::new(&record.title).matches(&NormalizedTitle::new(title)) {
                continue;
            }
            let loose = if strict_authors_match(authors, &credits) {
                false
            } else if matching_author_count(authors, &credits) > 0 {
                true
            } else {
                continue;
            };
            let Some(id) = doc.get("cover_i").and_then(Value::as_u64).filter(|id| *id > 0) else { continue };
            candidates.push((loose, id));
        }
        candidates.sort_by_key(|(loose, _)| *loose);
        for (_, id) in candidates {
            let url = self.openlibrary_covers.join(&format!("b/id/{id}-L.jpg?default=false")).map_err(MetadataClientError::new)?;
            if let Some(bytes) = self.image(url).await? {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    async fn audible_cover(&self, asin: &str) -> Result<Option<Vec<u8>>, MetadataClientError> {
        for origin in ["https://api.audible.com", "https://api.audible.co.uk"] {
            let url = Url::parse(&format!("{origin}/1.0/catalog/products/{asin}?response_groups=product_attrs&image_sizes=500")).map_err(MetadataClientError::new)?;
            let Some(bytes) = self.get(url, MAX_LOOKUP_BYTES).await? else { continue };
            let result: Value = serde_json::from_slice(&bytes).map_err(MetadataClientError::new)?;
            let Some(product) = result.get("product").filter(|product| product.get("asin").and_then(Value::as_str) == Some(asin)) else { continue };
            let image = product.get("product_images").and_then(Value::as_object).into_iter().flatten().filter_map(|(size, value)| Some((size.parse::<u32>().ok()?, value.as_str()?))).max_by_key(|(size, _)| *size);
            if let Some((_, image)) = image {
                let Ok(url) = Url::parse(image) else { continue };
                if url.scheme() == "https" && matches!(url.host_str(), Some("m.media-amazon.com" | "images-na.ssl-images-amazon.com" | "images-eu.ssl-images-amazon.com")) && url.username().is_empty() && url.password().is_none() {
                    if let Some(bytes) = self.image(url).await? {
                        return Ok(Some(bytes));
                    }
                }
            }
        }
        Ok(None)
    }

    async fn image(&self, url: Url) -> Result<Option<Vec<u8>>, MetadataClientError> {
        let Some(bytes) = self.get(url, MAX_IMAGE_BYTES).await? else { return Ok(None) };
        // Validate the header before CPU decoding, including a bound on pixel
        // allocation. Tiny provider placeholders must not become local covers.
        let size = ImageReader::new(Cursor::new(&bytes)).with_guessed_format().ok().and_then(|reader| reader.into_dimensions().ok());
        match size {
            Some((width, height)) if width >= 200 && height >= 200 && u64::from(width) * u64::from(height) <= 40_000_000 => Ok(Some(bytes)),
            _ => Ok(None),
        }
    }

    async fn pace_request(&self) -> Result<(), MetadataClientError> {
        let mut previous = self.requests.lock().await;
        if let Some(last) = *previous {
            let elapsed = last.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            let remaining = self.interval_ms.saturating_sub(elapsed);
            if remaining > 0 {
                executor::sleep(Duration::from_millis(u64::from(remaining))).await.map_err(MetadataClientError::new)?;
            }
        }
        *previous = Some(Instant::now());
        Ok(())
    }

    async fn get(&self, url: Url, limit: usize) -> Result<Option<Vec<u8>>, MetadataClientError> {
        library_runtime::transfers::retry(|| {
            MetadataClient::with_deadline(30_000, async {
                self.pace_request().await?;
                let response = self.http.get(url.clone()).send().await.map_err(|error| MetadataClientError::new(error.without_url()))?;
                if response.status() == StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                let response = response.error_for_status().map_err(|error| MetadataClientError::new(error.without_url()))?;
                MetadataClient::response_bytes(response, limit).await.map(Some)
            })
        })
        .await
    }
}

fn isbns(record: &BookRecord) -> Vec<String> {
    let recording = record.book.identifiers.iter().any(|id| id.scheme() == &Scheme::Asin);
    record.book.identifiers.iter().filter(|id| id.scheme() == &Scheme::Isbn && (id.scope() == Scope::Book || recording)).filter_map(|id| isbn_key(id.value())).collect::<BTreeSet<_>>().into_iter().collect()
}

fn isbn_key(value: &str) -> Option<String> {
    let isbn = canonical_isbn(value)?;
    if isbn.len() == 13 {
        return Some(isbn);
    }
    let mut converted = format!("978{}", &isbn[..9]);
    let sum: u32 = converted.bytes().enumerate().map(|(index, byte)| u32::from(byte - b'0') * if index % 2 == 0 { 1 } else { 3 }).sum();
    converted.push(char::from(b'0' + ((10 - sum % 10) % 10) as u8));
    Some(converted)
}

fn strict_authors_match(local: &[String], remote: &[String]) -> bool {
    let local = author_credits(local);
    let remote = author_credits(remote);
    local.iter().any(|a| remote.iter().any(|b| same_author(a, b)))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
