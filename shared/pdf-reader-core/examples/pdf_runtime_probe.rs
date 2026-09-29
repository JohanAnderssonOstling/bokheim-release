//! Check a packaged native runtime without starting the application UI.
use pdf_reader_core::PdfDocumentSession;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: pdf_runtime_probe <PDF> <search text>")?;
    let query = args.next().ok_or("missing search text")?;
    let document = PdfDocumentSession::open(path)?;
    let pages = document.info().page_count();
    assert!(pages > 0, "document has no pages");
    for index in 0..pages {
        let raster = document.render_page(index, Some(320))?;
        assert_eq!(raster.pixel_width(), 320);
        assert!(raster.pixel_height() > 0);
    }
    let matches = document.search(&query)?.len();
    assert!(matches > 0, "search text was not found");
    println!("PDFium runtime OK: rendered {pages} pages, found {matches} search matches");
    Ok(())
}
