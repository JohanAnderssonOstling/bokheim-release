use super::*;
use lopdf::{Document, Object, Stream, dictionary};
use std::{
    fs::{self, File},
    io::Cursor,
    path::Path,
};

fn pdf(path: &Path, marker: &str) {
    let mut doc = Document::with_version("1.7");
    let pages = doc.new_object_id();
    let stream = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
    let page = doc.add_object(dictionary! { "Type"=>"Page", "Parent"=>pages, "MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()], "Contents"=>stream });
    doc.objects.insert(pages, Object::Dictionary(dictionary! { "Type"=>"Pages", "Kids"=>vec![page.into()], "Count"=>1 }));
    let root = doc.add_object(dictionary! { "Type"=>"Catalog", "Pages"=>pages, "BokheimContentHash"=>Object::string_literal(marker) });
    let info = doc.add_object(dictionary! { "Title"=>Object::string_literal("Original title") });
    doc.trailer.set("Root", root);
    doc.trailer.set("Info", info);
    doc.save(path).unwrap();
}

#[test]
fn pdf_identity_ignores_embedded_markers_and_changes_with_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book.pdf");
    for marker in ["0".repeat(64), "broken".to_owned()] {
        pdf(&path, &marker);
        let original = fs::read(&path).unwrap();
        let identity = hash_file(&mut File::open(&path).unwrap()).unwrap();
        assert_eq!(identity, ContentHash::from(blake3::hash(&original)));
        assert_ne!(identity.to_hex().as_str(), marker);
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut doc = Document::load(&path).unwrap();
        let info = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        doc.get_object_mut(info).unwrap().as_dict_mut().unwrap().set("Title", Object::string_literal("Edited title"));
        doc.save(&path).unwrap();
        let edited = fs::read(&path).unwrap();
        let edited_identity = hash_file(&mut File::open(&path).unwrap()).unwrap();
        assert_ne!(edited_identity, identity);
        assert_eq!(edited_identity, ContentHash::from(blake3::hash(&edited)));
        assert_eq!(fs::read(&path).unwrap(), edited);
    }
}

#[test]
fn m4b_identity_changes_with_audio_or_metadata() {
    let original = include_bytes!("../../../client/app/tests/fixtures/embedded-cover.m4b");
    let identity = hash_file(&mut Cursor::new(original)).unwrap();
    assert_eq!(identity, ContentHash::from(blake3::hash(original)));
    let audio_at = original.windows(4).position(|bytes| bytes == b"mdat").unwrap() + 4;
    let title_at = original.windows(10).position(|bytes| bytes == b"The Hobbit").unwrap();
    for at in [audio_at, title_at] {
        let mut edited = original.to_vec();
        edited[at] ^= 1;
        let edited_identity = hash_file(&mut Cursor::new(&edited)).unwrap();
        assert_ne!(edited_identity, identity);
        assert_eq!(edited_identity, ContentHash::from(blake3::hash(&edited)));
    }
}

#[test]
fn identity_hashes_all_bytes_without_format_parsing() {
    for bytes in [b"%PDF-1.7\nbroken".as_slice(), b"\0\0\0\x18ftypM4B broken", b"unmodified EPUB-like fixture", b""] {
        let mut reader = Cursor::new(bytes);
        reader.set_position(bytes.len() as u64);
        assert_eq!(hash_file(&mut reader).unwrap(), ContentHash::from(blake3::hash(bytes)));
    }
}

#[test]
fn stream_hashing_retries_interruptions_and_starts_at_the_current_position() {
    struct Fragmented<'a> {
        bytes: Cursor<&'a [u8]>,
        interrupt: bool,
    }
    impl Read for Fragmented<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.interrupt = !self.interrupt;
            if self.interrupt {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = output.len().min(8191);
            self.bytes.read(&mut output[..count])
        }
    }
    let bytes: Vec<_> = (0..131_091).map(|index| index as u8).collect();
    let mut reader = Fragmented { bytes: Cursor::new(bytes.as_slice()), interrupt: false };
    reader.bytes.set_position(17);
    assert_eq!(hash_reader(&mut reader).unwrap(), ContentHash::from(blake3::hash(&bytes[17..])));
    assert_eq!(reader.bytes.position(), bytes.len() as u64);
}

#[test]
fn stream_hashing_preserves_source_errors() {
    struct Failed;
    impl Read for Failed {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "fixture source unreadable"))
        }
    }
    let mut reader = b"partial book".as_slice().chain(Failed);
    let error = hash_reader(&mut reader).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "fixture source unreadable");
}

#[test]
fn raw_hash_roundtrips_without_changing_json() {
    let bytes = std::array::from_fn(|index| index as u8);
    let hash = ContentHash::from_bytes(bytes);
    assert_eq!(std::mem::size_of::<ContentHash>(), 32);
    assert_eq!(hash.as_bytes(), &bytes);
    assert_eq!(hash.to_hex().parse::<ContentHash>().unwrap(), hash);
    assert_eq!(serde_json::to_string(&hash).unwrap(), format!("\"{hash}\""));
    assert_eq!(serde_json::from_str::<ContentHash>(&format!("\"{hash}\"")).unwrap(), hash);
    let stored = wire::encode(&hash).unwrap();
    assert_eq!(stored.len(), wire::header_bytes().len() + 1 + 32);
    assert_eq!(wire::decode::<ContentHash>(&stored, 1024).unwrap(), hash);
    let transported = protobuf_wire::encode(&vec![Some(hash)]).unwrap();
    assert!(transported.windows(32).any(|window| window == bytes));
    assert!(!transported.windows(64).any(|window| window == hash.to_hex().as_bytes()));
    assert_eq!(protobuf_wire::decode::<Vec<Option<ContentHash>>>(&transported, 1024).unwrap(), vec![Some(hash)]);
}

#[test]
fn raw_bytes_have_no_alphabet_constraint_and_reject_wrong_lengths() {
    assert_eq!(ContentHash::try_from([255; 32].as_slice()).unwrap().as_bytes(), &[255; 32]);
    for length in [0, 31, 33, 64] {
        assert!(ContentHash::try_from(vec![0; length].as_slice()).is_err());
    }
    for text in ["a".repeat(63), "a".repeat(65), "A".repeat(64), "g".repeat(64)] {
        assert!(text.parse::<ContentHash>().is_err());
    }
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_hashes_are_blobs_and_json_batch_lists_stay_hex() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE book(content_hash BLOB PRIMARY KEY)").unwrap();
    let hash = ContentHash::from_bytes([42; 32]);
    conn.execute("INSERT INTO book VALUES(?1)", [hash]).unwrap();
    let actual: (String, i64, ContentHash) = conn.query_row("SELECT typeof(content_hash),length(content_hash),content_hash FROM book", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    assert_eq!(actual, ("blob".into(), 32, hash));
    let json = serde_json::to_string(&vec![hash]).unwrap();
    let found: ContentHash = conn.query_row("SELECT book.content_hash FROM json_each(?1) requested JOIN book ON book.content_hash=unhex(requested.value)", [json], |row| row.get(0)).unwrap();
    assert_eq!(found, hash);
}
