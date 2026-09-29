#![cfg(target_arch = "wasm32")]
use pdf_reader_core::{
    PageRenderKey, PdfDocumentSession,
    browser::{BrowserSession, document_count},
};
use wasm_bindgen::prelude::*;

// A 64 MiB virtual source backed by only the tiny fixture and its trailing
// startxref. A full-file loader fails the read budget instead of allocating it.
struct SparsePdf {
    bytes: Vec<u8>,
    position: u64,
    reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl std::io::Read for SparsePdf {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        const SIZE: u64 = 64 * 1024 * 1024;
        let n = out.len().min((SIZE - self.position.min(SIZE)) as usize).min(997);
        if self.reads.fetch_add(n, std::sync::atomic::Ordering::Relaxed) + n > 1024 * 1024 {
            return Err(std::io::Error::other("PDF source exceeded 1 MiB read budget"));
        }
        let end_start = SIZE - self.bytes.len() as u64;
        for (i, byte) in out[..n].iter_mut().enumerate() {
            let at = self.position + i as u64;
            *byte = if at < self.bytes.len() as u64 {
                self.bytes[at as usize]
            } else if at >= end_start {
                self.bytes[(at - end_start) as usize]
            } else {
                b' '
            };
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl std::io::Seek for SparsePdf {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        let n = match from {
            std::io::SeekFrom::Start(n) => i128::from(n),
            std::io::SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            std::io::SeekFrom::End(n) => 64 * 1024 * 1024 + i128::from(n),
        };
        self.position = u64::try_from(n).map_err(std::io::Error::other)?;
        Ok(self.position)
    }
}

#[wasm_bindgen]
pub async fn check_pdf(bytes: Vec<u8>) -> Result<String, JsValue> {
    console_error_panic_hook::set_once();
    async fn check(bytes: Vec<u8>) -> Result<String, String> {
        let initial = document_count().await.map_err(|e| e.to_string())?;
        let cancelled_bytes = bytes.clone();
        let mut cancelled = Box::pin(BrowserSession::load(move || PdfDocumentSession::from_bytes("cancelled", cancelled_bytes)));
        // Start the request, then drop it without waiting for the worker response.
        std::future::poll_fn(|cx| {
            use std::future::Future;
            let _ = cancelled.as_mut().poll(cx);
            std::task::Poll::Ready(())
        })
        .await;
        drop(cancelled);
        assert_eq!(document_count().await.map_err(|e| e.to_string())?, initial);
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = reads.clone();
        let session = BrowserSession::load(move || PdfDocumentSession::from_reader("browser fixture", SparsePdf { bytes, position: 0, reads: counter })).await.map_err(|e| e.to_string())?;
        assert_eq!(session.info().page_count(), 2);
        assert_eq!(document_count().await.map_err(|e| e.to_string())?, initial + 1);
        for page_index in 0..2 {
            let rendered = session.render(PageRenderKey::with_margin_trim(page_index, 600, false)).await.map_err(|e| e.to_string())?;
            assert_eq!(rendered.pixel_width(), 600);
            assert!(rendered.pixel_height() > 0);
            assert!(rendered.rgba().chunks_exact(4).any(|pixel| pixel[0] < 100 && pixel[1] < 100 && pixel[2] < 100));
            let colored = rendered.rgba().chunks_exact(4).filter(|pixel| if page_index == 0 { pixel[0] > 150 && pixel[1] < 80 && pixel[2] < 80 } else { pixel[2] > 150 && pixel[0] < 80 && pixel[1] < 100 }).count();
            assert!(colored > 10000, "wrong PDF raster color on page {page_index}: {colored} expected pixels");
            let (rgba, width, height, crop, layout, _) = rendered.into_parts();
            let display = session.render_for_display(PageRenderKey::with_margin_trim(page_index, 600, false)).await.map_err(|e| e.to_string())?;
            let (bgra, display_width, display_height, display_crop, _, _) = display.into_parts();
            assert_eq!((width, height, crop), (display_width, display_height, display_crop));
            for (rgba, bgra) in rgba.chunks_exact(4).zip(bgra.chunks_exact(4)) {
                assert_eq!(bgra, [rgba[2], rgba[1], rgba[0], rgba[3]]);
            }
            let text: String = layout.geometry().characters().iter().map(|character| character.character()).collect();
            assert!(text.contains(if page_index == 0 { "First searchable page" } else { "Second searchable page" }), "{text}");
            let zoomed = session.render(PageRenderKey::with_margin_trim(page_index, 900, false)).await.map_err(|e| e.to_string())?;
            let (_, _, _, _, zoomed_layout, _) = zoomed.into_parts();
            assert!(std::sync::Arc::ptr_eq(&layout, &zoomed_layout), "zoom must reuse full-page text analysis across the worker boundary");
            for width in [600, 900] {
                let trimmed = session.render(PageRenderKey::with_margin_trim(page_index, width, true)).await.map_err(|e| e.to_string())?;
                let (_, _, _, _, trimmed_layout, _) = trimmed.into_parts();
                let trimmed_text: String = trimmed_layout.geometry().characters().iter().map(|character| character.character()).collect();
                assert_eq!(text, trimmed_text, "trimming must preserve extracted text at every resolution");
            }
            let characters = layout.geometry().characters();
            let first = characters.first().unwrap().bounds();
            let last = characters.iter().rev().find(|character| !character.character().is_whitespace()).unwrap().bounds();
            let selection = layout.select(((first.left() + first.right()) / 2.0, (first.top() + first.bottom()) / 2.0), ((last.left() + last.right()) / 2.0, (last.top() + last.bottom()) / 2.0)).unwrap();
            assert!(selection.text().contains("searchable"), "selection: {:?}; first: {first:?}, last: {last:?}", selection.text());
        }
        let retained = session.clone();
        drop(session);
        assert_eq!(document_count().await.map_err(|e| e.to_string())?, initial + 1);
        let hits = retained.search(0..2, "searchable".into()).await.map_err(|e| e.to_string())?;
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page_index(), 0);
        assert_eq!(hits[1].page_index(), 1);
        drop(retained);
        assert_eq!(document_count().await.map_err(|e| e.to_string())?, initial);
        assert!(BrowserSession::load(|| PdfDocumentSession::from_bytes("invalid", b"not a PDF".to_vec())).await.is_err());
        assert_eq!(document_count().await.map_err(|e| e.to_string())?, initial);
        assert!(reads.load(std::sync::atomic::Ordering::Relaxed) < 1024 * 1024);
        Ok("PASS: sparse 64 MiB source with <1 MiB reads, open, two page rasters, text selection, search, shared-session disposal, cancellation, invalid PDF".into())
    }
    check(bytes).await.map_err(|e| JsValue::from_str(&e))
}
