//! Browser integration fixture for early transport adoption and host handover.
#![cfg(target_arch = "wasm32")]
use app::{host::web::backend_configuration, AppClient};
use js_sys::Error;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

thread_local! {
    static CLIENT: RefCell<Option<AppClient>> = const { RefCell::new(None) };
}

#[wasm_bindgen]
pub async fn connect(broker_url: String) -> Result<u32, JsValue> {
    let (client, startup) = AppClient::connect_web(backend_configuration(&broker_url)).await.map_err(|error| Error::new(&error.to_string()))?;
    CLIENT.with(|slot| *slot.borrow_mut() = Some(client));
    Ok(startup.libraries.len() as u32)
}

#[wasm_bindgen]
pub async fn libraries() -> Result<u32, JsValue> {
    let client = CLIENT.with(|slot| slot.borrow().clone()).ok_or_else(|| Error::new("probe is not connected"))?;
    let libraries = client.libraries().await.map_err(|error| Error::new(&error.to_string()))?;
    Ok(libraries.len() as u32)
}

/// Exercise the real library owner and its common read-only taxonomy attach.
#[wasm_bindgen]
pub async fn create_library(name: String) -> Result<String, JsValue> {
    let client = CLIENT.with(|slot| slot.borrow().clone()).ok_or_else(|| Error::new("probe is not connected"))?;
    let entry = client.create_library_with_asset_storage(name, false).await.map_err(|error| Error::new(&error.to_string()))?;
    let count = client.library(*entry.library_id()).book_count().await.map_err(|error| Error::new(&error.to_string()))?;
    if count != 0 {
        return Err(Error::new("new fixture library is not empty").into());
    }
    Ok(entry.library_id().to_string())
}
