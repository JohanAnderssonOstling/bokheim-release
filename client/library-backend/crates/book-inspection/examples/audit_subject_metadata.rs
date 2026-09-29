//! Read-only comparison probe: one path per input line, one JSON result per line.
use std::io::{self, BufRead};
use std::path::Path;
fn main() {
    for line in io::stdin().lock().lines() {
        let path = line.unwrap();
        let result = if path.to_lowercase().ends_with(".epub") { book_inspection::inspect_epub(Path::new(&path)).map(|book| book.metadata) } else { book_inspection::inspect_pdf(Path::new(&path)).map(|book| book.metadata) };
        match result {
            Ok(book) => println!("{}", serde_json::json!({"path":path,"subjects":book.book.subjects.iter().map(|subject| serde_json::json!({"name":subject.name(),"source":subject.source()})).collect::<Vec<_>>()})),
            Err(error) => println!("{}", serde_json::json!({"path":path,"error":error.to_string()})),
        }
    }
}
