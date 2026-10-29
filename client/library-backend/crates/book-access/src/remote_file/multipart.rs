//! Bounded parsing for the exact ranges served by the Bokheim endpoint.

use http_response::BodyReadError;
use reqwest::header;

use super::RemoteFileError;

pub(super) async fn read(response: reqwest::Response, file_length: u64, ranges: &[(u64, usize)], length: usize) -> Result<Vec<u8>, RemoteFileError> {
    let content_type = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).ok_or("missing multipart type")?;
    let boundary = boundary(content_type)?.to_owned();
    if response.headers().contains_key(header::CONTENT_RANGE) {
        return Err("unexpected outer multipart range".into());
    }
    let limit = length + ranges.len() * 1024 + 256;
    let bytes = http_response::read(response, limit).await.map_err(|error| match error {
        BodyReadError::Http(error) => RemoteFileError::transport(error),
        BodyReadError::TooLarge { .. } => RemoteFileError::Failed("oversized multipart response".into()),
    })?;
    parse(&bytes, &boundary, file_length, ranges, length)
}

fn boundary(value: &str) -> Result<&str, RemoteFileError> {
    let mut parts = value.split(';');
    if !parts.next().is_some_and(|v| v.trim().eq_ignore_ascii_case("multipart/byteranges")) {
        return Err("invalid multipart type".into());
    }
    let mut boundary = None;
    for part in parts {
        if let Some((name, value)) = part.trim().split_once('=') {
            if name.trim().eq_ignore_ascii_case("boundary") {
                if boundary.is_some() {
                    return Err("duplicate multipart boundary".into());
                }
                let value = value.trim();
                let value = if let Some(quoted) = value.strip_prefix('"') { quoted.strip_suffix('"').ok_or("invalid quoted multipart boundary")? } else { value };
                if value.is_empty() || value.len() > 70 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&b)) {
                    return Err("invalid multipart boundary".into());
                }
                boundary = Some(value);
            }
        }
    }
    boundary.ok_or_else(|| "missing multipart boundary".into())
}

fn parse(bytes: &[u8], boundary: &str, file_length: u64, ranges: &[(u64, usize)], length: usize) -> Result<Vec<u8>, RemoteFileError> {
    let marker = format!("--{boundary}\r\n");
    let mut remaining = bytes;
    let mut output = Vec::with_capacity(length);
    for &(offset, count) in ranges {
        remaining = remaining.strip_prefix(marker.as_bytes()).ok_or("invalid multipart separator")?;
        let header_end = remaining.windows(4).take(1024).position(|v| v == b"\r\n\r\n").ok_or("invalid multipart headers")?;
        let headers = std::str::from_utf8(&remaining[..header_end]).map_err(|_| RemoteFileError::from("invalid multipart headers"))?;
        let expected = format!("bytes {offset}-{}/{}", offset + count as u64 - 1, file_length);
        let mut matched = false;
        for line in headers.split("\r\n") {
            let (name, value) = line.split_once(':').ok_or("invalid multipart header")?;
            if name.eq_ignore_ascii_case("content-range") {
                if matched || value.trim() != expected {
                    return Err("multipart range bounds changed".into());
                }
                matched = true;
            }
        }
        if !matched {
            return Err("missing multipart range".into());
        }
        remaining = &remaining[header_end + 4..];
        let data = remaining.get(..count).ok_or("incomplete multipart range")?;
        output.extend_from_slice(data);
        remaining = remaining[count..].strip_prefix(b"\r\n").ok_or("invalid multipart range length")?;
    }
    let closing = format!("--{boundary}--\r\n");
    if remaining != closing.as_bytes() {
        return Err("invalid multipart ending".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_data_can_contain_boundary_text() {
        let data = b"\0--test\r\n\xff";
        let mut wire = format!("--test\r\nContent-Range: bytes 10-{}/100\r\n\r\n", 10 + data.len() - 1).into_bytes();
        wire.extend_from_slice(data);
        wire.extend_from_slice(b"\r\n--test--\r\n");
        assert_eq!(parse(&wire, "test", 100, &[(10, data.len())], data.len()).unwrap(), data);
        assert!(parse(&wire, "test", 101, &[(10, data.len())], data.len()).is_err());
        assert!(parse(&wire[..wire.len() - 1], "test", 100, &[(10, data.len())], data.len()).is_err());
    }

    #[test]
    fn rejects_extra_duplicate_or_changed_ranges() {
        let wire = b"--b\r\nContent-Range: bytes 1-2/10\r\n\r\nab\r\n--b\r\nContent-Range: bytes 5-6/10\r\n\r\ncd\r\n--b--\r\n";
        assert_eq!(parse(wire, "b", 10, &[(1, 2), (5, 2)], 4).unwrap(), b"abcd");
        for ranges in [vec![(1, 2)], vec![(1, 2), (1, 2)], vec![(1, 2), (6, 2)]] {
            assert!(parse(wire, "b", 10, &ranges, 4).is_err());
        }
        assert_eq!(boundary("multipart/byteranges; boundary=\"b\"").unwrap(), "b");
        assert!(boundary("multipart/byteranges; boundary=b; boundary=c").is_err());
    }
}
