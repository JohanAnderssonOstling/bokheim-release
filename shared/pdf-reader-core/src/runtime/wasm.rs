use pdfium_render::prelude::*;

pub(crate) fn initialize() -> Result<Pdfium, String> {
    // The worker initializes the separately packaged Emscripten module first.
    Pdfium::bind_to_system_library().map(Pdfium::new).map_err(|error| format!("binding PDFium WASM: {error}"))
}
