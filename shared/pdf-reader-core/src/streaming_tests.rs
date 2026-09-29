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
    let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for rotation in [0, 90] {
        let (_, metadata, _) = inspect_reader_metadata(Cursor::new(annotated_streaming_fixture(rotation)))?;
        assert!(metadata.valid_for(&metadata.checksum));
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
    let _guard = PDFIUM_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    #[derive(Default)]
    struct Hook(std::sync::Mutex<Vec<Option<usize>>>);
    impl PdfPageSource for Hook {
        fn prepare_startup(&self) -> Result<(), String> { self.0.lock().unwrap().push(None); Ok(()) }
        fn prepare_page(&self, page: usize) -> Result<(), String> { self.0.lock().unwrap().push(Some(page)); Ok(()) }
    }
    let bytes = annotated_streaming_fixture(0);
    let (_, metadata, _) = inspect_reader_metadata(Cursor::new(bytes.clone()))?;
    let checksum = metadata.checksum.clone(); let hook = Arc::new(Hook::default());
    let document = PdfDocumentSession::from_reader_with_page_source("fixture", Cursor::new(bytes), &checksum, Some(metadata), Some(hook.clone()))?;
    assert_eq!(*hook.0.lock().unwrap(), vec![None]);
    document.render_page(0, Some(100))?;
    assert!(hook.0.lock().unwrap().iter().skip(1).all(|page| *page == Some(0)));
    assert!(hook.0.lock().unwrap().len() > 1);
    Ok(())
}
