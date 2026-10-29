#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::Session;
#[cfg(target_arch = "wasm32")]
pub(crate) use pdf_reader_core::browser::BrowserSession as Session;

use pdf_reader_core::{PdfDocumentSession, PdfResult};

pub(crate) async fn load(load: impl FnOnce() -> PdfResult<PdfDocumentSession> + Send + 'static) -> PdfResult<Session> {
    Session::load(load).await
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use pdf_reader_core::{PageRenderKey, PdfDocumentInfo, PdfDocumentSession, PdfResult, PdfSearchHit, RenderedPdfBgraPage};
    use std::sync::Arc;

    #[derive(Clone)]
    pub(crate) struct Session(Arc<PdfDocumentSession>);

    impl Session {
        pub async fn load(load: impl FnOnce() -> PdfResult<PdfDocumentSession> + Send + 'static) -> PdfResult<Self> {
            Ok(Self(Arc::new(PdfDocumentSession::load_async(load).await?)))
        }

        pub fn info(&self) -> &PdfDocumentInfo {
            self.0.info()
        }

        pub async fn prefetch_pages(&self, pages: Vec<usize>) -> PdfResult<()> {
            self.0.prefetch_pages(pages).await
        }

        pub async fn page_geometry(&self, page_index: usize, trim_margins: bool) -> PdfResult<pdf_reader_core::PdfPageGeometry> {
            self.0.page_geometry(page_index, trim_margins).await
        }

        pub async fn render_for_display(&self, key: PageRenderKey) -> PdfResult<RenderedPdfBgraPage> {
            self.0.render_for_display(key).await
        }

        pub async fn search(&self, pages: std::ops::Range<usize>, query: String) -> PdfResult<Vec<PdfSearchHit>> {
            self.0.search_pages(pages, query).await
        }

        pub async fn reading_fragments(&self, page: usize) -> PdfResult<Vec<pdf_reader_core::PdfReadingFragment>> {
            self.0.reading_fragments(page).await
        }
    }
}
