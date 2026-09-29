use lopdf::{dictionary, Document, Object, Stream};
use std::path::Path;

pub fn write_annotated_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
    let existing_annotation = document.add_object(dictionary! {
        "Type" => "Annot",
        "Subtype" => "Highlight",
        "Rect" => vec![60.into(), 600.into(), 240.into(), 632.into()],
        "QuadPoints" => vec![60.into(), 632.into(), 240.into(), 632.into(), 60.into(), 600.into(), 240.into(), 600.into()],
        "C" => vec![Object::Real(1.0), Object::Real(0.0), Object::Real(0.0), Object::Real(0.0)],
        "CA" => Object::Real(0.55),
        "Contents" => Object::string_literal("Existing interoperable comment"),
        "T" => Object::string_literal("Other reader"),
        "NM" => Object::string_literal("other-reader-annotation"),
    });
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()],
        "Contents" => content_id,
        "Resources" => dictionary! {},
        "Annots" => vec![existing_annotation.into()],
    });
    document.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1 }));
    let root_id = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", root_id);
    document.save(path).unwrap();
}
