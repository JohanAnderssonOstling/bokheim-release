#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::Session;
#[cfg(target_arch = "wasm32")]
pub(crate) use pdf_reader_core::browser::BrowserSession as Session;

use pdf_reader_core::{PdfDocumentSession, PdfResult};

pub(crate) async fn load(executor: gpui::BackgroundExecutor, load: impl FnOnce() -> PdfResult<PdfDocumentSession> + Send + 'static) -> PdfResult<Session> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = executor;
        Session::load(load).await
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Session::load(executor, load).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use gpui::BackgroundExecutor;
    use pdf_reader_core::{PageRenderKey, PdfDocumentInfo, PdfDocumentSession, PdfResult, PdfSearchHit, RenderedPdfBgraPage};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    pub(crate) struct Session(Arc<LoadedDocument>);

    struct LoadedDocument {
        document: Mutex<PdfDocumentSession>,
        info: PdfDocumentInfo,
        executor: BackgroundExecutor,
    }

    impl Session {
        pub async fn load(executor: BackgroundExecutor, load: impl FnOnce() -> PdfResult<PdfDocumentSession> + Send + 'static) -> PdfResult<Self> {
            let document = executor.spawn(async move { load() }).await?;
            let info = document.info().clone();
            Ok(Self(Arc::new(LoadedDocument { document: Mutex::new(document), info, executor })))
        }

        pub fn info(&self) -> &PdfDocumentInfo {
            &self.0.info
        }

        async fn with_document<T: Send + 'static>(&self, work: impl FnOnce(&PdfDocumentSession) -> PdfResult<T> + Send + 'static) -> PdfResult<T> {
            let state = self.0.clone();
            self.0
                .executor
                .spawn(async move {
                    let document = state.document.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    work(&document)
                })
                .await
        }

        pub async fn render_for_display(&self, key: PageRenderKey) -> PdfResult<RenderedPdfBgraPage> {
            self.with_document(move |document| document.render_page_for_display(key)).await
        }

        pub async fn search(&self, mut pages: std::ops::Range<usize>, query: String) -> PdfResult<Vec<PdfSearchHit>> {
            self.with_document(move |document| {
                pages.try_fold(Vec::new(), |mut hits, page| {
                    hits.extend(document.search_page(page, &query)?);
                    Ok(hits)
                })
            })
            .await
        }
    }
}
