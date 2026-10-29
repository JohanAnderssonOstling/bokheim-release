//! One native worker owns PDFium and every live document. Requests carry Rust
//! values; document handles never cross the worker boundary.
use super::*;
use crate::prefetch::CancelPrefetchOnDrop;
use std::cell::Cell;
use std::sync::mpsc::{self, Sender};

type Job = Box<dyn FnOnce() + Send + 'static>;
static WORKER: OnceLock<Result<Sender<Job>, String>> = OnceLock::new();
thread_local! {
    static ON_WORKER: Cell<bool> = const { Cell::new(false) };
    static DOCUMENTS: RefCell<HashMap<u64, LocalDocumentSession>> = RefCell::new(HashMap::new());
    static NEXT_DOCUMENT: Cell<u64> = const { Cell::new(1) };
}

pub(super) fn on_worker() -> bool {
    ON_WORKER.get()
}

fn enqueue(job: Job) -> PdfResult<()> {
    let worker = WORKER.get_or_init(|| {
        let (sender, requests) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("pdfium".into())
            .spawn(move || {
                ON_WORKER.set(true);
                for job in requests {
                    job();
                }
            })
            .map_err(|error| format!("starting PDFium worker: {error}"))?;
        Ok(sender)
    });
    worker.as_ref().map_err(|error| PdfError::new(error.clone()))?.send(job).map_err(|_| PdfError::new("PDFium worker stopped"))
}

pub(super) fn run<T: Send + 'static>(work: impl FnOnce() -> PdfResult<T> + Send + 'static) -> PdfResult<T> {
    // Constructors may be called inside load_async() or another worker request.
    if on_worker() {
        return work();
    }
    let (reply, received) = mpsc::sync_channel(1);
    enqueue(Box::new(move || {
        let _ = reply.send(work());
    }))?;
    received.recv().map_err(|_| PdfError::new("PDFium worker stopped without replying"))?
}

async fn request<T: Send + 'static>(work: impl FnOnce() -> PdfResult<T> + Send + 'static) -> PdfResult<T> {
    let (reply, received) = async_channel::bounded(1);
    enqueue(Box::new(move || {
        if !reply.is_closed() {
            let _ = reply.try_send(work());
        }
    }))?;
    received.recv().await.map_err(|_| PdfError::new("PDFium worker stopped without replying"))?
}

struct DocumentHandle(u64);
impl Drop for DocumentHandle {
    fn drop(&mut self) {
        let id = self.0;
        // Disposal is queued even if the last handle is dropped on the worker.
        let _ = enqueue(Box::new(move || {
            DOCUMENTS.with_borrow_mut(|documents| documents.remove(&id));
        }));
    }
}

pub struct PdfDocumentSession {
    handle: Arc<DocumentHandle>,
    info: PdfDocumentInfo,
}

impl PdfDocumentSession {
    fn load(load: impl FnOnce() -> PdfResult<LocalDocumentSession> + Send + 'static) -> PdfResult<Self> {
        run(move || {
            let document = load()?;
            let info = document.info().clone();
            let id = NEXT_DOCUMENT.with(|next| {
                let id = next.get();
                next.set(id.checked_add(1).ok_or_else(|| PdfError::new("PDF document IDs exhausted"))?);
                Ok::<_, PdfError>(id)
            })?;
            DOCUMENTS.with_borrow_mut(|documents| documents.insert(id, document));
            Ok(Self { handle: Arc::new(DocumentHandle(id)), info })
        })
    }

    pub async fn load_async(load: impl FnOnce() -> PdfResult<Self> + Send + 'static) -> PdfResult<Self> {
        request(load).await
    }

    pub fn open(path: impl Into<PathBuf>) -> PdfResult<Self> {
        let path = path.into();
        Self::load(move || LocalDocumentSession::open(path))
    }

    pub fn from_bytes(source_name: impl Into<String>, bytes: Vec<u8>) -> PdfResult<Self> {
        let source_name = source_name.into();
        Self::load(move || LocalDocumentSession::from_bytes(source_name, bytes))
    }

    pub fn from_reader<R: Read + Seek + Send + 'static>(source_name: impl Into<String>, reader: R) -> PdfResult<Self> {
        let source_name = source_name.into();
        Self::load(move || LocalDocumentSession::from_reader(source_name, reader))
    }

    pub fn from_reader_with_metadata<R: Read + Seek + Send + 'static>(source_name: impl Into<String>, reader: R, content_hash: &str, metadata: Option<PdfReaderMetadata>) -> PdfResult<Self> {
        let source_name = source_name.into();
        let content_hash = content_hash.to_owned();
        Self::load(move || LocalDocumentSession::from_reader_with_metadata(source_name, reader, &content_hash, metadata))
    }

    pub fn from_reader_with_page_source<R: Read + Seek + Send + 'static>(source_name: impl Into<String>, reader: R, content_hash: &str, metadata: Option<PdfReaderMetadata>, page_source: Option<Arc<dyn PdfPageSource>>) -> PdfResult<Self> {
        let source_name = source_name.into();
        let content_hash = content_hash.to_owned();
        Self::load(move || LocalDocumentSession::from_reader_with_page_source(source_name, reader, &content_hash, metadata, page_source))
    }

    pub fn from_reader_with_opening_page<R: Read + Seek + Send + 'static>(
        source_name: impl Into<String>, reader: R, content_hash: &str, metadata: Option<PdfReaderMetadata>, page_source: Option<Arc<dyn PdfPageSource>>, opening_page: Option<usize>,
    ) -> PdfResult<Self> {
        let source_name = source_name.into();
        let content_hash = content_hash.to_owned();
        Self::load(move || LocalDocumentSession::from_reader_with_opening_page(source_name, reader, &content_hash, metadata, page_source, opening_page))
    }

    pub fn info(&self) -> &PdfDocumentInfo {
        &self.info
    }

    fn work<T: Send + 'static>(&self, work: impl FnOnce(&mut LocalDocumentSession) -> PdfResult<T> + Send + 'static) -> impl FnOnce() -> PdfResult<T> + Send + 'static {
        let handle = self.handle.clone();
        move || DOCUMENTS.with_borrow_mut(|documents| work(documents.get_mut(&handle.0).ok_or_else(|| PdfError::new("PDF document is closed"))?))
    }

    pub fn inspect_skippable_pages(&self) -> Vec<usize> {
        // Failure must retain pages, rather than classify unreadable content as blank.
        run(self.work(|document| Ok(document.inspect_skippable_pages()))).unwrap_or_default()
    }

    pub fn set_skippable_pages(&mut self, pages: Vec<usize>) {
        self.info.skippable_pages = pages.clone();
        let _ = run(self.work(move |document| {
            document.set_skippable_pages(pages);
            Ok(())
        }));
    }

    pub fn render_page(&self, page_index: usize, target_width: Option<u16>) -> PdfResult<RenderedPdfPage> {
        self.render_page_with_margin_trim(page_index, target_width, false)
    }

    pub fn render_page_with_margin_trim(&self, page_index: usize, target_width: Option<u16>, trim_margins: bool) -> PdfResult<RenderedPdfPage> {
        run(self.work(move |document| document.render_page_with_margin_trim(page_index, target_width, trim_margins)))
    }

    pub fn render_page_for_display(&self, key: PageRenderKey) -> PdfResult<RenderedPdfBgraPage> {
        run(self.work(move |document| document.render_page_for_display(key)))
    }

    pub async fn prefetch_pages(&self, pages: Vec<usize>) -> PdfResult<()> {
        let cancellation = CancelPrefetchOnDrop::default();
        let cancelled = cancellation.token();
        request(self.work(move |document| document.prefetch_pages_cancellable(&pages, &cancelled))).await
    }

    pub async fn page_geometry(&self, page_index: usize, trim_margins: bool) -> PdfResult<PdfPageGeometry> {
        request(self.work(move |document| document.page_geometry(page_index, trim_margins))).await
    }

    pub async fn render_for_display(&self, key: PageRenderKey) -> PdfResult<RenderedPdfBgraPage> {
        request(self.work(move |document| document.render_page_for_display(key))).await
    }

    pub fn search(&self, query: &str) -> PdfResult<Vec<PdfSearchHit>> {
        let query = query.to_owned();
        run(self.work(move |document| document.search(&query)))
    }

    pub fn search_page(&self, page_index: usize, query: &str) -> PdfResult<Vec<PdfSearchHit>> {
        let query = query.to_owned();
        run(self.work(move |document| document.search_page(page_index, &query)))
    }

    pub async fn search_pages(&self, mut pages: std::ops::Range<usize>, query: String) -> PdfResult<Vec<PdfSearchHit>> {
        request(self.work(move |document| {
            pages.try_fold(Vec::new(), |mut hits, page| {
                hits.extend(document.search_page(page, &query)?);
                Ok(hits)
            })
        }))
        .await
    }

    /// Extracts text on the owning PDFium worker without rasterizing the page.
    pub async fn reading_fragments(&self, page_index: usize) -> PdfResult<Vec<PdfReadingFragment>> {
        request(self.work(move |document| document.extract_reading_fragments(page_index))).await
    }

    pub fn character_index_near_point(&self, page_index: usize, point: (f32, f32), tolerance: (f32, f32)) -> PdfResult<Option<usize>> {
        run(self.work(move |document| document.character_index_near_point(page_index, point, tolerance)))
    }

    pub(super) fn for_inspection(reader: impl Read + Seek + Send + 'static) -> PdfResult<Self> {
        Self::load(move || {
            let document = load_document(reader)?;
            let right_to_left = document.catalog().is_right_to_left();
            Ok(LocalDocumentSession {
                document,
                info: PdfDocumentInfo { source_name: String::new(), pages: Vec::new(), outline: Vec::new(), skippable_pages: Vec::new(), right_to_left },
                analysis: RefCell::new(PageAnalysisCache::default()),
                page_source: None,
            })
        })
    }

    pub(super) fn inspection_image(&self, number: usize) -> PdfResult<Option<RenderedPdfImage>> {
        run(self.work(move |document| {
            let Some(index) = number.checked_sub(1).and_then(|index| i32::try_from(index).ok()) else { return Ok(None) };
            let Ok(page) = document.document.pages().get(index) else { return Ok(None) };
            let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(2200).set_maximum_width(2600).set_maximum_height(3200)).map_err(|error| PdfError::new(format!("rendering inspection page {number}: {error}")))?;
            Ok(Some(RenderedPdfImage { rgba: bitmap.as_rgba_bytes(), pixel_width: bitmap.width() as u32, pixel_height: bitmap.height() as u32, source: RenderedPdfImageSource::RenderedPage }))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::io::{Cursor, Result as IoResult};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread::{self, Thread, ThreadId};
    use std::time::Duration;

    struct WakeThread(Thread);
    impl Wake for WakeThread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let waker = Waker::from(Arc::new(WakeThread(thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => thread::park(),
            }
        }
    }

    struct TrackedReader {
        bytes: Cursor<Vec<u8>>,
        calls: Sender<ThreadId>,
        dropped: Sender<ThreadId>,
    }
    impl Read for TrackedReader {
        fn read(&mut self, bytes: &mut [u8]) -> IoResult<usize> {
            let _ = self.calls.send(thread::current().id());
            self.bytes.read(bytes)
        }
    }
    impl Seek for TrackedReader {
        fn seek(&mut self, position: SeekFrom) -> IoResult<u64> {
            let _ = self.calls.send(thread::current().id());
            self.bytes.seek(position)
        }
    }
    impl Drop for TrackedReader {
        fn drop(&mut self) {
            let _ = self.dropped.send(thread::current().id());
        }
    }

    #[test]
    fn reader_operations_and_disposal_stay_on_the_pdfium_worker() -> PdfResult<()> {
        let owner = run(|| Ok(thread::current().id()))?;
        assert_ne!(owner, thread::current().id());
        let (calls, observed) = mpsc::channel();
        let (dropped, disposed) = mpsc::channel();
        let session = PdfDocumentSession::from_reader("tracked", TrackedReader { bytes: Cursor::new(include_bytes!("../tests/fixtures/cover-annotations.pdf").to_vec()), calls, dropped })?;
        let id = session.handle.0;
        session.render_page(0, Some(320))?;
        session.search_page(0, "text")?;
        session.character_index_near_point(0, (0.5, 0.5), (1.0, 1.0))?;
        block_on(session.render_for_display(PageRenderKey::new(0, 320)))?;
        block_on(session.search_pages(0..1, "text".into()))?;
        let reads = observed.try_iter().collect::<Vec<_>>();
        assert!(!reads.is_empty());
        assert!(reads.into_iter().all(|thread| thread == owner));
        drop(session);
        assert!(!run(move || Ok(DOCUMENTS.with_borrow(|documents| documents.contains_key(&id))))?);
        assert_eq!(disposed.recv_timeout(Duration::from_secs(5)).unwrap(), owner);
        Ok(())
    }

    #[test]
    fn cancelling_a_queued_request_skips_the_work() -> PdfResult<()> {
        let (started, active) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        enqueue(Box::new(move || {
            let _ = started.send(());
            let _ = blocked.recv();
        }))?;
        active.recv_timeout(Duration::from_secs(5)).unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let marker = ran.clone();
        let mut future = Box::pin(request(move || {
            marker.store(true, Ordering::SeqCst);
            Ok(())
        }));
        assert!(future.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        drop(future);
        release.send(()).unwrap();
        run(|| Ok(()))?;
        assert!(!ran.load(Ordering::SeqCst));
        Ok(())
    }

    #[test]
    fn cancelling_an_active_load_disposes_its_document() -> PdfResult<()> {
        let (opened, active) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut future = Box::pin(PdfDocumentSession::load_async(move || {
            let session = PdfDocumentSession::from_bytes("cancelled", include_bytes!("../tests/fixtures/cover-annotations.pdf").to_vec())?;
            let _ = opened.send(session.handle.0);
            let _ = blocked.recv();
            Ok(session)
        }));
        assert!(future.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        let id = active.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(future);
        release.send(()).unwrap();
        // A cancelled reply can queue disposal behind the first barrier.
        run(|| Ok(()))?;
        assert!(!run(move || Ok(DOCUMENTS.with_borrow(|documents| documents.contains_key(&id))))?);
        Ok(())
    }
}
