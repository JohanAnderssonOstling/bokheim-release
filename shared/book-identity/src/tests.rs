use super::*;
use lopdf::{dictionary, Document, Object, Stream};
use std::{fs, io::Cursor, path::Path};

fn pdf(path: &Path) {
    let mut doc = Document::with_version("1.7");
    let pages = doc.new_object_id();
    let stream = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
    let page = doc.add_object(dictionary! { "Type"=>"Page", "Parent"=>pages, "MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()], "Contents"=>stream });
    doc.objects.insert(pages, Object::Dictionary(dictionary! { "Type"=>"Pages", "Kids"=>vec![page.into()], "Count"=>1 }));
    let root = doc.add_object(dictionary! { "Type"=>"Catalog", "Pages"=>pages });
    let info = doc.add_object(dictionary! { "Title"=>Object::string_literal("Original title") });
    doc.trailer.set("Root", root);
    doc.trailer.set("Info", info);
    doc.save(path).unwrap();
}
fn hash(path: &Path) -> ContentHash {
    raw_hash(&mut fs::File::open(path).unwrap()).unwrap()
}
#[test]
fn pdf_first_hash_survives_metadata_edits_and_repeated_scans() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    pdf(&path);
    let original = fs::read(&path).unwrap();
    let id = hash(&path);
    assert_eq!(ensure(&path).unwrap(), id);
    let embedded = fs::read(&path).unwrap();
    assert!(embedded.starts_with(&original), "identity must be an incremental update, not a rewrite of book streams");
    assert_ne!(hash(&path), id);
    let doc = Document::load(&path).unwrap();
    assert_eq!(doc.get_pages().len(), 1);
    assert_eq!(doc.catalog().unwrap().get(PDF_KEY).unwrap().as_str().unwrap(), id.as_str().as_bytes());
    assert_eq!(ensure(&path).unwrap(), id);
    assert_eq!(fs::read(&path).unwrap(), embedded, "repeat scan must not write anything");
    let mut doc = Document::load(&path).unwrap();
    let info = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
    doc.get_object_mut(info).unwrap().as_dict_mut().unwrap().set("Title", Object::string_literal("Edited title"));
    doc.save(&path).unwrap();
    let edited = fs::read(&path).unwrap();
    assert_ne!(edited, embedded);
    assert_eq!(ensure(&path).unwrap(), id);
    assert_eq!(fs::read(&path).unwrap(), edited);
}

#[test]
fn malformed_identity_never_gets_replaced_with_a_new_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    pdf(&path);
    let mut doc = Document::load(&path).unwrap();
    doc.catalog_mut().unwrap().set(PDF_KEY, Object::string_literal("broken"));
    doc.save(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(ensure(&path).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn other_formats_keep_raw_hash_identity() {
    let bytes = b"unmodified EPUB-like fixture";
    let mut source = Cursor::new(bytes);
    assert_eq!(identify(&mut source).unwrap(), ContentHash::new(blake3::hash(bytes).to_hex().as_str()));
}

#[test]
fn embedded_pdf_identity_is_reused_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("embedded.pdf");
    pdf(&path);
    let id = hash(&path);
    let mut doc = Document::load(&path).unwrap();
    doc.catalog_mut().unwrap().set(PDF_KEY, Object::string_literal(id.to_string()));
    doc.save(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_ne!(hash(&path), id);
    assert_eq!(ensure(&path).unwrap(), id);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn pdf_with_prefix_retains_relative_cross_reference_offsets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefixed.pdf");
    pdf(&path);
    let mut bytes = b"Producer prefix\n".to_vec();
    bytes.extend(fs::read(&path).unwrap());
    fs::write(&path, &bytes).unwrap();
    let id = hash(&path);
    assert_eq!(ensure(&path).unwrap(), id);
    assert_eq!(identify(&mut fs::File::open(&path).unwrap()).unwrap(), id);
    assert!(fs::read(path).unwrap().starts_with(&bytes));
}

#[test]
fn malformed_books_are_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in [("broken.pdf", b"%PDF-1.7\nbroken".as_slice()), ("broken.m4b", b"\0\0\0\x18ftypM4B broken".as_slice())] {
        let path = dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        // Neither an unreadable PDF document nor unfingerprintable audio can be
        // given a derived identity, so both fall back to the byte hash and
        // neither file is rewritten.
        assert_eq!(ensure(&path).unwrap(), hash(&path));
        assert_eq!(identify(&mut fs::File::open(&path).unwrap()).unwrap(), hash(&path));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn embedded_pdf_identity_skips_large_page_streams() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.pdf");
    pdf(&path);
    let mut doc = Document::load(&path).unwrap();
    let page = *doc.get_pages().values().next().unwrap();
    let contents = doc.get_object(page).unwrap().as_dict().unwrap().get(b"Contents").unwrap().as_reference().unwrap();
    doc.get_object_mut(contents).unwrap().as_stream_mut().unwrap().set_content(vec![b' '; 8 * 1024 * 1024]);
    doc.save(&path).unwrap();
    let id = ensure(&path).unwrap();
    struct Counted {
        file: fs::File,
        count: usize,
    }
    impl Read for Counted {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let n = self.file.read(b)?;
            self.count += n;
            Ok(n)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
            self.file.seek(p)
        }
    }
    let mut source = Counted { file: fs::File::open(path).unwrap(), count: 0 };
    assert_eq!(identify(&mut source).unwrap(), id);
    assert!(source.count < 256 * 1024, "read {} bytes to identify an 8 MiB PDF", source.count);
}

#[test]
fn changed_audio_is_a_different_book_not_a_revision() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.m4b");
    fs::write(&path, include_bytes!("../../../client/app/tests/fixtures/embedded-cover.m4b")).unwrap();
    let id = ensure(&path).unwrap();
    let before = audio_fingerprint(&mut fs::File::open(&path).unwrap()).unwrap();
    assert_eq!(id, ContentHash::new(before.to_hex().as_str()), "identity is the audio fingerprint");
    let mut bytes = fs::read(&path).unwrap();
    let at = bytes.windows(4).position(|b| b == b"mdat").unwrap() + 4;
    bytes[at] ^= 1;
    fs::write(&path, bytes).unwrap();
    // Metadata edits keep the identity; touching a sample does not.
    assert_ne!(audio_fingerprint(&mut fs::File::open(&path).unwrap()).unwrap(), before);
    assert_ne!(identify(&mut fs::File::open(&path).unwrap()).unwrap(), id);
}

fn encrypted_pdf(path: &Path, encryption: u8, password: &str) {
    use lopdf::{EncryptionState, EncryptionVersion, Permissions};
    pdf(path);
    let mut doc = Document::load(path).unwrap();
    doc.trailer.set("ID", Object::Array(vec![Object::string_literal("0123456789abcdef"); 2]));
    // A direct root string catches double encryption and plaintext writes.
    doc.catalog_mut().unwrap().set("Lang", Object::string_literal("en-US"));
    let version = if encryption == 4 {
        EncryptionVersion::V4 {
            document: &doc,
            encrypt_metadata: true,
            crypt_filters: std::collections::BTreeMap::from([(b"StdCF".to_vec(), std::sync::Arc::new(lopdf::encryption::crypt_filters::Aes128CryptFilter) as std::sync::Arc<dyn lopdf::encryption::crypt_filters::CryptFilter>)]),
            stream_filter: b"StdCF".to_vec(),
            string_filter: b"StdCF".to_vec(),
            owner_password: "owner",
            user_password: password,
            permissions: Permissions::PRINTABLE,
        }
    } else if encryption == 5 {
        EncryptionVersion::V5 {
            encrypt_metadata: false,
            crypt_filters: std::collections::BTreeMap::from([(b"StdCF".to_vec(), std::sync::Arc::new(lopdf::encryption::crypt_filters::Aes256CryptFilter) as std::sync::Arc<dyn lopdf::encryption::crypt_filters::CryptFilter>)]),
            file_encryption_key: &[42; 32],
            stream_filter: b"StdCF".to_vec(),
            string_filter: b"StdCF".to_vec(),
            owner_password: "owner",
            user_password: password,
            permissions: Permissions::PRINTABLE,
        }
    } else if encryption == 1 {
        EncryptionVersion::V1 { document: &doc, owner_password: "owner", user_password: password, permissions: Permissions::PRINTABLE }
    } else {
        EncryptionVersion::V2 { document: &doc, owner_password: "owner", user_password: password, key_length: 128, permissions: Permissions::PRINTABLE }
    };
    let state = EncryptionState::try_from(version).unwrap();
    doc.encrypt(&state).unwrap();
    doc.save(path).unwrap();
}

#[test]
fn empty_password_pdf_identity_preserves_encryption_and_content() {
    for encryption in [1, 2, 4, 5] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("encrypted.pdf");
        encrypted_pdf(&path, encryption, "");
        let before = Document::load(&path).unwrap();
        let page_content = before.get_page_content(*before.get_pages().get(&1).unwrap());
        let original = fs::read(&path).unwrap();
        let identity = hash(&path);
        assert_eq!(ensure(&path).unwrap(), identity);
        let embedded = fs::read(&path).unwrap();
        assert!(embedded.starts_with(&original));
        let metadata = pdf_range_reader::metadata(fs::File::open(&path).unwrap()).unwrap();
        let info = metadata.get_object(metadata.trailer.get(b"Info").unwrap().as_reference().unwrap()).unwrap().as_dict().unwrap();
        assert_eq!(info.get(b"Title").unwrap().as_str().unwrap(), b"Original title");
        let ranged = pdf_range_reader::document_root(fs::File::open(&path).unwrap()).unwrap();
        assert!(ranged.is_encrypted());
        assert_eq!(ranged.get_encrypted().unwrap().get(b"P").unwrap().as_i64().unwrap(), pdf_range_reader::document_root(Cursor::new(original)).unwrap().get_encrypted().unwrap().get(b"P").unwrap().as_i64().unwrap());
        let doc = Document::load(&path).unwrap();
        assert_eq!(doc.catalog().unwrap().get(PDF_KEY).unwrap().as_str().unwrap(), identity.as_str().as_bytes());
        assert_eq!(doc.catalog().unwrap().get(b"Lang").unwrap().as_str().unwrap(), b"en-US");
        assert_eq!(doc.get_page_content(*doc.get_pages().get(&1).unwrap()), page_content);
        assert_eq!(ensure(&path).unwrap(), identity);
        assert_eq!(fs::read(&path).unwrap(), embedded);
    }
}

#[test]
fn nonempty_password_pdf_uses_byte_identity_without_modification() {
    for encryption in [1, 2, 4, 5] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.pdf");
        encrypted_pdf(&path, encryption, "required");
        let original = fs::read(&path).unwrap();
        assert_eq!(ensure(&path).unwrap(), hash(&path));
        assert_eq!(identify(&mut fs::File::open(&path).unwrap()).unwrap(), hash(&path));
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn unmarked_readonly_pdf_uses_byte_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly.pdf");
    pdf(&path);
    let original = fs::read(&path).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    assert_eq!(ensure(&path).unwrap(), hash(&path));
    assert_eq!(identify(&mut fs::File::open(&path).unwrap()).unwrap(), hash(&path));
    assert_eq!(fs::read(path).unwrap(), original);
}
