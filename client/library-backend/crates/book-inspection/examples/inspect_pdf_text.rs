//! Read-only diagnostic comparing PDF text with the bibliographic evidence rules.
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let name = std::env::args().nth(1).ok_or("expected PDF path")?;
    let book = book_inspection::inspect_pdf(std::path::Path::new(&name))?.metadata;
    let doc = lopdf::Document::load(&name)?;
    for page in 1..=3 {
        let text = doc.extract_text(&[page])?;
        println!("{}", serde_json::json!({"page":page,"text":text,"evidence":book_inspection::evidence::inspect_section(&book,&format!("pdf:page:{page}"),&text)}));
    }
    Ok(())
}
