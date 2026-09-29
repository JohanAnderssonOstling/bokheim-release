//! Read-only smoke test of the production MOBI inspection path.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    for name in std::fs::read_to_string(std::env::args().nth(1).ok_or("missing file list")?)?.lines() {
        let path = std::path::Path::new(name);
        let result = book_inspection::inspect_mobi_pages_reader(path, std::fs::File::open(path)?);
        match result {
            Ok(book) => println!(
                "{}",
                serde_json::json!({
                    "file":name, "title":book.title,
                    "identifiers":book.book.identifiers.iter().map(|id| id.value().to_owned()).collect::<Vec<_>>(),
                    "lookup_isbns":book_inspection::related_isbn::lookup_candidates(&book.book.identifiers),
                    "subjects":book.book.subjects.iter().filter_map(|s| s.code().map(str::to_owned)).collect::<Vec<_>>(),
                })
            ),
            Err(error) => println!("{}", serde_json::json!({"file":name,"error":error.to_string()})),
        }
    }
    Ok(())
}
