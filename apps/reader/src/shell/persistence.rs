//! Asynchronous library persistence coordinated by the reader.
//!
//! The backend owns any platform-specific blocking work. This module only
//! folds loaded data back into its entity, serializes optimistic mutations,
//! and debounces position updates.

use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Context};
use library_backend::LibraryClient;

pub(crate) type PersistenceFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + 'static>>;
type Mutation = Box<dyn FnOnce(&LibraryClient) -> PersistenceFuture<()> + 'static>;

/// Awaits `op` against the library, then applies `apply` to the view with its
/// result.
///
/// `what` names the write for log messages, phrased to follow "failed to" —
/// for example `"save annotation"`. If the operation fails, the failure is
/// logged and `apply` is not called.
pub(crate) fn write<V, T>(view: &Context<V>, library: Rc<LibraryClient>, what: &'static str, op: impl FnOnce(&LibraryClient) -> PersistenceFuture<T> + 'static, apply: impl FnOnce(&mut V, &mut Context<V>, T) + 'static)
where
    V: 'static,
    T: 'static,
{
    view.spawn(async move |this, cx| {
        let result = op(library.as_ref()).await;
        let _ = this.update(cx, |this, cx| match result {
            Ok(value) => apply(this, cx, value),
            Err(error) => log::error!("failed to {what}: {error}"),
        });
    })
    .detach();
}

/// Serializes durable mutations independently of UI updates.
///
/// Reader state is changed optimistically on the GPUI thread. This queue then
/// preserves the same order in storage.
pub(crate) struct MutationWriter {
    mutations: async_channel::Sender<(&'static str, Mutation)>,
}

impl MutationWriter {
    pub(crate) fn new(cx: &App, library: Rc<LibraryClient>) -> Self {
        let (mutations, incoming) = async_channel::unbounded::<(&'static str, Mutation)>();
        cx.spawn(async move |_cx| {
            while let Ok((what, mutation)) = incoming.recv().await {
                if let Err(error) = mutation(library.as_ref()).await {
                    log::error!("failed to {what}: {error}");
                }
            }
        })
        .detach();
        Self { mutations }
    }

    pub(crate) fn record(&self, what: &'static str, mutation: impl FnOnce(&LibraryClient) -> PersistenceFuture<()> + 'static) {
        if self.mutations.try_send((what, Box::new(mutation))).is_err() {
            log::error!("failed to queue {what}");
        }
    }
}

/// How long a position update waits for a quieter one to supersede it.
const POSITION_DEBOUNCE: Duration = Duration::from_millis(250);

/// Coalesces a stream of reading-position updates into occasional library
/// writes.
///
/// Page turns arrive far faster than the database should be written. Each
/// update waits out [`POSITION_DEBOUNCE`]; anything that queued up meanwhile is
/// drained and only the newest position is persisted.
pub(crate) struct PositionWriter<P> {
    updates: async_channel::Sender<P>,
}

impl<P: 'static> PositionWriter<P> {
    /// Starts the debouncing task. `persist` performs the actual library
    /// write for the newest position; `what` names it for log messages.
    pub(crate) fn new(cx: &App, what: &'static str, persist: impl Fn(&LibraryClient, &P) -> PersistenceFuture<()> + 'static, library: Rc<LibraryClient>) -> Self {
        let (updates, incoming) = async_channel::unbounded::<P>();
        let executor = cx.background_executor().clone();
        let persist = Rc::new(persist);
        cx.spawn(async move |_cx| {
            while let Ok(mut position) = incoming.recv().await {
                executor.timer(POSITION_DEBOUNCE).await;
                while let Ok(newer) = incoming.try_recv() {
                    position = newer;
                }
                if let Err(error) = persist(library.as_ref(), &position).await {
                    log::error!("failed to {what}: {error}");
                }
            }
        })
        .detach();
        Self { updates }
    }

    /// Records a new position. Dropping it because the task has stopped is
    /// logged once per update and is not otherwise fatal.
    pub(crate) fn record(&self, position: P, what: &'static str) {
        if self.updates.try_send(position).is_err() {
            log::warn!("{what} persistence task stopped");
        }
    }
}

/// One reading position as the library stores it.
///
/// Every format reduces to this pair before persistence, so no reader decides
/// on its own whether to report progress or in which unit. `percent` is always
/// 0–100: readers work in their own coordinates — an EPUB layout fraction, a
/// page out of a count — and convert exactly once, in the constructor for
/// their format.
pub(crate) struct StoredPosition {
    position: String,
    percent: f32,
    /// The navigation entry this position falls inside, where the reader knows
    /// it. Only the renderer can resolve a CFI or a page to an entry, so the
    /// library cannot work it out later — it travels with the position instead.
    entry: Option<String>,
}

impl StoredPosition {
    /// The renderer reports how far through the book layout has reached as a
    /// 0–1 fraction.
    pub(crate) fn epub(cfi: String, fraction: f32) -> Self {
        Self { position: cfi, percent: fraction.clamp(0.0, 1.0) * 100.0, entry: None }
    }

    /// Pages are a PDF's only progress signal: there is no reflowed text to
    /// weigh, so page N of M is both what the toolbar shows and what is
    /// stored. `offset` locates the view within the page and rides along in
    /// the position string without affecting progress.
    pub(crate) fn pdf_page(page_index: usize, offset: f32, page_count: usize) -> Self {
        let percent = if page_count == 0 { 0.0 } else { (page_index.saturating_add(1) as f32 / page_count as f32 * 100.0).clamp(0.0, 100.0) };
        Self { position: book_model::pdf_page_position_reading_position(page_index as u32, offset), percent, entry: Some(book_model::pdf_toc_target(page_index)) }
    }

    /// The entry the position was resolved to, once the reader knows it. An
    /// EPUB learns this from a separate renderer event than the one that moves
    /// the position, so it is attached rather than constructed.
    pub(crate) fn within(mut self, entry: Option<String>) -> Self {
        self.entry = entry;
        self
    }

    /// The stored 0–100, so a toolbar can show exactly what was persisted.
    pub(crate) fn percent(&self) -> f32 {
        self.percent
    }
}

/// The debounced writer for the reading-position register.
///
/// Wraps [`PositionWriter`] with the one payload and the one library call that
/// every format shares, leaving readers nothing to vary but their own
/// coordinates.
pub(crate) struct ReadingPositionWriter {
    writer: PositionWriter<StoredPosition>,
}

impl ReadingPositionWriter {
    pub(crate) fn new(cx: &App, content_hash: app::ContentHash, library: Rc<LibraryClient>) -> Self {
        let writer = PositionWriter::new(
            cx,
            "save reading position",
            move |library, position: &StoredPosition| {
                let content_hash = content_hash.clone();
                let stored = position.position.clone();
                let percent = position.percent;
                let entry = position.entry.clone();
                let library = library.clone();
                Box::pin(async move { library.update_reading_position(content_hash, stored, Some(percent), entry).await })
            },
            library,
        );
        Self { writer }
    }

    pub(crate) fn record(&self, position: StoredPosition) {
        self.writer.record(position, "reading position");
    }
}

#[cfg(test)]
mod tests {
    use super::StoredPosition;

    #[test]
    fn an_epub_layout_fraction_is_stored_as_a_percentage() {
        let position = StoredPosition::epub("epubcfi(/6/24!/4/2)".to_owned(), 0.55);
        assert_eq!(position.position, "epubcfi(/6/24!/4/2)");
        assert!((position.percent() - 55.0).abs() < 0.01, "the library stores 0-100, not the renderer's 0-1");
    }

    #[test]
    fn an_epub_overshoot_cannot_store_more_than_full() {
        assert_eq!(StoredPosition::epub(String::new(), 1.004).percent(), 100.0);
        assert_eq!(StoredPosition::epub(String::new(), -0.5).percent(), 0.0);
    }

    #[test]
    fn a_pdf_page_is_stored_as_its_share_of_the_document() {
        assert_eq!(StoredPosition::pdf_page(0, 0.0, 4).percent(), 25.0);
        assert_eq!(StoredPosition::pdf_page(3, 0.0, 4).percent(), 100.0);
        assert_eq!(StoredPosition::pdf_page(9, 0.0, 4).percent(), 100.0, "a page past the count cannot exceed full");
    }

    #[test]
    fn a_pdf_with_no_pages_reports_no_progress() {
        assert_eq!(StoredPosition::pdf_page(0, 0.0, 0).percent(), 0.0);
    }

    #[test]
    fn a_pdf_position_carries_the_page_offset() {
        assert_eq!(StoredPosition::pdf_page(17, 0.5, 100).position, "pdfpage(17,0.5)");
    }
}
