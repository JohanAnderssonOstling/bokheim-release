//! Cache the bundled reference in this worker's OPFS-backed SQLite VFS.
use js_sys::{global, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Response, WorkerGlobalScope};

pub async fn install() -> Result<(), String> {
    let filename = subject_projection::bisac_reference_filename();
    client_platform_web::sqlite::prune_reference_databases("bisac-reference.sqlite3", "bisac-reference", &filename)?;
    if subject_projection::open_bisac_reference()? {
        return Ok(());
    }
    let scope: WorkerGlobalScope = global().dyn_into().map_err(|_| "BISAC reference requires a worker")?;
    let url = format!("./{filename}");
    let response: Response = JsFuture::from(scope.fetch_with_str(&url)).await.map_err(|e| format!("BISAC reference download failed: {e:?}"))?.dyn_into().map_err(|_| "invalid BISAC response")?;
    if !response.ok() {
        return Err(format!("BISAC reference download returned HTTP {}", response.status()));
    }
    let buffer = JsFuture::from(response.array_buffer().map_err(|e| format!("BISAC response failed: {e:?}"))?).await.map_err(|e| format!("BISAC response failed: {e:?}"))?;
    subject_projection::install_bisac_reference(&Uint8Array::new(&buffer).to_vec(), |path, image| {
        let name = path.to_str().ok_or("BISAC reference path is not UTF-8")?;
        client_platform_web::sqlite::import_reference_database(name, image)
    })
}
