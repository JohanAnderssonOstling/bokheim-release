use library_model::CardKey;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::{sync::mpsc, thread};
use tokio::sync::oneshot;

#[tokio::test]
async fn eviction_rejects_an_old_response_and_allows_fresh_covers_after_reading() {
    for (index, (width, fallback)) in [(None, false), (Some(300), false), (Some(600), false), (Some(300), true), (Some(600), true)].into_iter().enumerate() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        std::env::set_var("BOKHEIM_CATALOG_ORIGIN", format!("http://{}", listener.local_addr().unwrap()));
        let key = CardKey([93 + index as u8; 32]);
        let suffix = width.map(thumbnail::card_thumbnail_suffix).unwrap_or_else(|| "card".into());
        let (started, received) = oneshot::channel();
        let (resume, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut started = Some(started);
            for body in [b"old".as_slice(), b"fresh", b"after reading"] {
                if fallback {
                    let mut stream = request(&listener, key, &suffix);
                    stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
                let selected = if fallback {
                    if width == Some(300) {
                        "card"
                    } else {
                        "detail"
                    }
                } else {
                    &suffix
                };
                let mut stream = request(&listener, key, selected);
                if let Some(started) = started.take() {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(body).unwrap();
            }
        });

        let (old, ()) = tokio::join!(cover(key, width), async {
            received.await.unwrap();
            library_catalog::evict_cover_caches();
            resume.send(()).unwrap();
        });
        assert_eq!(old.as_deref(), Some(b"old".as_slice()));
        assert_eq!(cover(key, width).await.as_deref(), Some(b"fresh".as_slice()));
        assert_eq!(cover(key, width).await.as_deref(), Some(b"fresh".as_slice()), "a new request can cache normally");
        library_catalog::evict_cover_caches();
        assert_eq!(cover(key, width).await.as_deref(), Some(b"after reading".as_slice()));
        server.join().unwrap();
    }
}

fn request(listener: &TcpListener, key: CardKey, suffix: &str) -> TcpStream {
    let (mut stream, _) = listener.accept().unwrap();
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0);
        request.extend_from_slice(&buffer[..read]);
    }
    assert!(String::from_utf8_lossy(&request).starts_with(&format!("GET /covers/{key}-{suffix}.jpg ")));
    stream
}

async fn cover(key: CardKey, width: Option<u32>) -> Option<Vec<u8>> {
    match width {
        Some(width) => library_catalog::card_cover(key, width).await,
        None => library_catalog::external_cover(key, false).await,
    }
}
