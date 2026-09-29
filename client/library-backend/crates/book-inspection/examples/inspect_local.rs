//! Headless inspection for library jobs and local evidence audits; no network.
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pages = std::env::args().any(|arg| arg == "--pages");
    let keep_going = std::env::args().any(|arg| arg == "--keep-going");
    for name in std::env::args().skip(1).filter(|arg| arg != "--keep-going" && arg != "--pages") {
        let result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let path = std::path::Path::new(&name);
            let book = match path.extension().and_then(|s| s.to_str()) {
                Some("epub") if pages => book_inspection::inspect_epub_pages_reader(path, std::fs::File::open(path)?)?,
                Some("pdf") if pages => book_inspection::inspect_pdf_pages_reader(path, std::fs::File::open(path)?)?.metadata,
                Some("epub") => book_inspection::inspect_epub(path)?.metadata,
                Some("pdf") => book_inspection::inspect_pdf(path)?.metadata,
                _ => return Err("expected an EPUB or PDF path".into()),
            };
            println!(
                "{}",
                serde_json::json!({"file":name, "title":book.title, "isbns":book.book.identifiers.iter().filter(|id| id.scheme() == &book_model::Scheme::Isbn && id.scope() == book_model::Scope::Book).filter_map(|id| id.canonical_value()).collect::<Vec<_>>(), "reference_isbns":book.book.identifiers.iter().filter(|id| id.scheme() == &book_model::Scheme::Isbn && id.scope() == book_model::Scope::Edition).filter_map(|id| id.canonical_value()).collect::<Vec<_>>(), "usable_lcc":book.book.subjects.iter().filter(|s| s.authority() == Some("lcc")).filter_map(|s| s.code()).filter(|code| book_inspection::evidence::usable_lcc(code)).collect::<Vec<_>>(), "lcc":book.book.subjects.iter().filter(|s| s.authority() == Some("lcc")).filter_map(|s| s.code()).collect::<Vec<_>>()})
            );
            Ok(())
        })();
        if let Err(error) = result {
            if !keep_going {
                return Err(error);
            }
            println!("{}", serde_json::json!({"file":name,"error":error.to_string()}));
        }
    }
    Ok(())
}
