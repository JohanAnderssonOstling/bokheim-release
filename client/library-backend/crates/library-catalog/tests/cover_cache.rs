use library_model::CardKey;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant},
};

#[tokio::test]
async fn cover_files_share_fallbacks_retry_failures_and_refresh_together() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    std::env::set_var("BOKHEIM_CATALOG_ORIGIN", format!("http://{}", listener.local_addr().unwrap()));
    let key = CardKey([170; 32]);
    let missing = CardKey([171; 32]);
    let retry = CardKey([172; 32]);
    let browse = thumbnail::card_thumbnail_suffix(300);
    let dense = thumbnail::card_thumbnail_suffix(600);
    let replies = [
        (key, browse.as_str(), 404, ""),
        (key, "card", 503, "temporary failure"),
        (key, "detail", 200, "shared master"),
        (key, "card", 200, "legacy card recovered"),
        (key, dense.as_str(), 404, ""),
        (key, "detail", 200, "fresh master"),
        (key, browse.as_str(), 200, "fresh card"),
        (key, dense.as_str(), 404, ""),
        (key, "card", 200, "fresh legacy card"),
        (missing, browse.as_str(), 404, ""),
        (missing, "card", 404, ""),
        (missing, "detail", 404, ""),
        (retry, browse.as_str(), 503, "temporary failure"),
        (retry, browse.as_str(), 200, "versioned card recovered"),
    ]
    .map(|(key, suffix, status, body)| (format!("/covers/{key}-{suffix}.jpg"), status, body));
    let server = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        for (path, status, body) in replies {
            let started = Instant::now();
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && started.elapsed() < Duration::from_secs(5) => thread::sleep(Duration::from_millis(1)),
                    Err(error) => panic!("expected {path}: {error}"),
                }
            };
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut socket = BufReader::new(socket);
            let mut line = String::new();
            socket.read_line(&mut line).unwrap();
            assert_eq!(line, format!("GET {path} HTTP/1.1\r\n"));
            loop {
                line.clear();
                assert_ne!(socket.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            write!(socket.get_mut(), "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });

    assert!(library_catalog::card_cover(key, 0).await.is_none());
    assert_eq!(library_catalog::card_cover(key, 300).await.unwrap(), b"shared master");
    // A failed legacy request remains retryable even if its detail fallback worked.
    assert_eq!(library_catalog::card_cover(key, 300).await.unwrap(), b"legacy card recovered");
    assert_eq!(library_catalog::card_cover(key, 300).await.unwrap(), b"legacy card recovered");
    assert_eq!(library_catalog::external_cover(key, false).await.unwrap(), b"legacy card recovered");
    assert_eq!(library_catalog::card_cover(key, 600).await.unwrap(), b"shared master");
    assert_eq!(library_catalog::external_cover(key, true).await.unwrap(), b"shared master");

    assert_eq!(library_catalog::fresh_external_cover(key).await.unwrap().unwrap(), b"fresh master");
    assert_eq!(library_catalog::external_cover(key, true).await.unwrap(), b"fresh master");
    assert_eq!(library_catalog::card_cover(key, 300).await.unwrap(), b"fresh card");
    assert_eq!(library_catalog::card_cover(key, 600).await.unwrap(), b"fresh master");
    assert_eq!(library_catalog::external_cover(key, false).await.unwrap(), b"fresh legacy card");
    for _ in 0..2 {
        assert!(library_catalog::card_cover(missing, 300).await.is_none());
    }
    assert!(library_catalog::card_cover(retry, 300).await.is_none());
    assert_eq!(library_catalog::card_cover(retry, 300).await.unwrap(), b"versioned card recovered");
    assert_eq!(library_catalog::card_cover(retry, 300).await.unwrap(), b"versioned card recovered");
    server.join().unwrap();
}
