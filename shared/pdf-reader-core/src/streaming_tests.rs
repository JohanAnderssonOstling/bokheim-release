fn annotated_streaming_fixture(rotation: u16) -> Vec<u8> {
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [20 40 580 760] /Rotate {rotation} /Resources << >> /Annots [4 0 R 5 0 R 6 0 R 7 0 R] >>"),
        "<< /Type /Annot /Subtype /Highlight /Rect [60 600 240 632] /QuadPoints [60 632 240 632 60 600 240 600] /C [1 0 0] /Contents (Imported highlight) >>".to_owned(),
        "<< /Type /Annot /Subtype /Text /Rect [250 600 270 620] /Contents (Imported note) >>".to_owned(),
        "<< /Type /Annot /Subtype /Square /Rect [300 600 400 700] /Contents (Complex annotation) >>".to_owned(),
        "<< /Type /Annot /Subtype /Highlight /Rect [60 500 240 532] /NM (bokheim:owned) /Contents (App-owned mark) >>".to_owned(),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec(); let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len()); bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len(); bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets { bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()); }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes()); bytes
}

#[test]
fn ingestion_keeps_simple_annotations_in_rotated_cropped_display_coordinates() -> PdfResult<()> {
    for rotation in [0, 90] {
        let (_, metadata, _) = inspect_reader_metadata(Cursor::new(annotated_streaming_fixture(rotation)))?;
        assert!(metadata.valid_for(&metadata.content_hash));
        assert_eq!(metadata.annotations.len(), 2, "complex and app-owned annotations must be omitted");
        assert_eq!(metadata.annotations[0].note, "Imported highlight");
        assert_eq!(metadata.annotations[1].kind, PdfStoredAnnotationKind::Note);
        assert_eq!(metadata.annotations[1].note, "Imported note");
        let rect = metadata.annotations[0].rects[0];
        let expected = if rotation == 0 { [40.0 / 560.0, 128.0 / 720.0, 180.0 / 560.0, 32.0 / 720.0] } else { [560.0 / 720.0, 40.0 / 560.0, 32.0 / 720.0, 180.0 / 560.0] };
        for (actual, expected) in rect.into_iter().zip(expected) { assert!((actual - expected).abs() < 0.001, "rotation={rotation}, rect={rect:?}, actual={actual}, expected={expected}"); }
    }
    Ok(())
}

#[test]
fn document_prepares_startup_then_only_requested_pages() -> PdfResult<()> {
    #[derive(Default)]
    struct Hook(std::sync::Mutex<Vec<Option<usize>>>);
    impl PdfPageSource for Hook {
        fn prepare_startup(&self) -> Result<(), String> { self.0.lock().unwrap().push(None); Ok(()) }
        fn prepare_page(&self, page: usize) -> Result<(), String> { self.0.lock().unwrap().push(Some(page)); Ok(()) }
    }
    let bytes = annotated_streaming_fixture(0);
    let (_, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
    let content_hash = metadata.content_hash.clone(); let hook = Arc::new(Hook::default());
    let document = PdfDocumentSession::from_reader_with_page_source("fixture", Cursor::new(bytes), &content_hash, Some(metadata), Some(hook.clone()))?;
    assert_eq!(*hook.0.lock().unwrap(), vec![None]);
    document.render_page(0, Some(100))?;
    assert!(hook.0.lock().unwrap().iter().skip(1).all(|page| *page == Some(0)));
    assert!(hook.0.lock().unwrap().len() > 1);
    Ok(())
}

#[test]
fn document_batches_opening_and_clamps_stale_saved_page() -> PdfResult<()> {
    #[derive(Default)]
    struct Hook(std::sync::Mutex<Vec<(&'static str, usize)>>);
    impl PdfPageSource for Hook {
        fn prepare_startup(&self) -> Result<(), String> { self.0.lock().unwrap().push(("startup", 0)); Ok(()) }
        fn prepare_page(&self, page: usize) -> Result<(), String> { self.0.lock().unwrap().push(("page", page)); Ok(()) }
        fn prepare_opening(&self, page: usize) -> Result<(), String> { self.0.lock().unwrap().push(("opening", page)); Ok(()) }
    }
    let bytes = annotated_streaming_fixture(0);
    let (_, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
    let content_hash = metadata.content_hash.clone();
    let hook = Arc::new(Hook::default());
    let document = PdfDocumentSession::from_reader_with_opening_page("fixture", Cursor::new(bytes), &content_hash, Some(metadata), Some(hook.clone()), Some(99))?;
    assert_eq!(*hook.0.lock().unwrap(), vec![("opening", 0)]);
    document.render_page(0, Some(100))?;
    assert!(hook.0.lock().unwrap().iter().skip(1).all(|call| *call == ("page", 0)));
    Ok(())
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn dropping_prefetch_future_cancels_work_already_running_on_the_pdf_worker() -> PdfResult<()> {
    use std::{
        sync::{Mutex, mpsc},
        task::{Context, Poll, Waker},
        time::Duration,
    };

    struct Hook {
        started: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        stopped: mpsc::SyncSender<bool>,
    }
    impl PdfPageSource for Hook {
        fn prepare_startup(&self) -> Result<(), String> { Ok(()) }
        fn prepare_page(&self, _: usize) -> Result<(), String> { Ok(()) }
        fn prefetch_pages_cancellable(&self, _: &[usize], cancellation: &PdfPrefetchCancellation) -> Result<(), String> {
            assert!(!cancellation.is_cancelled());
            self.started.send(()).unwrap();
            self.release.lock().unwrap().recv_timeout(Duration::from_secs(10)).unwrap();
            self.stopped.send(cancellation.is_cancelled()).unwrap();
            Ok(())
        }
    }
    let (started, wait_started) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::channel();
    let (stopped, wait_stopped) = mpsc::sync_channel(1);
    let bytes = annotated_streaming_fixture(0);
    let (_, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
    let hash = metadata.content_hash.clone();
    let hook = Arc::new(Hook { started, release: Mutex::new(wait_release), stopped });
    let document = PdfDocumentSession::from_reader_with_page_source("cancel-prefetch", Cursor::new(bytes), &hash, Some(metadata), Some(hook))?;
    let mut future = Box::pin(document.prefetch_pages(vec![0]));
    assert!(matches!(future.as_mut().poll(&mut Context::from_waker(Waker::noop())), Poll::Pending));
    wait_started.recv_timeout(Duration::from_secs(10)).unwrap();
    drop(future);
    release.send(()).unwrap();
    assert!(wait_stopped.recv_timeout(Duration::from_secs(10)).unwrap(), "dropping the async request must be visible inside the running synchronous worker");
    document.render_page(0, Some(100))?;
    Ok(())
}

#[test]
fn prefetch_window_scales_with_settings_and_prioritizes_visible_pages() {
    let mut info = PdfDocumentInfo { source_name: "window".into(), pages: vec![PdfPageInfo { width_points: 600.0, height_points: 800.0 }; 30], outline: vec![], skippable_pages: vec![], right_to_left: false };
    assert_eq!(info.prefetch_window(10, &[10], 1), vec![10, 11]);
    assert_eq!(info.prefetch_window(10, &[10, 11], 2), vec![10, 11, 12, 9]);
    assert_eq!(info.prefetch_window(10, &[10, 11, 12, 13], 4), vec![10, 11, 12, 13, 14, 15, 16, 9]);
    assert_eq!(info.prefetch_window(10, &[10, 11, 12, 13, 14, 15], 6).len(), 12);
    assert_eq!(info.prefetch_window(0, &[0, 1, 2, 3], 4), vec![0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(info.prefetch_window(29, &[29], 4), vec![29, 28, 27, 26, 25, 24, 23, 22]);
    info.skippable_pages = vec![9, 12, 14, 16];
    assert_eq!(info.prefetch_window(10, &[10, 11, 13, 15], 4), vec![10, 11, 13, 15, 17, 18, 19, 8]);
    assert!(info.prefetch_window(30, &[], 4).is_empty());
    info.pages.truncate(3);
    info.skippable_pages.clear();
    assert_eq!(info.prefetch_window(0, &[0], 4), vec![0, 1, 2]);
    info.pages.clear();
    assert!(info.prefetch_window(0, &[], 4).is_empty());
}
