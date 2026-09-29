use library_files::asset_store::{AssetStore, ImportProgressObserver};
use library_model::ImportFileStage;
use std::{
    io::Read,
    sync::{Arc, Mutex},
};

#[tokio::test]
async fn copy_reports_bytes_and_stages_without_changing_contents() {
    let root = tempfile::tempdir().unwrap();
    let store = AssetStore::open(root.path().to_str().unwrap()).unwrap();
    let stages = Arc::new(Mutex::new(Vec::new()));
    let observed = stages.clone();
    let report: ImportProgressObserver = Arc::new(move |stage| observed.lock().unwrap().push(stage));
    let bytes = vec![7u8; 150_000];
    let staged = store.stage_book_with_progress("/book.epub".into(), Box::new(std::io::Cursor::new(bytes.clone())), Some(report)).await.unwrap();
    let mut actual = Vec::new();
    staged.open_reader().unwrap().read_to_end(&mut actual).unwrap();
    assert_eq!(actual, bytes);
    let stages = stages.lock().unwrap();
    assert_eq!(stages.first(), Some(&ImportFileStage::Copying { copied_bytes: 0 }));
    assert!(stages.ends_with(&[ImportFileStage::Copying { copied_bytes: 150_000 }, ImportFileStage::Saving, ImportFileStage::Identifying]));
}
