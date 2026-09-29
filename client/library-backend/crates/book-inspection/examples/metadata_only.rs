fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for file in std::env::args().skip(1) {
        let metadata = book_inspection::inspect_epub_metadata(std::path::Path::new(&file))?;
        let isbns: Vec<_> = metadata.book.identifiers.iter().filter(|id| id.scheme() == &book_model::Scheme::Isbn).filter_map(|id| id.canonical_value()).collect();
        println!("{}", serde_json::json!({"file":file,"isbns":isbns}));
    }
    Ok(())
}
