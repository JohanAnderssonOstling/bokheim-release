use pdf_view_common::PdfPrefetchCancellation;

/// The async caller owns this guard; the synchronous worker owns its token.
/// Dropping a pending UI request also stops work that has already started.
#[derive(Default)]
pub(super) struct CancelPrefetchOnDrop(PdfPrefetchCancellation);

impl CancelPrefetchOnDrop {
    pub fn token(&self) -> PdfPrefetchCancellation {
        self.0.clone()
    }
}

impl Drop for CancelPrefetchOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
