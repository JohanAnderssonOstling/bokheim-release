#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub(super) use native::initialize;
#[cfg(target_arch = "wasm32")]
pub(super) use wasm::initialize;

// All native PDFium users share this gate. Reentrancy permits nested extraction
// helpers and document cleanup while an operation already owns the gate.
#[cfg(not(target_arch = "wasm32"))]
static PDFIUM_ACCESS: parking_lot::ReentrantMutex<()> = parking_lot::ReentrantMutex::new(());

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn lock() -> parking_lot::ReentrantMutexGuard<'static, ()> {
    PDFIUM_ACCESS.lock()
}

// Each browser worker owns its PDFium runtime and executes synchronously.
#[cfg(target_arch = "wasm32")]
pub(super) fn lock() {}

/// Sessions retain documents between operations without retaining the gate.
/// Closing a document is itself a PDFium operation and must use the same gate.
pub(super) struct Document(Option<pdfium_render::prelude::PdfDocument<'static>>);

impl From<pdfium_render::prelude::PdfDocument<'static>> for Document {
    fn from(document: pdfium_render::prelude::PdfDocument<'static>) -> Self {
        Self(Some(document))
    }
}

impl std::ops::Deref for Document {
    type Target = pdfium_render::prelude::PdfDocument<'static>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        let _pdfium = lock();
        drop(self.0.take());
    }
}
