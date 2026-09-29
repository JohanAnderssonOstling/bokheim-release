//! Pure cover extraction and JPEG thumbnail generation for supported books.

#[cfg(feature = "book-memory")]
use epub_provider::EpubProvider;
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
use image::io::Reader as ImageReader;
use image::{ColorType, DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, RgbImage};
#[cfg(feature = "book-memory")]
use mobi::headers::ExthRecord;
#[cfg(feature = "book-extractors")]
use std::io::SeekFrom;
use std::io::{BufWriter, Cursor};
use std::io::{Read, Seek};

/// Covers are displayed at `LARGE_BOOK_COVER_WIDTH_PX` *logical* pixels, so the
/// stored asset has to carry the device pixel ratio too: 200 logical at DPR 3
/// is 600 real pixels. Anything smaller is upscaled on HiDPI displays and on
/// phones.
///
/// The 600px asset remains the synchronized source of truth. The 300px browse
/// bucket is cached on the server and device, and can be rebuilt from it.
pub const BROWSE_THUMBNAIL_WIDTH: u32 = 300;
pub const THUMBNAIL_WIDTH: u32 = 600;
const MAX_COVER_IMAGE_BYTES: usize = 24 * 1024 * 1024;
const MAX_COVER_IMAGE_PIXELS: u64 = 24_000_000;
const MAX_COVER_IMAGE_DIMENSION: u32 = 20_000;
const EPUB_FALLBACK_MIN_WIDTH: u32 = 300;
const EPUB_FALLBACK_MIN_HEIGHT: u32 = 400;
const EPUB_FALLBACK_MIN_ASPECT_RATIO: f64 = 0.45;
const EPUB_FALLBACK_MAX_ASPECT_RATIO: f64 = 0.90;

pub type ThumbnailResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// The high-density thumbnail and its smaller browse derivative. Both are
/// produced from the same decoded cover.
pub struct ThumbnailVersions {
    pub browse: Vec<u8>,
    pub high_density: Vec<u8>,
}

impl ThumbnailVersions {
    fn from_image(image: &RgbImage) -> ThumbnailResult<Self> {
        Ok(Self { browse: encode_thumbnail_at_width(image, BROWSE_THUMBNAIL_WIDTH)?, high_density: encode_thumbnail_at_width(image, THUMBNAIL_WIDTH)? })
    }
}

/// Uses the same bounded cover extraction for files and browser storage readers.
#[cfg(feature = "book-extractors")]
pub fn generate_thumbnail_versions_from_reader(extension: &str, reader: impl Read + Seek + Send + Sync + 'static) -> ThumbnailResult<Option<ThumbnailVersions>> {
    generate_thumbnail_versions_with_pdf_renderer(extension, reader, render_pdf_cover)
}

/// Decode a downloaded cover once and produce both display buckets.
pub fn generate_thumbnail_versions_from_image_bytes(bytes: &[u8]) -> ThumbnailResult<ThumbnailVersions> {
    ThumbnailVersions::from_image(&decode_cover_image(bytes, image::guess_format(bytes)?)?)
}

/// Read image geometry without decoding its pixels.
pub fn cover_image_dimensions(bytes: &[u8]) -> image::ImageResult<(u32, u32)> {
    Ok(ImageReader::with_format(Cursor::new(bytes), image::guess_format(bytes)?).into_dimensions()?)
}

/// Produces one current display bucket from normalized image bytes.
pub fn generate_thumbnail_at_width_from_image_bytes(bytes: &[u8], width: u32) -> ThumbnailResult<Vec<u8>> {
    let format = image::guess_format(bytes)?;
    encode_thumbnail_at_width(&decode_cover_image(bytes, format)?, width)
}

#[cfg(feature = "book-memory")]
fn epub_cover_from_provider(epub: &EpubProvider) -> ThumbnailResult<Option<RgbImage>> {
    if let Some(image_path) = epub.declared_cover_image_path()? {
        let bytes = epub.read_bytes(&image_path)?;
        let image = decode_epub_image(&bytes, &image_path)?;
        if reasonable_cover_dimensions(image.width(), image.height()) {
            return Ok(Some(image));
        }
    }

    for image_path in epub.image_paths_in_reading_order()? {
        let Ok(bytes) = epub.read_bytes(&image_path) else { continue };
        let Ok(format) = image::guess_format(&bytes) else { continue };
        let Ok((width, height)) = ImageReader::with_format(Cursor::new(&bytes), format).into_dimensions() else { continue };
        if !reasonable_cover_dimensions(width, height) {
            continue;
        }
        if let Ok(image) = decode_cover_image(&bytes, format) {
            return Ok(Some(image));
        }
    }

    Ok(None)
}

#[cfg(feature = "book-memory")]
fn decode_epub_image(bytes: &[u8], path: &str) -> ThumbnailResult<RgbImage> {
    if bytes.len() > MAX_COVER_IMAGE_BYTES {
        return Err("cover image exceeds the byte limit".into());
    }
    if path.rsplit('.').next().is_some_and(|extension| extension.eq_ignore_ascii_case("svg")) {
        return rasterize_svg_cover(bytes);
    }
    let format = image_format(path).or_else(|| image::guess_format(bytes).ok()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("unsupported EPUB cover image format: {path}")))?;
    decode_cover_image(bytes, format)
}

#[cfg(feature = "book-memory")]
fn rasterize_svg_cover(bytes: &[u8]) -> ThumbnailResult<RgbImage> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &options)?;
    let size = tree.size();
    let source_width = size.width();
    let source_height = size.height();
    if !source_width.is_finite() || !source_height.is_finite() || source_width <= 0.0 || source_height <= 0.0 {
        return Err("SVG cover has invalid dimensions".into());
    }
    let aspect_ratio = f64::from(source_width) / f64::from(source_height);
    let destination_height = (f64::from(THUMBNAIL_WIDTH) / aspect_ratio).round().max(1.0);
    if destination_height > f64::from(MAX_COVER_IMAGE_DIMENSION) || u64::from(THUMBNAIL_WIDTH) * destination_height as u64 > MAX_COVER_IMAGE_PIXELS {
        return Err("SVG cover exceeds the pixel limit".into());
    }
    let destination_height = destination_height as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(THUMBNAIL_WIDTH, destination_height).ok_or("could not allocate SVG cover pixmap")?;
    // JPEG has no alpha channel. A white mat preserves the expected appearance
    // of transparent SVG book covers instead of turning transparent pixels black.
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    let scale = THUMBNAIL_WIDTH as f32 / source_width;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    rgb_from_rgba(pixmap.data(), THUMBNAIL_WIDTH, destination_height)
}

/// Returns whether an image is large enough and sufficiently cover-shaped to
/// be preferred over another book-cover source. Providers use
/// the same geometry contract for upstream thumbnails and EPUB fallbacks.
pub fn reasonable_cover_dimensions(width: u32, height: u32) -> bool {
    if width < EPUB_FALLBACK_MIN_WIDTH || height < EPUB_FALLBACK_MIN_HEIGHT {
        return false;
    }
    let aspect_ratio = f64::from(width) / f64::from(height);
    (EPUB_FALLBACK_MIN_ASPECT_RATIO..=EPUB_FALLBACK_MAX_ASPECT_RATIO).contains(&aspect_ratio)
}

#[cfg(feature = "book-extractors")]
fn mobi_cover_from_reader(mut file: impl Read + Seek) -> ThumbnailResult<Option<RgbImage>> {
    // Parsing only MobiMetadata avoids reading and decoding the book's text.
    // CoverOffset and ThumbOffset are relative to first_image_index.
    let metadata = mobi::MobiMetadata::from_read(&mut file)?;
    let file_len = file.seek(SeekFrom::End(0))?;
    let records = &metadata.records.records;
    let first_image_index = metadata.mobi.first_image_index as usize;
    // Prefer the embedded thumbnail, then the cover, then remaining image records.
    let preferred = [ExthRecord::ThumbOffset, ExthRecord::CoverOffset].map(|record| mobi_image_offset(&metadata, record).and_then(|offset| first_image_index.checked_add(offset)));
    let candidates = preferred.iter().copied().flatten().chain((first_image_index..records.len()).filter(|index| !preferred.contains(&Some(*index))));
    for record_index in candidates {
        if let Some(image) = decode_mobi_image_record(&mut file, records, record_index, file_len)? {
            if reasonable_cover_dimensions(image.width(), image.height()) {
                return Ok(Some(image));
            }
        }
    }

    Ok(None)
}

#[cfg(feature = "book-memory")]
fn mobi_image_offset(metadata: &mobi::MobiMetadata, record: ExthRecord) -> Option<usize> {
    let bytes: [u8; 4] = metadata.exth_record(record)?.first()?.as_slice().try_into().ok()?;
    usize::try_from(u32::from_be_bytes(bytes)).ok()
}

#[cfg(feature = "book-extractors")]
fn decode_mobi_image_record(file: &mut (impl Read + Seek), records: &[mobi::record::PdbRecord], record_index: usize, file_len: u64) -> ThumbnailResult<Option<RgbImage>> {
    let Some((start, len)) = mobi_record_bounds(records, record_index, file_len) else { return Ok(None) };
    if len > MAX_COVER_IMAGE_BYTES as u64 {
        return Ok(None);
    }
    let Ok(len) = usize::try_from(len) else { return Ok(None) };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes)?;
    let Ok(format) = image::guess_format(&bytes) else { return Ok(None) };
    match decode_cover_image(&bytes, format) {
        Ok(image) => Ok(Some(image)),
        // A malformed image record should not prevent trying the full cover or
        // another image record from the same book.
        Err(_) => Ok(None),
    }
}

#[cfg(feature = "book-extractors")]
fn mobi_record_bounds(records: &[mobi::record::PdbRecord], record_index: usize, file_len: u64) -> Option<(u64, u64)> {
    let start = u64::from(records.get(record_index)?.offset);
    let end = records.get(record_index + 1).map_or(file_len, |record| u64::from(record.offset));
    (start < end && end <= file_len).then(|| (start, end - start))
}

// The renderers provide pixels already composited onto their background.
fn rgb_from_rgba(pixels: &[u8], width: u32, height: u32) -> ThumbnailResult<RgbImage> {
    let expected = (u64::from(width) * u64::from(height)).checked_mul(4).ok_or("renderer dimensions overflow")?;
    if width == 0 || height == 0 || u64::try_from(pixels.len())? != expected {
        return Err("renderer returned invalid RGBA dimensions".into());
    }
    let rgb = pixels.chunks_exact(4).flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
    RgbImage::from_raw(width, height, rgb).ok_or_else(|| "renderer returned invalid RGB dimensions".into())
}

fn render_pdf_cover(reader: impl Read + Seek + 'static) -> ThumbnailResult<RgbImage> {
    let rendered = pdf_reader_core::render_first_page_image_from_reader(reader, THUMBNAIL_WIDTH as u16)?;
    rgb_from_rgba(rendered.rgba(), rendered.pixel_width(), rendered.pixel_height())
}

fn encode_thumbnail_at_width(decoded: &RgbImage, width: u32) -> ThumbnailResult<Vec<u8>> {
    let (source_width, source_height) = decoded.dimensions();
    if source_width == 0 || source_height == 0 || width == 0 {
        return Err("cover image has zero dimensions".into());
    }
    let destination_height = ((source_height as f64 * f64::from(width) / source_width as f64).round() as u32).max(1);
    validate_cover_image_dimensions(width, destination_height)?;
    if source_width == width {
        return encode_jpeg(decoded.as_raw(), source_width, source_height);
    }
    let source = ImageRef::new(source_width, source_height, decoded.as_raw(), PixelType::U8x3)?;
    let mut destination = Image::new(width, destination_height, PixelType::U8x3);
    Resizer::new().resize(&source, &mut destination, Some(&ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Hamming))))?;

    encode_jpeg(destination.buffer(), width, destination_height)
}

#[cfg(test)]
mod core_tests {
    use super::*;

    #[test]
    fn renderer_pixels_preserve_rgb_and_reject_inconsistent_buffers() {
        let rgba = [10, 20, 30, 255, 40, 50, 60, 255];
        assert_eq!(rgb_from_rgba(&rgba, 2, 1).unwrap().as_raw(), &[10, 20, 30, 40, 50, 60]);
        for bytes in [&rgba[..7], &rgba[..5], &rgba[..4]] {
            assert!(rgb_from_rgba(bytes, 2, 1).is_err());
        }
        assert!(rgb_from_rgba(&[], 0, 0).is_err());
        assert!(rgb_from_rgba(&[], u32::MAX, u32::MAX).is_err());
    }

    #[test]
    fn extreme_aspect_ratio_is_rejected_before_thumbnail_allocation() {
        let image = RgbImage::new(1, MAX_COVER_IMAGE_DIMENSION);
        assert!(encode_thumbnail_at_width(&image, THUMBNAIL_WIDTH).is_err());
    }

    #[test]
    fn embedded_bmp_cover_is_normalized_to_jpeg() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 3, image::Rgb([24, 96, 192])));
        let mut bmp = Cursor::new(Vec::new());
        source.write_to(&mut bmp, ImageFormat::Bmp).unwrap();

        let thumbnail = generate_thumbnail_at_width_from_image_bytes(bmp.get_ref(), THUMBNAIL_WIDTH).unwrap();

        assert_eq!(image::guess_format(&thumbnail).unwrap(), ImageFormat::Jpeg);
        let decoded = image::load_from_memory(&thumbnail).unwrap();
        assert_eq!(decoded.to_rgb8().dimensions(), (THUMBNAIL_WIDTH, 900));
    }
}

fn encode_jpeg(rgb: &[u8], width: u32, height: u32) -> ThumbnailResult<Vec<u8>> {
    let mut encoded = BufWriter::new(Vec::new());
    JpegEncoder::new(&mut encoded).write_image(rgb, width, height, ColorType::Rgb8)?;
    Ok(encoded.into_inner().map_err(std::io::Error::other)?)
}

fn decode_cover_image(bytes: &[u8], format: ImageFormat) -> ThumbnailResult<RgbImage> {
    if bytes.len() > MAX_COVER_IMAGE_BYTES {
        return Err("cover image exceeds the encoded size limit".into());
    }
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()?;
    validate_cover_image_dimensions(width, height)?;
    if format == ImageFormat::Jpeg {
        let mut decoder = JpegDecoder::new(Cursor::new(bytes))?;
        let (source_width, source_height) = decoder.dimensions();
        if source_width > THUMBNAIL_WIDTH {
            let requested_height = ((u64::from(source_height) * u64::from(THUMBNAIL_WIDTH)).div_ceil(u64::from(source_width))).clamp(1, u64::from(u16::MAX)) as u16;
            decoder.scale(THUMBNAIL_WIDTH as u16, requested_height)?;
        }
        return Ok(DynamicImage::from_decoder(decoder)?.into_rgb8());
    }
    let decoded = ImageReader::with_format(Cursor::new(bytes), format).decode()?;
    // Reduce RGBA/16-bit images before RGB conversion: converting the full
    // source would retain a second book-sized pixel buffer.
    let reduced = if width > THUMBNAIL_WIDTH {
        let height = (u64::from(height) * u64::from(THUMBNAIL_WIDTH)).div_ceil(u64::from(width)).max(1) as u32;
        decoded.thumbnail_exact(THUMBNAIL_WIDTH, height)
    } else {
        decoded
    };
    Ok(reduced.into_rgb8())
}

fn validate_cover_image_dimensions(width: u32, height: u32) -> ThumbnailResult<()> {
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or("cover image dimensions overflow")?;
    if width == 0 || height == 0 || width > MAX_COVER_IMAGE_DIMENSION || height > MAX_COVER_IMAGE_DIMENSION || pixels > MAX_COVER_IMAGE_PIXELS {
        return Err("cover image exceeds the pixel limit".into());
    }
    Ok(())
}

#[cfg(feature = "book-memory")]
fn image_format(path: &str) -> Option<ImageFormat> {
    match path.rsplit('.').next()?.to_ascii_lowercase().as_str() {
        "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
        "png" => Some(ImageFormat::Png),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

#[cfg(all(test, feature = "book-extractors"))]
mod book_extractor_tests {
    use super::*;
    use mobi::record::PdbRecord;
    use std::io::Write;

    #[test]
    fn reads_big_endian_mobi_image_offset() {
        let mut metadata = mobi::MobiMetadata::default();
        metadata.exth.records.insert(ExthRecord::ThumbOffset, vec![42_u32.to_be_bytes().to_vec()]);

        assert_eq!(mobi_image_offset(&metadata, ExthRecord::ThumbOffset), Some(42));
    }

    #[test]
    fn validates_mobi_record_bounds() {
        let records = vec![PdbRecord { id: 0, offset: 100 }, PdbRecord { id: 1, offset: 180 }];

        assert_eq!(mobi_record_bounds(&records, 0, 250), Some((100, 80)));
        assert_eq!(mobi_record_bounds(&records, 1, 250), Some((180, 70)));
        assert_eq!(mobi_record_bounds(&records, 2, 250), None);
        assert_eq!(mobi_record_bounds(&records, 1, 150), None);
    }

    #[test]
    fn validates_epub_fallback_image_geometry() {
        assert!(reasonable_cover_dimensions(300, 400));
        assert!(reasonable_cover_dimensions(600, 900));
        assert!(!reasonable_cover_dimensions(299, 600));
        assert!(!reasonable_cover_dimensions(600, 399));
        assert!(!reasonable_cover_dimensions(300, 800));
        assert!(!reasonable_cover_dimensions(800, 600));
    }

    #[test]
    fn rejects_cover_images_with_pathological_decoded_dimensions() {
        assert!(validate_cover_image_dimensions(4_000, 6_000).is_ok());
        assert!(validate_cover_image_dimensions(4_001, 6_000).is_err());
        assert!(validate_cover_image_dimensions(1, MAX_COVER_IMAGE_DIMENSION + 1).is_err());
    }

    #[test]
    fn reduces_large_png_before_rgb_conversion_and_preserves_both_cover_sizes() {
        for source in [
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(1200, 1800, image::Rgb([180, 40, 20]))),
            DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1200, 1800, image::Rgba([180, 40, 20, 255]))),
            DynamicImage::ImageRgba16(image::ImageBuffer::from_pixel(1200, 1800, image::Rgba([46260_u16, 10280, 5140, 65535]))),
        ] {
            let mut png = Cursor::new(Vec::new());
            source.write_to(&mut png, image::ImageOutputFormat::Png).unwrap();
            let cover = decode_cover_image(png.get_ref(), ImageFormat::Png).unwrap();
            assert_eq!(cover.dimensions(), (600, 900));
            assert_eq!(cover.get_pixel(300, 450).0, [180, 40, 20]);
            let versions = ThumbnailVersions::from_image(&cover).unwrap();
            assert_eq!(image::load_from_memory(&versions.browse).unwrap().width(), 300);
            let high = image::load_from_memory(&versions.high_density).unwrap();
            assert_eq!((high.width(), high.height()), (600, 900));
        }
    }

    #[test]
    fn rasterizes_svg_cover_to_thumbnail_width() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="600"><rect width="400" height="600" fill="#c02010"/></svg>"##;
        let rendered = rasterize_svg_cover(svg).unwrap();

        assert_eq!(rendered.dimensions(), (THUMBNAIL_WIDTH, 900));
        let center = rendered.get_pixel(THUMBNAIL_WIDTH / 2, 450);
        assert!(center[0] > 170 && center[1] < 60 && center[2] < 45, "unexpected SVG pixel: {center:?}");
    }

    #[test]
    fn uses_first_reasonable_spine_image_when_cover_art_is_absent() {
        let file = tempfile::Builder::new().suffix(".epub").tempfile().unwrap();
        let mut archive = zip::ZipWriter::new(file.reopen().unwrap());
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        archive.start_file("mimetype", stored).unwrap();
        archive.write_all(b"application/epub+zip").unwrap();
        archive.start_file("META-INF/container.xml", deflated).unwrap();
        archive
            .write_all(br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="EPUB/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#)
            .unwrap();
        archive.start_file("EPUB/package.opf", deflated).unwrap();
        archive
            .write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Image fallback</dc:title></metadata><manifest><item id="unreferenced" href="images/unreferenced.jpg" media-type="image/jpeg"/><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/><item id="tiny" href="images/tiny.jpg" media-type="image/jpeg"/><item id="candidate" href="images/illustration-1.jpg" media-type="image/jpeg"/></manifest><spine><itemref idref="page"/></spine></package>"#)
            .unwrap();
        archive.start_file("EPUB/page.xhtml", deflated).unwrap();
        archive.write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><img src="images/tiny.jpg"/><img src="images/illustration-1.jpg"/></body></html>"#).unwrap();
        archive.start_file("EPUB/images/tiny.jpg", deflated).unwrap();
        archive.write_all(&encode_jpeg(&vec![20; 64 * 64 * 3], 64, 64).unwrap()).unwrap();
        archive.start_file("EPUB/images/unreferenced.jpg", deflated).unwrap();
        let unreferenced_pixels = [20, 40, 190].into_iter().cycle().take(400 * 600 * 3).collect::<Vec<_>>();
        archive.write_all(&encode_jpeg(&unreferenced_pixels, 400, 600).unwrap()).unwrap();
        archive.start_file("EPUB/images/illustration-1.jpg", deflated).unwrap();
        let candidate_pixels = [190, 35, 25].into_iter().cycle().take(400 * 600 * 3).collect::<Vec<_>>();
        archive.write_all(&encode_jpeg(&candidate_pixels, 400, 600).unwrap()).unwrap();
        archive.finish().unwrap();

        let jpeg = generate_thumbnail_versions_from_reader("epub", file.reopen().unwrap()).unwrap().expect("reasonable image fallback").high_density;
        let bytes = std::fs::read(file.path()).unwrap();
        let in_memory = generate_thumbnail_versions_from_reader("epub", Cursor::new(bytes.clone())).unwrap().expect("in-memory reasonable image fallback").high_density;
        let provider = EpubProvider::try_from_reader(Cursor::new(bytes)).unwrap();
        let from_provider = encode_thumbnail_at_width(&epub_cover_from_provider(&provider).unwrap().expect("shared-provider reasonable image fallback"), THUMBNAIL_WIDTH).unwrap();
        assert_eq!(in_memory, jpeg);
        assert_eq!(from_provider, jpeg);
        let rendered = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(rendered.dimensions(), (THUMBNAIL_WIDTH, 900));
        let center = rendered.get_pixel(THUMBNAIL_WIDTH / 2, 450);
        assert!(center[0] > 170 && center[1] < 55 && center[2] < 45, "unexpected fallback pixel: {center:?}");
    }
}

mod pdf_cover;
#[cfg(not(target_arch = "wasm32"))]
mod pdf_process;
#[cfg(not(target_arch = "wasm32"))]
pub use pdf_process::{configure_pdf_cover_process, run_pdf_cover_process, PdfCoverProcess};

/// Extract simple opaque PDF image covers directly; render complex pages with PDFium.
/// Resizing/encoding happens after the renderer releases its worker.
#[cfg(feature = "book-extractors")]
pub fn generate_thumbnail_versions_with_pdf_renderer<R: Read + Seek + Send + Sync + 'static>(extension: &str, mut reader: R, render: impl FnOnce(R) -> ThumbnailResult<RgbImage>) -> ThumbnailResult<Option<ThumbnailVersions>> {
    let cover = match extension {
        "epub" => epub_cover_from_provider(&EpubProvider::try_from_reader(reader)?)?,
        "mobi" | "azw" | "azw3" => mobi_cover_from_reader(reader)?,
        "m4b" => {
            let tag = mp4ameta::Tag::read_from(&mut reader)?;
            tag.artwork().map(|artwork| decode_cover_image(artwork.data, image::guess_format(artwork.data)?)).transpose()?
        }
        "mp3folder" => {
            let mut archive = zip::ZipArchive::new(reader)?;
            let mut cover = None;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index)?;
                let name = entry.name().to_ascii_lowercase();
                if ["cover.jpg", "cover.jpeg", "cover.png", "cover.webp", "folder.jpg", "folder.jpeg", "folder.png", "folder.webp"].contains(&name.as_str()) {
                    if entry.size() > audiobook_folder::MAX_COVER_BYTES { return Err("audiobook cover exceeds the byte limit".into()); }
                    let mut bytes = Vec::new();
                    entry.read_to_end(&mut bytes)?;
                    cover = image::guess_format(&bytes).ok().and_then(|format| decode_cover_image(&bytes, format).ok());
                    break;
                }
            }
            if cover.is_none() && archive.len() != 0 {
                #[cfg(not(target_arch = "wasm32"))]
                let facts = {
                    let mut audio = tempfile::tempfile()?;
                    std::io::copy(&mut archive.by_index(0)?, &mut audio)?;
                    audio.seek(SeekFrom::Start(0))?;
                    audiobook_folder::inspect_mp3(audio, true).ok()
                };
                #[cfg(target_arch = "wasm32")]
                let facts = {
                    let mut audio = Vec::new();
                    archive.by_index(0)?.read_to_end(&mut audio)?;
                    audiobook_folder::inspect_mp3(std::io::Cursor::new(audio), true).ok()
                };
                if let Some(bytes) = facts.and_then(|facts| facts.artwork) {
                    cover = image::guess_format(&bytes).ok().and_then(|format| decode_cover_image(&bytes, format).ok());
                }
            }
            cover
        }
        "pdf" => Some(match pdf_cover::extract(&mut reader) {
            Some(image) => image,
            None => {
                reader.seek(SeekFrom::Start(0))?;
                render(reader)?
            }
        }),
        _ => None,
    };
    cover.as_ref().map(ThumbnailVersions::from_image).transpose()
}

#[cfg(all(test, feature = "book-extractors"))]
mod audiobook_sidecar_tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn mp3_folder_cover_is_used_for_thumbnails() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(600, 900).write_to(&mut png, ImageFormat::Png).unwrap();
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        archive.start_file("01.mp3", stored).unwrap();
        archive.write_all(b"audio").unwrap();
        archive.start_file("cover.png", stored).unwrap();
        archive.write_all(png.get_ref()).unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        assert!(generate_thumbnail_versions_from_reader("mp3folder", std::io::Cursor::new(bytes)).unwrap().is_some());
    }

    #[test]
    fn mp3_embedded_art_is_used_when_folder_has_no_cover_file() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(600, 900).write_to(&mut png, ImageFormat::Png).unwrap();
        let mut picture = vec![0];
        picture.extend_from_slice(b"image/png\0");
        picture.extend_from_slice(&[3, 0]);
        picture.extend_from_slice(png.get_ref());
        let mut frame = Vec::new();
        frame.extend_from_slice(b"APIC");
        frame.extend_from_slice(&(picture.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&picture);
        let size = frame.len() as u32;
        let mut mp3 = vec![b'I', b'D', b'3', 3, 0, 0, ((size >> 21) & 0x7f) as u8, ((size >> 14) & 0x7f) as u8, ((size >> 7) & 0x7f) as u8, (size & 0x7f) as u8];
        mp3.extend(frame);
        mp3.extend_from_slice(include_bytes!("../../book-metadata/tests/fixtures/silence.mp3"));
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        archive.start_file("01.mp3", stored).unwrap();
        archive.write_all(&mp3).unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        assert!(generate_thumbnail_versions_from_reader("mp3folder", std::io::Cursor::new(bytes)).unwrap().is_some());
    }
}
