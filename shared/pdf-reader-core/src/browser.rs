//! PDFium is owned by one dedicated worker. Only Rust values cross its boundary;
//! PDFium handles and JavaScript bindings never leave that worker.

use super::*;
use crate::prefetch::CancelPrefetchOnDrop;
use futures_channel::oneshot;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/browser.js")]
extern "C" {
    #[wasm_bindgen(catch)]
    fn wake_pdf_worker(module: JsValue, memory: JsValue, failed: &js_sys::Function, completed: &js_sys::Function) -> Result<(), JsValue>;
    fn complete_pdf_worker_job(id: &str);
}

type Job = Box<dyn FnOnce() + Send>;
type Completion = Box<dyn FnOnce(Option<String>)>;
static JOBS: OnceLock<concurrent_queue::ConcurrentQueue<Job>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static DOCUMENTS: RefCell<HashMap<u64, PdfDocumentSession>> = RefCell::new(HashMap::new());
    static PENDING: RefCell<HashMap<u64, Completion>> = RefCell::new(HashMap::new());
    static FAILED: RefCell<Option<(String, bool)>> = const { RefCell::new(None) };
    static FAILURE_CALLBACK: Closure<dyn FnMut(String, bool)> = Closure::own_aborting(fail);
    static COMPLETION_CALLBACK: Closure<dyn FnMut(String)> = Closure::own_aborting(complete);
}

fn fail(message: String, retryable: bool) {
    FAILED.with(|state| *state.borrow_mut() = Some((message.clone(), retryable)));
    let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    for (_, reject) in pending {
        reject(Some(message.clone()));
    }
    if let Some(queue) = JOBS.get() {
        while queue.pop().is_ok() {}
    }
}

/// Invoked only by the PDF worker's event loop, after both WASM modules are ready.
#[wasm_bindgen]
pub fn run_pdf_worker_job() {
    if let Some(queue) = JOBS.get() {
        if let Ok(job) = queue.pop() {
            job();
        }
    }
}

fn enqueue(job: Job) -> PdfResult<()> {
    if let Some((error, _)) = FAILED.with(|state| state.borrow().clone()) {
        return Err(PdfError::new(error));
    }
    JOBS.get_or_init(concurrent_queue::ConcurrentQueue::unbounded).push(job).map_err(|error| PdfError::new(error.to_string()))?;
    let result = FAILURE_CALLBACK.with(|failed| COMPLETION_CALLBACK.with(|completed| wake_pdf_worker(wasm_bindgen::module(), wasm_bindgen::memory(), failed.as_ref().unchecked_ref(), completed.as_ref().unchecked_ref())));
    if let Err(error) = result {
        let message = format!("starting PDF worker: {error:?}");
        fail(message.clone(), true);
        return Err(PdfError::new(message));
    }
    Ok(())
}

fn complete(id: String) {
    let Ok(id) = id.parse::<u64>() else { return };
    let completion = PENDING.with(|pending| pending.borrow_mut().remove(&id));
    if let Some(completion) = completion {
        completion(None);
    }
}

async fn request<T: Send + 'static>(work: impl FnOnce() -> PdfResult<T> + Send + 'static) -> PdfResult<T> {
    // Only a new request retries startup; dropping failed sessions must not restart it.
    FAILED.with(|state| {
        let mut state = state.borrow_mut();
        if state.as_ref().is_some_and(|(_, retryable)| *retryable) {
            *state = None;
        }
    });
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let result = Arc::new(concurrent_queue::ConcurrentQueue::bounded(1));
    let worker_result = result.clone();
    let (sender, response) = oneshot::channel();
    PENDING.with(|pending| {
        pending.borrow_mut().insert(
            id,
            Box::new(move |error| {
                let value = match error {
                    Some(error) => Err(PdfError::new(error)),
                    None => result.pop().unwrap_or_else(|_| Err(PdfError::new("PDF worker completed without a result"))),
                };
                let _ = sender.send(value);
            }),
        );
    });
    struct Pending(u64);
    impl Drop for Pending {
        fn drop(&mut self) {
            PENDING.with(|pending| pending.borrow_mut().remove(&self.0));
        }
    }
    let _pending = Pending(id);
    enqueue(Box::new(move || {
        if Arc::strong_count(&worker_result) > 1 {
            let _ = worker_result.push(work());
            complete_pdf_worker_job(&id.to_string());
        }
    }))?;
    // The channel wakes the caller on its own JavaScript thread.
    response.await.map_err(|_| PdfError::new("PDF worker stopped before completing the request"))?
}

struct DocumentHandle(u64);
impl Drop for DocumentHandle {
    fn drop(&mut self) {
        let id = self.0;
        let _ = enqueue(Box::new(move || {
            DOCUMENTS.with(|documents| documents.borrow_mut().remove(&id));
        }));
    }
}

#[derive(Clone)]
pub struct BrowserSession(Rc<LoadedDocument>);

struct LoadedDocument {
    handle: DocumentHandle,
    info: PdfDocumentInfo,
}

impl BrowserSession {
    pub async fn load(load: impl FnOnce() -> PdfResult<PdfDocumentSession> + Send + 'static) -> PdfResult<Self> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        // Owned on the caller before awaiting so cancellation also schedules disposal.
        let handle = DocumentHandle(id);
        let info = request(move || {
            let document = load()?;
            let info = document.info().clone();
            DOCUMENTS.with(|documents| documents.borrow_mut().insert(id, document));
            Ok(info)
        })
        .await?;
        Ok(Self(Rc::new(LoadedDocument { handle, info })))
    }

    pub fn info(&self) -> &PdfDocumentInfo {
        &self.0.info
    }

    async fn with_document<T: Send + 'static>(&self, work: impl FnOnce(&PdfDocumentSession) -> PdfResult<T> + Send + 'static) -> PdfResult<T> {
        let id = self.0.handle.0;
        request(move || {
            DOCUMENTS.with(|documents| {
                let documents = documents.borrow();
                let document = documents.get(&id).ok_or_else(|| PdfError::new("PDF document is closed"))?;
                work(document)
            })
        })
        .await
    }

    pub async fn render(&self, key: PageRenderKey) -> PdfResult<RenderedPdfPage> {
        self.with_document(move |document| document.render_page_with_margin_trim(key.page_index(), Some(key.target_width()), key.trim_margins())).await
    }

    pub async fn prefetch_pages(&self, pages: Vec<usize>) -> PdfResult<()> {
        let cancellation = CancelPrefetchOnDrop::default();
        let cancelled = cancellation.token();
        self.with_document(move |document| document.prefetch_pages_cancellable(&pages, &cancelled)).await
    }

    pub async fn page_geometry(&self, page_index: usize, trim_margins: bool) -> PdfResult<PdfPageGeometry> {
        self.with_document(move |document| document.page_geometry(page_index, trim_margins)).await
    }

    pub async fn reading_fragments(&self, page_index: usize) -> PdfResult<Vec<PdfReadingFragment>> {
        self.with_document(move |document| document.extract_reading_fragments(page_index)).await
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
