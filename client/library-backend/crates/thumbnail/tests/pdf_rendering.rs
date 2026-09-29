#![cfg(all(feature = "book-extractors", not(target_arch = "wasm32")))]

use std::io::{Cursor, Read};
use thumbnail::{generate_thumbnail_versions_from_reader, generate_thumbnail_versions_with_pdf_renderer};

// A red image with page margins and, optionally, a blue vector overlay.
fn pdf(overlay: bool) -> Vec<u8> {
    let content = format!("q 160 0 0 240 20 30 cm /Cover Do Q\n{}", if overlay { "0 0 1 rg 80 120 40 60 re f" } else { "" });
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 300] /Resources << /XObject << /Cover 5 0 R >> >> /Contents 4 0 R >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        String::new(),
    ];
    let mut objects = objects.map(String::into_bytes);
    objects[4] = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 3 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 18 >>\nstream\n".to_vec();
    objects[4].extend_from_slice(&[255, 0, 0].repeat(6));
    objects[4].extend_from_slice(b"\nendstream");
    let mut output = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        output.extend_from_slice(object);
        output.extend_from_slice(b"\nendobj\n");
    }
    let xref = output.len();
    output.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in &offsets[1..] {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    output
}

#[test]
fn complex_pdf_uses_supplied_renderer_with_rewound_reader() {
    let bytes = pdf(true);
    let versions = generate_thumbnail_versions_with_pdf_renderer("pdf", Cursor::new(bytes.clone()), |mut reader| {
        assert_eq!(reader.position(), 0);
        let mut received = Vec::new();
        reader.read_to_end(&mut received)?;
        assert_eq!(received, bytes);
        Ok(image::RgbImage::from_pixel(600, 900, image::Rgb([0, 0, 255])))
    })
    .unwrap()
    .unwrap();
    let image = image::load_from_memory(&versions.high_density).unwrap().to_rgb8();
    assert!(image.get_pixel(300, 450)[2] > 250);
    assert_eq!(image::load_from_memory(&versions.browse).unwrap().width(), 300);
}

#[test]
fn renderer_errors_propagate() {
    let result = generate_thumbnail_versions_with_pdf_renderer("pdf", Cursor::new(pdf(true)), |_| Err("renderer unavailable".into()));
    assert_eq!(result.err().unwrap().to_string(), "renderer unavailable");
}

#[test]
fn pdfium_preserves_page_margins_and_vector_overlay() {
    for overlay in [true] {
        let versions = generate_thumbnail_versions_from_reader("pdf", Cursor::new(pdf(overlay))).unwrap().unwrap();
        for (bytes, width) in [(&versions.browse, 300), (&versions.high_density, 600)] {
            let image = image::load_from_memory(bytes).unwrap().to_rgb8();
            assert_eq!(image.dimensions(), (width, width * 3 / 2));
            assert!(image.get_pixel(5, 5).0.iter().all(|c| *c > 250));
            let red = image.get_pixel(width / 4, width / 2);
            assert!(red[0] > 250 && red[1] < 5 && red[2] < 5);
            let center = image.get_pixel(width / 2, width * 3 / 4);
            assert!(if overlay { center[2] > 250 && center[0] < 5 } else { center[0] > 250 && center[2] < 5 });
        }
    }
}

#[test]
fn simple_image_cover_bypasses_renderer() {
    let versions = generate_thumbnail_versions_with_pdf_renderer("pdf", Cursor::new(pdf(false)), |_| panic!("simple cover should use lopdf")).unwrap().unwrap();
    let image = image::load_from_memory(&versions.high_density).unwrap().to_rgb8();
    assert_eq!(image.dimensions(), (600, 900));
    let pixel = image.get_pixel(300, 450);
    assert!(pixel[0] > 250 && pixel[1] < 5 && pixel[2] < 5);
}
