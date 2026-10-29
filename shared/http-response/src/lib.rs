//! Bounded HTTP body collection. Callers own status, retry, and decoding policy.

use futures_util::StreamExt;

#[derive(Debug)]
pub enum BodyReadError {
    Http(reqwest::Error),
    TooLarge { limit: usize },
}

pub async fn read(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, BodyReadError> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(BodyReadError::TooLarge { limit });
    }
    let mut bytes = Vec::new();
    let stream = response.bytes_stream();
    futures_util::pin_mut!(stream);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(BodyReadError::Http)?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(BodyReadError::TooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
