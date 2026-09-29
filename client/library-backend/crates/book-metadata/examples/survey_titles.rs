//! Metadata-only title audit. Does not import, modify files, or run OCR.
use std::path::{Path, PathBuf};
fn files(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_dir() {
            files(&entry.path(), output)?;
        } else if entry.file_type()?.is_file() {
            output.push(entry.path());
        }
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os().nth(1).ok_or("usage: survey_titles DIRECTORY")?;
    let mut paths = Vec::new();
    files(Path::new(&root), &mut paths)?;
    paths.sort();
    for path in paths {
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else { continue };
        let format = match extension.to_ascii_lowercase().as_str() {
            "epub" => book_model::BookFormat::Epub,
            "pdf" => book_model::BookFormat::Pdf,
            "mobi" => book_model::BookFormat::Mobi,
            "m4b" => book_model::BookFormat::M4b,
            _ => continue,
        };
        let metadata = book_metadata::inspect_book(&path, format).map(|book| book.metadata);
        let value = match metadata {
            Ok(book) => serde_json::json!({"file":path,"format":extension,"title":book.title,"subtitle":book.subtitle()}),
            Err(error) => serde_json::json!({"file":path,"format":extension,"error":error.to_string()}),
        };
        println!("{value}");
    }
    Ok(())
}
