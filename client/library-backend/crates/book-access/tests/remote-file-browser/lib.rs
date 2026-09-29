use std::{
    cell::{Cell, RefCell},
    io::{Read, Seek, SeekFrom},
};
use wasm_bindgen::prelude::*;
use book_access::{remote_file, BoxedBookReader, ContentHash};
thread_local! {
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static FILE: RefCell<Option<BoxedBookReader>> = RefCell::new(None);
    static TOKEN: RefCell<String> = RefCell::new("test-token".into());
}
#[wasm_bindgen]
pub fn set_token(token: String) {
    TOKEN.with(|value| *value.borrow_mut() = token);
}
#[wasm_bindgen]
pub async fn open_file(url: String) -> Result<(), JsValue> {
    close_file();
    let generation = GENERATION.with(Cell::get);
    let url = reqwest::Url::parse(&url).map_err(|e| JsValue::from_str(&e.to_string()))?;
    let (source, initial_bytes) = remote_file::describe(url.clone(), TOKEN.with(|value| value.borrow().clone()), ContentHash::new("7777777777777777777777777777777777777777777777777777777777777777")).await.map_err(|e| JsValue::from_str(&e.to_string()))?;
    let reader = remote_file::browser_reader(source.length, Some(initial_bytes), move |offset, length| {
        let url = url.clone();
        let source = source.clone();
        let token = TOKEN.with(|value| value.borrow().clone());
        Box::pin(async move { remote_file::fetch_range(&reqwest::Client::new(), url, &source, &token, offset, length).await.map_err(|e| e.to_string()) })
    })?;
    if GENERATION.with(Cell::get) != generation { return Err("file closed while opening".into()); }
    FILE.with(|file| *file.borrow_mut() = Some(reader));
    Ok(())
}
#[wasm_bindgen]
pub async fn read_file(offset: u32, length: u32) -> Result<js_sys::Uint8Array, JsValue> {
    let generation = GENERATION.with(Cell::get);
    let mut file = FILE.with(|file| file.borrow_mut().take()).ok_or("file not open")?;
    let (file, result) = wasm_thread::spawn(move || {
        let result = (|| {
            file.seek(SeekFrom::Start(offset as u64))?;
            let mut bytes = vec![0; length as usize];
            file.read_exact(&mut bytes)?;
            Ok::<_, std::io::Error>(bytes)
        })()
        .map_err(|e| e.to_string());
        (file, result)
    })
    .join_async()
    .await
    .map_err(|_| "parser worker panicked")?;
    if GENERATION.with(Cell::get) != generation {
        return Err("file closed while reading".into());
    }
    FILE.with(|slot| *slot.borrow_mut() = Some(file));
    Ok(js_sys::Uint8Array::from(result?.as_slice()))
}
#[wasm_bindgen]
pub fn close_file() {
    GENERATION.with(|generation| generation.set(generation.get() + 1));
    FILE.with(|file| file.borrow_mut().take());
    EPUB.with(|slot| slot.borrow_mut().take());
}

thread_local! {
    static EPUB: RefCell<Option<std::sync::Arc<epub_provider::EpubProvider>>> = const { RefCell::new(None) };
}
#[wasm_bindgen]
pub async fn open_epub(url: String) -> Result<(), JsValue> {
    EPUB.with(|slot| slot.borrow_mut().take());
    open_file(url).await?;
    let generation = GENERATION.with(Cell::get);
    let file = FILE.with(|slot| slot.borrow_mut().take()).ok_or("file not open")?;
    let provider = wasm_thread::spawn(move || epub_provider::EpubProvider::try_from_reader(file).map_err(|e| e.to_string())).join_async().await.map_err(|_| "EPUB parser worker panicked")??;
    if GENERATION.with(Cell::get) != generation { return Err("file closed while opening EPUB".into()); }
    EPUB.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::new(provider)));
    Ok(())
}
#[wasm_bindgen]
pub async fn read_epub_entry(name: String) -> Result<usize, JsValue> {
    let provider = EPUB.with(|slot| slot.borrow().clone()).ok_or("EPUB not open")?;
    Ok(wasm_thread::spawn(move || provider.read_string(&name).map(|text| text.len()).map_err(|e| e.to_string())).join_async().await.map_err(|_| "EPUB parser worker panicked")??)
}
