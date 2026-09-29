pub(super) use pdf_range_reader::{document_root, load_edges, metadata};

#[cfg(test)]
mod tests {
    use lopdf::dictionary;
    use lopdf::{Dictionary, Document, Object, Stream};
    use pdf_range_reader::{load, load_edges};
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[test]
    fn edge_inspection_reads_final_copyright_without_middle_page_streams() {
        use lopdf::content::{Content, Operation};
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let mut kids = Vec::new();
        let mut streams = Vec::new();
        for index in 0..50 {
            let text = if index == 49 { "Copyright Ada Author. Library of Congress Cataloging-in-Book Data. Science History. ISBN 978-0-393-24327-7 (pdf). Classification: LCC Q125 .A12 2023" } else { "Ordinary chapter" };
            let content = Content { operations: vec![Operation::new("BT", vec![]), Operation::new("Tf", vec!["F1".into(), 12.into()]), Operation::new("Tj", vec![Object::string_literal(text)]), Operation::new("ET", vec![])] };
            let stream = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            streams.push(stream);
            let page = document
                .add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => stream, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }, "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()] });
            kids.push(page.into());
        }
        document.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 50, "Kids" => kids }.into());
        let root = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        let info = document.add_object(dictionary! { "Title" => Object::string_literal("Science History"), "Author" => Object::string_literal("Ada Author") });
        document.trailer.set("Root", root);
        document.trailer.set("Info", info);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        let loaded = load_edges(std::io::Cursor::new(bytes.clone()), 8).unwrap();
        assert!(!loaded.objects.contains_key(&streams[25]));
        assert!(loaded.objects.contains_key(&streams[49]));
        let inspected = crate::inspect_pdf_reader(std::path::Path::new("book.pdf"), std::io::Cursor::new(bytes)).unwrap();
        {
            assert!(inspected.metadata.book.subjects.iter().any(|s| s.code() == Some("Q125 .A12 2023")));
            assert_eq!(inspected.metadata.book.identifiers[0].canonical_value().as_deref(), Some("9780393243277"));
            assert!(inspected.metadata.book.subjects[0].source().contains("pdf:page:50"));
        }
    }
    struct Counted<R> {
        inner: R,
        read: Arc<AtomicUsize>,
    }
    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            let count = self.inner.read(bytes)?;
            self.read.fetch_add(count, Ordering::Relaxed);
            Ok(count)
        }
    }
    impl<R: Seek> Seek for Counted<R> {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }
    #[test]
    fn indirect_page_children_and_invalid_optional_info_remain_readable() {
        for invalid_info in [false, true] {
            let mut document = Document::with_version("1.7");
            let pages = document.new_object_id();
            let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()] });
            let kids = document.add_object(Object::Array(vec![page.into()]));
            document.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => kids }.into());
            let root = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
            document.trailer.set("Root", root);
            let mut bytes = Vec::new();
            document.save_to(&mut bytes).unwrap();
            if invalid_info {
                let pointer = bytes.windows(9).rposition(|part| part == b"startxref").unwrap();
                let previous = std::str::from_utf8(&bytes[pointer + 9..]).unwrap().split_whitespace().next().unwrap();
                let offset = bytes.len();
                let info = "10 0 obj\n<< /Invalid Name (Bad producer metadata) >>\nendobj\n";
                let xref = offset + info.len();
                let update = format!("{info}xref\n10 1\n{offset:010} 00000 n\ntrailer\n<< /Size 11 /Info 10 0 R /Prev {previous} >>\nstartxref\n{xref}\n%%EOF\n");
                bytes.extend_from_slice(update.as_bytes());
            }
            let inspected = crate::inspect_pdf_reader(std::path::Path::new("readable.pdf"), std::io::Cursor::new(bytes.clone())).unwrap();
            assert_eq!(inspected.metadata.title, "readable");
            let loaded = load(std::io::Cursor::new(bytes), 20).unwrap();
            assert_eq!(loaded.get_pages().len(), 1);
        }
    }

    #[test]
    fn incremental_metadata_uses_latest_revision_and_inherits_the_page_tree() {
        let mut document = Document::with_version("1.7");
        let pages = document.new_object_id();
        let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()] });
        document.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()] }.into());
        let root = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", root);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        let pointer = bytes.windows(9).rposition(|part| part == b"startxref").unwrap();
        let previous = std::str::from_utf8(&bytes[pointer + 9..]).unwrap().split_whitespace().next().unwrap();
        let object = bytes.len();
        let update = format!("10 0 obj\n<< /Title (Latest title) >>\nendobj\n");
        let xref = object + update.len();
        let trailer = format!("xref\n10 1\n{object:010} 00000 n\ntrailer\n<< /Size 11 /Info 10 0 R /Prev {previous} >>\nstartxref\n{xref}\n%%EOF\n");
        bytes.extend_from_slice(update.as_bytes());
        bytes.extend_from_slice(trailer.as_bytes());
        let loaded = load(std::io::Cursor::new(bytes), 20).unwrap();
        assert_eq!(loaded.get_pages().len(), 1);
        assert_eq!(loaded.get_object((10, 0)).unwrap().as_dict().unwrap().get(b"Title").unwrap().as_str().unwrap(), b"Latest title");
    }

    #[test]
    fn metadata_skips_large_images_with_classic_and_compressed_references() {
        for modern in [false, true] {
            let mut document = Document::with_version("1.7");
            let pages = document.new_object_id();
            let image = document.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 4000, "Height" => 4000, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, vec![255; 32 * 1024 * 1024]));
            let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
            let text = document.add_object(Stream::new(Dictionary::new(), b"BT /F1 12 Tf (ISBN 978-0-8213-3827-8) Tj ET".to_vec()));
            let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => text, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "Im0" => image } }, "MediaBox" => vec![0.into(),0.into(),600.into(),800.into()] });
            document.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()] }.into());
            let root = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
            let info = document.add_object(dictionary! { "Title" => Object::string_literal("Large illustrated book"), "Author" => Object::string_literal("An Author") });
            document.trailer.set("Root", root);
            document.trailer.set("Info", info);
            let mut bytes = Vec::new();
            if modern {
                document.save_modern(&mut bytes).unwrap();
            } else {
                document.save_to(&mut bytes).unwrap();
            }
            let read = Arc::new(AtomicUsize::new(0));
            let reader = Counted { inner: std::io::Cursor::new(bytes), read: read.clone() };
            let loaded = load(reader, 20).unwrap();
            assert_eq!(loaded.get_object(info).unwrap().as_dict().unwrap().get(b"Title").unwrap().as_str().unwrap(), b"Large illustrated book");
            assert!(loaded.extract_text(&[1]).unwrap().contains("978-0-8213-3827-8"));
            assert!(read.load(Ordering::Relaxed) < 1024 * 1024, "read {} bytes (modern={modern})", read.load(Ordering::Relaxed));
            assert!(loaded.get_object(image).unwrap().as_stream().unwrap().content.is_empty());
        }
    }
}
