use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;
use wire::{self as wire, WireError};

fn content_type(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::CONTENT_TYPE).and_then(|value| value.to_str().ok())
}

fn accepts_wire(headers: &HeaderMap) -> bool {
    headers.get_all(header::ACCEPT).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(',')).any(|candidate| {
        let disabled = candidate
            .split(';')
            .skip(1)
            .any(|parameter| parameter.trim().split_once('=').filter(|(name, _)| name.trim().eq_ignore_ascii_case("q")).and_then(|(_, quality)| quality.trim().parse::<f32>().ok()).is_some_and(|quality| quality <= 0.0));
        wire::is_current_media_type(candidate) && !disabled
    })
}

fn content_encoding_supported(headers: &HeaderMap) -> bool {
    match headers.get(header::CONTENT_ENCODING).and_then(|value| value.to_str().ok()).map(str::trim) {
        None | Some("") => true,
        Some(value) if value.eq_ignore_ascii_case("identity") => true,
        Some(_) => false,
    }
}

fn wire_decode_status(error: WireError) -> StatusCode {
    match error {
        WireError::UnsupportedVersion(_) => StatusCode::UPGRADE_REQUIRED,
        WireError::TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        WireError::Encode(_) => StatusCode::INTERNAL_SERVER_ERROR,
        WireError::Decode(_) | WireError::InvalidHeader => StatusCode::BAD_REQUEST,
    }
}

pub fn decode_request<T: DeserializeOwned>(headers: &HeaderMap, body: &[u8], decoded_limit: usize) -> Result<T, StatusCode> {
    if !content_encoding_supported(headers) {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    match content_type(headers) {
        Some(value) if wire::is_current_media_type(value) => wire::decode(body, decoded_limit).map_err(wire_decode_status),
        Some(value) if value.split(';').next().is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(wire::MEDIA_TYPE_BASE)) => Err(StatusCode::UPGRADE_REQUIRED),
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

pub fn decode_optional_request<T: DeserializeOwned>(headers: &HeaderMap, body: &[u8], decoded_limit: usize) -> Result<Option<T>, StatusCode> {
    if body.is_empty() {
        Ok(None)
    } else {
        decode_request(headers, body, decoded_limit).map(Some)
    }
}

pub fn response<T: Serialize>(request_headers: &HeaderMap, value: T) -> Result<Response, StatusCode> {
    if !accepts_wire(request_headers) {
        return Err(StatusCode::NOT_ACCEPTABLE);
    }

    let encoded = wire::encode(&value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut response = encoded.into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(wire::MEDIA_TYPE));
    response.headers_mut().insert(header::VARY, HeaderValue::from_static("Accept"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Example {
        values: Vec<u64>,
    }

    #[tokio::test]
    async fn only_the_versioned_binary_media_type_crosses_http() {
        let original = Example { values: (0..1_000).collect() };
        let encoded = wire::encode(&original).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(wire::MEDIA_TYPE));
        assert_eq!(decode_request::<Example>(&headers, &encoded, wire::MAX_DECODED_REQUEST_BYTES).unwrap(), original);

        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.remove(header::CONTENT_ENCODING);
        assert!(matches!(decode_request::<Example>(&headers, b"{}", wire::MAX_DECODED_REQUEST_BYTES), Err(StatusCode::UNSUPPORTED_MEDIA_TYPE)));

        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(wire::MEDIA_TYPE_BASE));
        assert!(matches!(decode_request::<Example>(&headers, &encoded, wire::MAX_DECODED_REQUEST_BYTES), Err(StatusCode::UPGRADE_REQUIRED)));

        assert!(matches!(response(&HeaderMap::new(), &original), Err(StatusCode::NOT_ACCEPTABLE)));
        let mut response_headers = HeaderMap::new();
        response_headers.insert(header::ACCEPT, HeaderValue::from_static(wire::MEDIA_TYPE));
        let response = response(&response_headers, &original).unwrap();
        assert_eq!(response.headers()[header::CONTENT_TYPE], wire::MEDIA_TYPE);
        let bytes = to_bytes(response.into_body(), wire::MAX_DECODED_RESPONSE_BYTES).await.unwrap();
        assert_eq!(wire::decode::<Example>(&bytes, wire::MAX_DECODED_RESPONSE_BYTES).unwrap(), original);
    }
}
