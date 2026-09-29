//! Diagnostic text from the same PDFium extraction used by local inspection.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for name in std::env::args().skip(1) {
        let pages = pdf_reader_core::inspect_page_text(std::fs::File::open(&name)?, 20, 128 * 1024)?;
        for (page, text) in pages {
            if text.to_lowercase().contains("isbn") || text.contains("2013933932") || text.contains("Copyright") || text.contains('©') {
                println!("{}", serde_json::json!({"file":name,"page":page,"text":text}));
            }
        }
    }
    Ok(())
}
