//! Request counts and exact contents against the production generic HTTP handler.

use std::{
    io::{Cursor, Read, Seek, SeekFrom, Write},
    sync::Arc,
};

use axum::{routing::get, Router};
use book_access::{remote_file, AsyncBookSource, FetchFuture, RemoteFile, RemoteFileError, SparseBookSource};
use book_content::comic::ComicBook;
use sync_common::BookId;

use super::opening::{serve, Fixture};

async fn server(bytes: Vec<u8>, legacy: bool) -> (Fixture, reqwest::Client, reqwest::Url, tokio::task::JoinHandle<()>) {
    let fixture = Fixture { bytes: bytes.into(), requests: Arc::default(), hash: BookId::new(&"d".repeat(64)), legacy, delay_ms: 0 };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/book", listener.local_addr().unwrap()).parse().unwrap();
    let app = Router::new().route("/book", get(serve)).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (fixture, reqwest::Client::new(), url, task)
}

fn cbz(deleted: bool) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // Metadata and the requested page are separate spans; natural sort differs
    // from storage order and lexical order. Large stored pages expose read-ahead.
    let info = if deleted { "<ComicInfo><Title>Remote comic</Title><Pages><Page Image='0' Type='Delete'/></Pages></ComicInfo>" } else { "<ComicInfo><Title>Remote comic</Title><Manga>YesAndRightToLeft</Manga></ComicInfo>" };
    for (name, bytes) in [("ComicInfo.xml", info.as_bytes().to_vec()), ("page10.jpg", vec![10; 512 * 1024]), ("page1.jpg", vec![1; 512 * 1024]), ("page2.jpg", vec![2; 512 * 1024]), ("padding.bin", vec![9; 128 * 1024])] {
        archive.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        archive.write_all(&bytes).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

async fn comic_source(client: reqwest::Client, url: reqwest::Url, file: RemoteFile, seed: Vec<u8>, optimized: bool) -> AsyncBookSource {
    let (read_client, read_url, read_file) = (client.clone(), url.clone(), file.clone());
    let read = move |offset, count| {
        let (client, url, file) = (read_client.clone(), read_url.clone(), read_file.clone());
        Box::pin(async move { remote_file::fetch_range(&client, url, &file, "test", offset, count).await }) as FetchFuture<'static, Result<Vec<u8>, RemoteFileError>>
    };
    if !optimized {
        return AsyncBookSource::remote(file.length, seed, read).unwrap();
    }
    let source = SparseBookSource::remote(file.length, file.length - seed.len() as u64, seed, read)
        .unwrap()
        .with_remote_prefetch(move |ranges| {
            let (client, url, file) = (client.clone(), url.clone(), file.clone());
            Box::pin(async move { remote_file::fetch_ranges(&client, url, &file, "test", ranges).await })
        })
        .unwrap();
    source.into()
}

async fn comic_opening(optimized: bool, deleted: bool, legacy: bool) -> Vec<(String, u64)> {
    let (fixture, client, url, server) = server(cbz(deleted), legacy).await;
    let (file, seed) = if optimized { remote_file::describe_tail(&client, url.clone(), "test", fixture.hash).await.unwrap() } else { remote_file::describe(&client, url.clone(), "test", fixture.hash).await.unwrap() };
    let source = comic_source(client, url, file, seed, optimized).await;
    let prefetch = source.clone();
    let book = ComicBook::open_with_prefetch(
        move || source.reader(),
        1,
        move |ranges| {
            let source = prefetch.clone();
            async move { source.prefetch(ranges).await }
        },
    )
    .await
    .unwrap();
    assert_eq!(book.metadata.title, "Remote comic");
    assert_eq!(book.right_to_left, !deleted);
    let reader = book.into_reader().await.unwrap();
    let expected = if deleted { 10 } else { 2 };
    assert_eq!(reader.read_page(1).await.unwrap(), vec![expected; 512 * 1024]);
    let opening_count = fixture.requests.lock().unwrap().len();
    if optimized {
        assert_eq!(reader.read_page(1).await.unwrap(), vec![expected; 512 * 1024]);
    }
    if optimized {
        assert_eq!(fixture.requests.lock().unwrap().len(), opening_count, "repeated page read must use shared bytes");
    }
    // A later page is one exact record request and is retained for rereads.
    if optimized && !deleted && !legacy {
        assert_eq!(opening_count, 2);
        assert_eq!(reader.read_page(2).await.unwrap(), vec![10; 512 * 1024]);
        assert_eq!(fixture.requests.lock().unwrap().len(), opening_count + 1);
    }
    server.abort();
    let requests = fixture.requests.lock().unwrap().clone();
    requests
}

#[tokio::test]
async fn cbz_batches_metadata_and_resumed_page_then_caches_page_reads() {
    let baseline = comic_opening(false, false, false).await;
    let optimized = comic_opening(true, false, false).await;
    assert_eq!(optimized.len(), 3, "opening is 2 requests, later page adds 1: {optimized:?}");
    assert_eq!(optimized[0].0, "bytes=-65557");
    assert!(optimized[1].0.contains(','));
    assert!(baseline.len() > optimized.len(), "baseline={baseline:?}, optimized={optimized:?}");
    eprintln!("CBZ: baseline {} opening requests, optimized 2 opening + 1 later page", baseline.len());
}

#[tokio::test]
async fn cbz_deleted_pages_and_legacy_servers_preserve_page_mapping() {
    let deleted = comic_opening(true, true, false).await;
    assert_eq!(deleted.len(), 3, "metadata remaps the requested visible page");
    let legacy = comic_opening(true, false, true).await;
    assert!(legacy.len() > 2);
}

#[cfg(feature = "pdf")]
#[tokio::test]
async fn pdf_combines_startup_and_resumed_page_in_one_http_batch() {
    use book_access::pdf_bundle;
    use pdf_view_common::{PdfByteRange, PdfDependencyIndex};

    const BLOCK: usize = 64 * 1024;
    let bytes: Vec<_> = (0..12 * BLOCK).map(|index| (index / BLOCK) as u8).collect();
    for optimized in [false, true] {
        let (fixture, client, url, server) = server(bytes.clone(), false).await;
        let (file, prefix) = remote_file::describe(&client, url.clone(), "test", fixture.hash).await.unwrap();
        let range = |block| PdfByteRange { offset: block * BLOCK as u64, length: BLOCK as u64 };
        let index = PdfDependencyIndex { length: file.length, startup: vec![range(1), range(3)], pages: vec![vec![range(5)], vec![range(3), range(7), range(9)]] };
        let fetch = remote_file::native_bundle_fetch(move |ranges| {
            let (client, url, file) = (client.clone(), url.clone(), file.clone());
            Box::pin(async move {
                if optimized {
                    remote_file::fetch_ranges(&client, url, &file, "test", ranges).await
                } else {
                    remote_file::fetch_bundle(&client, url, &file, "test", ranges).await
                }
            })
        })
        .unwrap();
        let (mut reader, hook) = pdf_bundle::open(index, prefix, fetch).unwrap();
        tokio::task::spawn_blocking(move || {
            if optimized {
                hook.prepare_opening(1).unwrap();
            } else {
                hook.prepare_startup().unwrap();
                hook.prepare_page(1).unwrap();
            }
            hook.prepare_startup().unwrap();
            hook.prepare_page(1).unwrap();
            for block in [1, 3, 7, 9] {
                reader.seek(SeekFrom::Start((block * BLOCK) as u64 + 13)).unwrap();
                let mut bytes = [0; 17];
                reader.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, [block as u8; 17]);
            }
            // Unindexed demand reads remain usable and share the same cache.
            reader.seek(SeekFrom::Start(11 * BLOCK as u64)).unwrap();
            reader.read_exact(&mut [0; 1]).unwrap();
        })
        .await
        .unwrap();
        server.abort();
        let calls = fixture.requests.lock().unwrap();
        assert_eq!(calls.len(), if optimized { 3 } else { 6 }, "prefix + opening + demand miss: {calls:?}");
        if optimized {
            assert!(calls[1].0.contains(','));
        }
        assert!(!calls[1].0.contains(&format!("{}-", 5 * BLOCK)), "unopened page excluded");
        eprintln!("PDF: optimized={optimized}, {} opening requests, cached startup/page reads, 1 later demand request", calls.len() - 1);
    }
}

#[cfg(feature = "pdf")]
#[tokio::test]
async fn unindexed_pdf_streams_large_demand_reads_over_http() {
    use book_access::pdf_bundle;

    const BLOCK: usize = 64 * 1024;
    let length = 8 * 1024 * 1024 + 13;
    let bytes = (0..length).map(|index| (index % 251) as u8).collect();
    let (fixture, client, url, server) = server(bytes, false).await;
    let (file, prefix) = remote_file::describe(&client, url.clone(), "test", fixture.hash).await.unwrap();
    let fetch = remote_file::native_bundle_fetch(move |ranges| {
        let (client, url, file) = (client.clone(), url.clone(), file.clone());
        Box::pin(async move { remote_file::fetch_ranges(&client, url, &file, "test", ranges).await })
    })
    .unwrap();
    let mut reader = pdf_bundle::open_unindexed(length as u64, prefix, fetch).unwrap();
    tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0; BLOCK];
        let mut position = 0;
        while position < length {
            let count = BLOCK.min(length - position);
            reader.read_exact(&mut buffer[..count]).unwrap();
            assert!(buffer[..count].iter().enumerate().all(|(index, byte)| *byte == ((position + index) % 251) as u8));
            position += count;
        }
        reader.seek(SeekFrom::Start(BLOCK as u64)).unwrap();
        reader.read_exact(&mut buffer).unwrap();
    })
    .await
    .unwrap();
    server.abort();
    let calls = fixture.requests.lock().unwrap();
    assert_eq!(calls.len(), 9, "one prefix plus eight adaptive demand requests: {calls:?}");
    assert!(calls.iter().any(|(_, bytes)| *bytes == 4 * 1024 * 1024), "sequential demand should reach a 4 MiB HTTP request");
    assert_eq!(calls.iter().map(|(_, bytes)| *bytes).sum::<u64>(), length as u64, "every file byte transfers once, including the cached prefix");
}
