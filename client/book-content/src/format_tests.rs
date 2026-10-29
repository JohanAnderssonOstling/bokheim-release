use crate::{Book, BookFormat, comic::ComicBook};
use futures_lite::future::block_on;
use std::io::{Cursor, Write};

const FB2: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><genre>sf</genre><author><first-name>Ada</first-name><last-name>Author</last-name></author>
<translator><first-name>Test</first-name><last-name>Translator</last-name></translator><book-title>A &amp; B</book-title>
<annotation><p>A description.</p></annotation><lang>en</lang><coverpage><image l:href="#cover"/></coverpage></title-info>
<publish-info><isbn>9780306406157</isbn><year>1980</year></publish-info></description>
<body><section id="one"><title><p>Chapter one</p></title><p>Hello <emphasis>world</emphasis> &amp; friends.
<a l:href="#note" type="note">1</a><image l:href="#cover"/></p>
<section id="nested"><title><p>Nested</p></title><p>More text.</p></section></section>
<section id="two"><title><p>Chapter two</p></title><p><a l:href="#one">Back</a></p></section></body>
<body name="notes"><section id="note"><title><p>Note</p></title><p>A footnote.</p></section></body>
<binary id="cover" content-type="image/png">aG Vs\nbG8=</binary></FictionBook>"##;

fn zip(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap()
}

#[test]
fn fb2_metadata_navigation_images_and_cross_document_notes() {
    let source = FB2.replace("\\n", "\n");
    let mut book = Book::open_reader(BookFormat::Fb2, Cursor::new(source.into_bytes())).unwrap();
    let metadata = book.metadata().unwrap();
    assert_eq!(metadata.title, "A & B");
    assert_eq!(metadata.creators[0].name, "Ada Author");
    assert_eq!(metadata.contributors[0].role.as_str(), "trl");
    assert!(metadata.identifiers.is_empty(), "print ISBN must not be ebook identity");
    let cover = book.preferred_cover_resources().unwrap().pop().unwrap();
    assert_eq!(book.read_resource_sync(&cover, 100).unwrap(), b"hello");
    let toc = book.toc().unwrap().unwrap();
    assert_eq!(toc.len(), 2, "notes are not main contents entries");
    assert_eq!(toc[0].children[0].title, "Nested");
    let prepared = block_on(book.prepare_reading()).unwrap();
    assert_eq!(prepared.languages(), ["en"]);
    assert_eq!(prepared.document_uris().len(), 3);
    assert_eq!(prepared.resources.media_type(&cover).unwrap().as_deref(), Some("image/png"));
    let html = String::from_utf8(prepared.resources.read_bytes(&prepared.document_uris()[0]).unwrap()).unwrap();
    for uri in prepared.document_uris() {
        crate::xml::parse(&prepared.resources.read_bytes(uri).unwrap(), 24 * 1024 * 1024).unwrap();
    }
    assert!(html.contains("<em>world</em>"));
    assert!(html.contains("href=\"/fb2/section-2.xhtml#fb2-node-"));
    assert!(html.contains("epub:type=\"noteref\""));
    let notes = String::from_utf8(prepared.resources.read_bytes(&prepared.document_uris()[2]).unwrap()).unwrap();
    assert!(notes.contains("role=\"doc-footnote\""));
    assert_eq!(block_on(prepared.read_resource(cover)).unwrap(), b"hello");
}

#[test]
fn fb2_accepts_zip_and_declared_legacy_encoding() {
    let text = FB2.replace("utf-8", "windows-1251").replace("A &amp; B", "Привет").replace("\\n", "\n");
    let (bytes, _, errors) = encoding_rs::WINDOWS_1251.encode(&text);
    assert!(!errors);
    let mut book = Book::open_reader(BookFormat::Fb2, zip(&[("novel.fb2", &bytes)])).unwrap();
    assert_eq!(book.metadata().unwrap().title, "Привет");
}

#[test]
fn fb2_rejects_invalid_xml_and_ambiguous_archives() {
    for text in ["<unrelated/>", "<!DOCTYPE x [<!ENTITY e 'x'>]><FictionBook/>", "<FictionBook><body>", "<a/><b/>"] {
        assert!(Book::open_reader(BookFormat::Fb2, Cursor::new(text.as_bytes().to_vec())).is_err());
    }
    assert!(Book::open_reader(BookFormat::Fb2, zip(&[("a.fb2", FB2.as_bytes()), ("b.fb2", FB2.as_bytes())])).is_err());
}

#[test]
fn cbz_orders_pages_and_projects_comicinfo_without_decoding_images() {
    let info = br#"<ComicInfo><Title>Issue one</Title><Writer>Alice, Bob</Writer><Series>Series</Series>
<Manga>YesAndRightToLeft</Manga><Pages><Page Image="1" Type="FrontCover" Bookmark="Part two" DoublePage="true"/></Pages></ComicInfo>"#;
    let mut book = ComicBook::open_reader(zip(&[("10.jpg", b"ten"), ("2.jpg", b"two"), ("1.jpg", b"one"), ("__MACOSX/0.jpg", b"skip"), (".hidden.jpg", b"skip"), ("ComicInfo.xml", info)])).unwrap();
    assert_eq!(book.pages.iter().map(|page| page.name.as_str()).collect::<Vec<_>>(), ["1.jpg", "2.jpg", "10.jpg"]);
    assert_eq!(book.metadata.title, "Issue one");
    assert_eq!(book.metadata.creators.len(), 2);
    assert!(book.right_to_left);
    assert_eq!(book.cover_page, 1);
    assert!(book.pages[1].double_page);
    assert_eq!(book.read_page(book.cover_page).unwrap(), b"two");
    let reader = block_on(book.into_reader()).unwrap();
    assert_eq!(block_on(reader.read_page(2)).unwrap(), b"ten");
    assert!(block_on(reader.read_page(3)).is_err());
}

#[test]
fn cbz_rejects_empty_and_unrelated_archives() {
    assert!(ComicBook::open_reader(zip(&[("readme.txt", b"no pages")])).is_err());
}

#[test]
fn cbz_async_reading_keeps_unrequested_pages_in_the_archive() {
    use futures_lite::io::BufReader;
    use futures_util::io::AllowStdIo;
    use std::{
        io::{self, Read, Seek, SeekFrom},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    struct Counted {
        cursor: Cursor<Arc<[u8]>>,
        reads: Arc<AtomicUsize>,
    }
    impl Read for Counted {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = self.cursor.read(bytes)?;
            self.reads.fetch_add(count, Ordering::Relaxed);
            Ok(count)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.cursor.seek(position)
        }
    }
    let names: Vec<_> = (0..16).map(|index| format!("{index:02}.png")).collect();
    let page = vec![7; 256 * 1024];
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in &names {
        archive.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        archive.write_all(&page).unwrap();
    }
    let bytes: Arc<[u8]> = archive.finish().unwrap().into_inner().into();
    let length = bytes.len();
    let count = Arc::new(AtomicUsize::new(0));
    let reads = count.clone();
    let book = block_on(ComicBook::open(move || BufReader::new(AllowStdIo::new(Counted { cursor: Cursor::new(bytes.clone()), reads: reads.clone() })))).unwrap();
    let reader = block_on(book.into_reader()).unwrap();
    assert!(count.load(Ordering::Relaxed) < length / 4, "opening must only read the directory and metadata");
    assert_eq!(block_on(reader.read_page(5)).unwrap(), page);
    assert!(count.load(Ordering::Relaxed) < length / 2, "reading a page must not buffer the whole CBZ");
}

#[test]
fn malformed_optional_comicinfo_does_not_hide_readable_pages() {
    let mut book = ComicBook::open_reader(zip(&[("1.png", b"page"), ("ComicInfo.xml", b"<ComicInfo><broken>")])).unwrap();
    assert_eq!(book.read_page(0).unwrap(), b"page");
}

#[test]
fn failed_comic_prefetch_retains_ordinary_page_reads() {
    use futures_util::io::{AllowStdIo, BufReader};
    let bytes = zip(&[("1.png", b"first"), ("2.png", b"second"), ("ComicInfo.xml", b"<ComicInfo><Title>Fallback</Title></ComicInfo>")]).into_inner();
    let book = block_on(ComicBook::open_with_prefetch(move || BufReader::new(AllowStdIo::new(Cursor::new(bytes.clone()))), 1, |_| async { Err(std::io::Error::other("prefetch unavailable")) })).unwrap();
    assert_eq!(book.metadata.title, "Fallback");
    let reader = block_on(book.into_reader()).unwrap();
    assert_eq!(block_on(reader.read_page(1)).unwrap(), b"second");
}

#[test]
fn comicinfo_deleted_pages_do_not_shift_cover_and_bookmark_references() {
    let info = br#"<ComicInfo><Pages><Page Image="0" Type="Deleted"/><Page Image="1" Type="FrontCover" Bookmark="Start"/></Pages></ComicInfo>"#;
    let mut book = ComicBook::open_reader(zip(&[("1.png", b"deleted"), ("2.png", b"cover"), ("ComicInfo.xml", info)])).unwrap();
    assert_eq!(book.pages.len(), 1);
    assert_eq!(book.cover_page, 0);
    assert_eq!(book.pages[0].bookmark.as_deref(), Some("Start"));
    assert_eq!(book.read_page(0).unwrap(), b"cover");
}
