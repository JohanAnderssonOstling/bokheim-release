use super::*;
use std::io::Cursor;
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn fixture(root: &Path, extra: &[(&str, &[u8])]) -> PathBuf {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for name in REQUIRED {
        writer.start_file(*name, SimpleFileOptions::default()).unwrap();
        writer.write_all(b"runtime").unwrap();
    }
    for (name, bytes) in extra {
        writer.start_file(*name, SimpleFileOptions::default()).unwrap();
        writer.write_all(bytes).unwrap();
    }
    let archive = writer.finish().unwrap().into_inner();
    let executable = root.join("Standalone.exe");
    let mut output = File::create(&executable).unwrap();
    output.write_all(b"MZ launcher stub").unwrap();
    output.write_all(&archive).unwrap();
    output.write_all(MAGIC).unwrap();
    output.write_all(&(archive.len() as u64).to_le_bytes()).unwrap();
    output.write_all(&Sha256::digest(&archive)).unwrap();
    executable
}

#[test]
fn extracts_once_and_preserves_updated_runtime() {
    let temp = TempDir::new().unwrap();
    let executable = fixture(temp.path(), &[("tts/alba/phonemes.txt", b"voice data")]);
    let cache = temp.path().join("Cache with spaces and å");
    let directory = prepare(&executable, &cache).unwrap();
    assert_eq!(fs::read(directory.join("tts/alba/phonemes.txt")).unwrap(), b"voice data");
    fs::write(directory.join("Bokheim.exe"), b"newer app from signed update").unwrap();
    assert_eq!(prepare(&executable, &cache).unwrap(), directory);
    assert_eq!(fs::read(directory.join("Bokheim.exe")).unwrap(), b"newer app from signed update");
    fs::remove_file(directory.join("taxonomy.sqlite3")).unwrap();
    assert!(prepare(&executable, &cache).is_err());
    assert_eq!(fs::read(directory.join("Bokheim.exe")).unwrap(), b"newer app from signed update");
}

#[test]
fn interrupted_extraction_and_concurrent_launches_recover() {
    let temp = TempDir::new().unwrap();
    let executable = fixture(temp.path(), &[]);
    let key = Payload::open(&executable).unwrap().key();
    let cache = temp.path().join("cache");
    let partial = cache.join(format!("{key}.partial"));
    fs::create_dir_all(&partial).unwrap();
    fs::write(partial.join("Bokheim.exe"), b"interrupted").unwrap();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4).map(|_| scope.spawn(|| prepare(&executable, &cache).unwrap())).collect();
        for handle in handles {
            assert_eq!(fs::read(handle.join().unwrap().join("Bokheim.exe")).unwrap(), b"runtime");
        }
    });
    assert!(!partial.exists());
}

#[test]
fn corrupted_payload_never_becomes_launchable() {
    let temp = TempDir::new().unwrap();
    let executable = fixture(temp.path(), &[]);
    let mut file = OpenOptions::new().write(true).open(&executable).unwrap();
    file.seek(SeekFrom::Start(20)).unwrap();
    file.write_all(b"corrupted").unwrap();
    drop(file);
    let cache = temp.path().join("cache");
    let key = Payload::open(&executable).unwrap().key();
    assert!(prepare(&executable, &cache).is_err());
    assert!(!cache.join(&key).exists());
    assert!(!cache.join(format!("{key}.partial")).exists());
}

#[test]
fn unsafe_paths_and_case_duplicates_are_rejected() {
    for name in ["../escape", "/absolute", "tts/../../escape", "tts\\escape", "tts/alternate:stream", "tts/CON.txt", "tts/LPT1", "tts/trailing.", "tts/trailing ", "bokheim.EXE"] {
        let temp = TempDir::new().unwrap();
        let executable = fixture(temp.path(), &[(name, b"unsafe")]);
        assert!(prepare(&executable, &temp.path().join("cache")).is_err(), "accepted {name}");
        assert!(!temp.path().join("escape").exists());
    }
}

#[test]
fn truncated_and_invalid_trailers_are_rejected() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("missing.exe");
    fs::write(&path, b"MZ incomplete").unwrap();
    assert!(Payload::open(&path).is_err());
    let path = fixture(temp.path(), &[]);
    let mut bytes = fs::read(&path).unwrap();
    let length = bytes.len();
    bytes[length - 40..length - 32].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, bytes).unwrap();
    assert!(Payload::open(&path).is_err());
}
