#![cfg(not(target_arch = "wasm32"))]

use book_access::{
    remote_file::{fetch_ranges, RemoteFile},
    BookId, RemoteFileError,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

const WIRE: &[u8] = b"--b\r\nContent-Range: bytes 1-2/10\r\n\r\n\0\xff\r\n--b\r\nContent-Range: bytes 5-6/10\r\n\r\ncd\r\n--b--\r\n";
const BODY_LIMIT: usize = 4 + 2 * 1024 + 256;

async fn fetch(framing: &str, body: Vec<u8>) -> Result<Vec<u8>, RemoteFileError> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/book?token=fixture-private-token", listener.local_addr().unwrap()).parse().unwrap();
    let hash = BookId::from_bytes([42; 32]);
    let headers = format!("HTTP/1.1 206 Partial Content\r\nETag: \"{hash}\"\r\nContent-Type: multipart/byteranges; boundary=b\r\n{framing}\r\nConnection: close\r\n\r\n");
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
        }
        assert!(String::from_utf8_lossy(&request).to_ascii_lowercase().contains("range: bytes=1-2,5-6\r\n"));
        let _ = socket.write_all(&[headers.as_bytes(), &body].concat());
    });
    let client = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
    let source = RemoteFile { hash, length: 10, revision: None };
    let result = fetch_ranges(&client, url, &source, "fixture", vec![(1, 2), (5, 2)]).await;
    server.join().unwrap();
    result
}

fn chunked(bytes: &[u8], size: usize) -> Vec<u8> {
    let mut body = Vec::new();
    for chunk in bytes.chunks(size) {
        body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        body.extend_from_slice(chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    body
}

#[tokio::test]
async fn multipart_ranges_preserve_binary_payload_across_stream_chunks() {
    assert_eq!(fetch("Transfer-Encoding: chunked", chunked(WIRE, 3)).await.unwrap(), b"\0\xffcd");
    assert_eq!(fetch(&format!("Content-Length: {}", WIRE.len()), WIRE.to_vec()).await.unwrap(), b"\0\xffcd");
}

#[tokio::test]
async fn multipart_limits_apply_to_advertised_lengths_and_streamed_bytes() {
    // With no body, a missing preflight check would report a transport error.
    let advertised = fetch(&format!("Content-Length: {}", BODY_LIMIT + 1), Vec::new()).await.unwrap_err();
    let streamed = fetch("Transfer-Encoding: chunked", chunked(&vec![b'x'; BODY_LIMIT + 1], 128)).await.unwrap_err();
    for error in [advertised, streamed] {
        assert!(matches!(error, RemoteFileError::Failed(ref message) if message == "oversized multipart response"), "{error}");
    }
}

#[tokio::test]
async fn interrupted_multipart_bodies_are_retryable_without_exposing_the_url() {
    let error = fetch(&format!("Content-Length: {}", WIRE.len() + 1), WIRE.to_vec()).await.unwrap_err();
    assert!(matches!(error, RemoteFileError::Retryable(_)), "{error}");
    assert!(!error.to_string().contains("fixture-private-token"));
    assert!(!error.to_string().contains("http://"));
}
