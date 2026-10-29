use super::*;
use crate::{Book, BookFormat, EpubOpeningPlan};
use async_zip::{
    base::read1::{ZipOptions, seek::ZipArchiveReader},
    error::ZipError,
};
use futures_lite::{future::block_on, io::Cursor as AsyncCursor};
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const CHAPTER: &[u8] = b"<html><body>A readable chapter</body></html>";
const CONTAINER: &[u8] = br#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#;
const PACKAGE: &[u8] = br#"<package><manifest><item id="one" href="one.xhtml" media-type="application/xhtml+xml"/><item id="two" href="two.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="two"/><itemref idref="one"/></spine></package>"#;

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for &(name, bytes) in entries {
        writer.start_file(name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn directory(bytes: &[u8]) -> usize {
    let end = bytes.len() - 22;
    u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize
}

fn prepare(bytes: Vec<u8>) -> crate::ReadingBook {
    let book = Book::open_reader(BookFormat::Epub, Cursor::new(bytes)).unwrap();
    block_on(book.prepare_reading()).unwrap()
}

#[test]
fn missing_or_broken_container_recovers_the_package_and_reading_order() {
    for container in [None, Some(b"broken XML".as_slice()), Some(br#"<container><rootfile full-path="missing.opf"/></container>"#)] {
        let mut entries = vec![("OPS/one.xhtml", CHAPTER), ("OPS/two.xhtml", CHAPTER), ("OPS/book.opf", PACKAGE)];
        if let Some(container) = container {
            entries.push(("META-INF/container.xml", container));
        }
        let reading = prepare(archive(&entries));
        assert_eq!(reading.document_uris(), ["OPS/two.xhtml", "OPS/one.xhtml"]);
    }
}

#[test]
fn missing_package_still_opens_html_and_ignores_non_ascii_unrelated_entries() {
    let reading = prepare(archive(&[("one.xhtml", CHAPTER), ("other-💡", b"unrelated")]));
    assert_eq!(reading.document_uris(), ["one.xhtml"]);
    assert_eq!(block_on(reading.read_resource("one.xhtml".into())).unwrap(), CHAPTER);
}

#[test]
fn optional_missing_resources_and_stale_spine_items_do_not_block_reading() {
    let package = br#"<package><manifest>
        <item id="missing" href="missing.xhtml" media-type="application/xhtml+xml"/>
        <item id="remote" href="https://example.invalid/chapter" media-type="application/xhtml+xml"/>
        <item id="one" href="text/CHAPTER%201.XHTML" media-type="wrong/type"/>
        <item id="css" href="missing.css" media-type="text/css"/>
        <item id="auxiliary" href="missing-notes.xhtml" media-type="text/html"/>
        </manifest><spine><itemref idref="missing"/><itemref idref="remote"/><itemref idref="one"/><itemref idref="one"/></spine></package>"#;
    let bytes = archive(&[("META-INF/container.xml", CONTAINER), ("OPS/book.opf", package), ("OPS/Text/chapter 1.xhtml", CHAPTER)]);
    let reading = prepare(bytes);
    assert_eq!(reading.document_uris(), ["OPS/Text/chapter 1.xhtml"]);
    assert_eq!(reading.resources.read_bytes("OPS/text/CHAPTER%201.XHTML").unwrap(), CHAPTER);
    assert_eq!(reading.resources.media_type("OPS/Text/chapter 1.xhtml").unwrap().as_deref(), Some("text/html"));
}

#[test]
fn recovered_html_can_still_load_stylesheets_without_a_manifest() {
    let reading = prepare(archive(&[("one.xhtml", CHAPTER), ("styles.css", b"body { color: blue }")]));
    assert_eq!(reading.resources.read_bytes("styles.css").unwrap(), b"body { color: blue }");
    assert_eq!(reading.resources.media_type("styles.css").unwrap().as_deref(), Some("text/css"));
}

#[test]
fn swapped_html_and_css_media_types_are_recovered_from_filenames() {
    let package = br#"<package><manifest><item id="one" href="one.xhtml" media-type="text/css"/><item id="css" href="style.css" media-type="text/html"/></manifest><spine><itemref idref="one"/></spine></package>"#;
    let reading = prepare(archive(&[("book.opf", package), ("one.xhtml", CHAPTER), ("style.css", b"body { color: blue }")]));
    assert_eq!(reading.resources.media_type("one.xhtml").unwrap().as_deref(), Some("text/html"));
    assert_eq!(reading.resources.media_type("style.css").unwrap().as_deref(), Some("text/css"));
}

#[test]
fn valid_media_types_with_unusual_extensions_still_work() {
    let package = br#"<package><manifest><item id="one" href="chapter.css" media-type="application/xhtml+xml"/><item id="css" href="style.xhtml" media-type="text/css"/></manifest><spine><itemref idref="one"/></spine></package>"#;
    let reading = prepare(archive(&[("book.opf", package), ("chapter.css", CHAPTER), ("style.xhtml", b"<!-- body { color: blue } -->")]));
    assert_eq!(reading.document_uris(), ["chapter.css"]);
    assert_eq!(reading.resources.media_type("chapter.css").unwrap().as_deref(), Some("application/xhtml+xml"));
    assert_eq!(reading.resources.media_type("style.xhtml").unwrap().as_deref(), Some("text/css"));
}

#[test]
fn all_missing_spine_items_fall_back_to_available_html() {
    let reading = prepare(archive(&[("OPS/book.opf", PACKAGE), ("OPS/recovered.xhtml", CHAPTER)]));
    assert_eq!(reading.document_uris(), ["OPS/recovered.xhtml"]);
}

#[test]
fn unreadable_chapter_is_skipped_and_remaining_chapters_still_open() {
    let mut bytes = archive(&[("OPS/one.xhtml", CHAPTER), ("OPS/two.xhtml", CHAPTER), ("OPS/book.opf", PACKAGE)]);
    bytes[..4].fill(0); // Break only the first chapter's local signature.
    let reading = prepare(bytes);
    assert_eq!(reading.document_uris(), ["OPS/two.xhtml"]);
}

#[test]
fn entirely_unreadable_spine_recovers_other_available_html() {
    let mut bytes = archive(&[("OPS/one.xhtml", CHAPTER), ("OPS/recovered.xhtml", CHAPTER), ("OPS/book.opf", PACKAGE)]);
    bytes[..4].fill(0);
    let reading = prepare(bytes);
    assert_eq!(reading.document_uris(), ["OPS/recovered.xhtml"]);
}

#[test]
fn no_readable_chapters_still_reports_an_error() {
    let mut bytes = archive(&[("one.xhtml", CHAPTER)]);
    bytes[..4].fill(0);
    let book = Book::open_reader(BookFormat::Epub, Cursor::new(bytes)).unwrap();
    assert!(block_on(book.prepare_reading()).is_err());
    assert!(Book::open_reader(BookFormat::Epub, Cursor::new(archive(&[("unused.bin", b"no text")]))).is_err());
    let book = Book::open_reader(BookFormat::Epub, Cursor::new(archive(&[("empty.xhtml", b"")]))).unwrap();
    assert!(block_on(book.prepare_reading()).is_err());
}

#[test]
fn legacy_and_utf16_package_encodings_preserve_metadata() {
    let xml = "<?xml version=\"1.0\" encoding=\"windows-1252\"?><package><metadata><dc:title xmlns:dc=\"http://purl.org/dc/elements/1.1/\">Café</dc:title></metadata></package>";
    let legacy = WINDOWS_1252.encode(xml).0.into_owned();
    let utf16_xml = xml.replace("windows-1252", "UTF-16");
    let mut utf16 = vec![0xff, 0xfe];
    utf16.extend(utf16_xml.encode_utf16().flat_map(u16::to_le_bytes));
    for package in [legacy, utf16] {
        let mut book = Book::open_reader(BookFormat::Epub, Cursor::new(archive(&[("book.opf", &package), ("one.xhtml", CHAPTER)]))).unwrap();
        assert_eq!(book.metadata().unwrap().title, "Café");
        assert_eq!(block_on(book.prepare_reading()).unwrap().document_uris(), ["one.xhtml"]);
    }
}

#[test]
fn stale_local_sizes_compression_and_crc_use_the_central_directory() {
    let mut bytes = archive(&[("one.xhtml", CHAPTER)]);
    bytes[8..10].copy_from_slice(&8_u16.to_le_bytes()); // Stale local Deflate method.
    bytes[14..26].fill(0); // Stale local checksum and sizes.
    let central = directory(&bytes);
    bytes[central + 16..central + 20].fill(0); // Stale directory checksum.
    bytes[central + 24..central + 28].copy_from_slice(&1_u32.to_le_bytes()); // Wrong decoded size.
    let reading = prepare(bytes);
    assert_eq!(reading.resources.read_bytes("one.xhtml").unwrap(), CHAPTER);
    assert_eq!(reading.document_weights, [CHAPTER.len() as u64]);
}

#[test]
fn stale_directory_counts_are_hints_and_actual_entry_limits_are_enforced() {
    let mut bytes = archive(&[("one.xhtml", CHAPTER), ("two.xhtml", CHAPTER)]);
    let end = bytes.len() - 22;
    bytes[end + 8..end + 10].copy_from_slice(&123_u16.to_le_bytes());
    bytes[end + 10..end + 12].fill(0);
    bytes[end + 12..end + 16].copy_from_slice(&20_000_000_u32.to_le_bytes());
    assert_eq!(prepare(bytes.clone()).document_uris().len(), 2);
    block_on(async {
        let options = ZipOptions { prefer_central_directory: true, validate_num_cd_files: false, max_cd_num_files: 1, ..ZipOptions::untrusted() };
        let error = ZipArchiveReader::open_with_options(AsyncCursor::new(bytes.clone()), options).await.err().unwrap();
        assert!(matches!(error, ZipError::NumFilesAboveMax(1)));
        let options = ZipOptions { prefer_central_directory: true, validate_num_cd_files: false, max_cd_size_in_bytes: 1, ..ZipOptions::untrusted() };
        let error = ZipArchiveReader::open_with_options(AsyncCursor::new(bytes), options).await.err().unwrap();
        assert!(matches!(error, ZipError::CDSizeAboveMax(1)));
    });
}

#[test]
fn legacy_cp437_names_and_windows_path_separators_are_readable() {
    let mut bytes = archive(&[("a.xhtml", CHAPTER)]);
    let central = directory(&bytes);
    bytes[30] = 0x86; // CP437 å, with no UTF-8 flag.
    bytes[central + 46] = 0x86;
    let reading = prepare(bytes);
    assert_eq!(reading.document_uris(), ["å.xhtml"]);
    assert_eq!(reading.resources.read_bytes("å.xhtml").unwrap(), CHAPTER);

    let reading = prepare(archive(&[("OPS\\chapter.xhtml", CHAPTER)]));
    assert_eq!(reading.resources.read_bytes("OPS/chapter.xhtml").unwrap(), CHAPTER);
}

#[test]
fn case_fallback_does_not_guess_between_ambiguous_resources() {
    let reading = prepare(archive(&[("One.xhtml", CHAPTER), ("one.xhtml", b"different chapter")]));
    assert_eq!(reading.resources.read_bytes("One.xhtml").unwrap(), CHAPTER);
    assert!(reading.resources.read_bytes("ONE.XHTML").is_err());
}

#[test]
fn remote_opening_plan_handles_missing_package_and_optional_resources() {
    for bytes in [archive(&[("one.xhtml", CHAPTER)]), archive(&[("OPS/book.opf", PACKAGE), ("OPS/two.xhtml", CHAPTER)])] {
        block_on(async {
            let bytes: Arc<[u8]> = bytes.into();
            let factory = move || AsyncCursor::new(bytes.clone());
            let plan = EpubOpeningPlan::read(factory.clone()).await.unwrap();
            assert!(!plan.initial_ranges().unwrap().is_empty());
            assert!(!plan.required_ranges(factory).await.unwrap().is_empty());
        });
    }
}
