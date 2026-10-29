#[path = "../crates/html/src/book.rs"]
mod adapter;
pub(crate) use adapter::BookSource as NativeBook;

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use book_content::BookFormat;
    use futures_lite::future::block_on;
    use html::pipeline::{DocumentFactory, ResourceContext};
    use html::resources::{ImagePipeline, ImageService, InlineJobSpawner};
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use std::{
        collections::HashSet,
        io::Cursor,
        sync::Arc,
        time::{Duration, Instant},
    };

    #[test]
    fn fb2_adapter_loads_embedded_images_through_the_html_pipeline() {
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 3, Rgb([20, 100, 180]))).write_to(&mut png, ImageFormat::Png).unwrap();
        let xml = format!(
            r##"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink"><description><title-info><book-title>Test book</book-title></title-info></description><body><section><title><p>Chapter</p></title><p>Reading text.</p><image l:href="#picture"/></section></body><binary id="picture" content-type="image/png">{}</binary></FictionBook>"##,
            STANDARD.encode(png.into_inner())
        );
        let book = NativeBook::from_reader(BookFormat::Fb2, Cursor::new(xml.into_bytes())).unwrap();
        let uri = &book.document_uris()[0];
        let source = String::from_utf8(book.provider.read_bytes(uri).unwrap()).unwrap();
        let mut factory = DocumentFactory::new();
        let document = factory.with_resources(ResourceContext::new(book.provider.as_ref(), uri)).parse_to_dom(&source);
        assert_eq!(document.images().len(), 1);
        let loader = book.image_loader(|task| {
            std::thread::spawn(move || block_on(task));
        });
        let service = ImageService::with_loader(loader, Arc::new(InlineJobSpawner), ImageService::DEFAULT_BYTE_BUDGET);
        let mut pipeline = ImagePipeline::with_service(service);
        pipeline.ensure_window(document.images(), &HashSet::from([0]));
        let deadline = Instant::now() + Duration::from_secs(2);
        while pipeline.get_decoded(0).is_none() && Instant::now() < deadline {
            pipeline.poll();
            std::thread::yield_now();
        }
        assert_eq!(pipeline.get_decoded(0).expect("FB2 image must load through the real HTML resource pipeline").dimensions(), (2, 3));
    }
}
