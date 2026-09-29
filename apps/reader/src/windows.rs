//! Reader page lifecycle inside the application's single window.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

use gpui::prelude::*;
use gpui::{AnimationExt, AnyView, App, AppContext, Context, EventEmitter, IntoElement, Render, SharedString, Task, Window};
use html_book::BookSourceFormat;
use library_backend::BookFormat;
use library_backend::BoxedBookReader;
use library_backend::LibraryClient;
use library_model::BookLocator;

use crate::epub::ReaderView as EpubReaderView;
use crate::pdf::PdfReaderView;
use crate::settings::{ReaderPreferences, ReaderSettings, SaveReaderSettings};

use crate::CloseReader;

pub struct CloseRequested;
#[cfg(feature = "audiobooks")]
pub struct AudiobookOpened(pub gpui::Entity<audiobook_player::PlaybackSession>);

enum PreparedBook {
    Book {
        book: Arc<crate::book::NativeBook>,
        preparation: html_view_core::RendererPreparation,
    },
    Pdf {
        page_source: Option<std::sync::Arc<dyn pdf_reader_core::PdfPageSource>>,
        metadata: Option<pdf_reader_core::PdfReaderMetadata>,
        reader: BoxedBookReader,
        initial_page: usize,
        initial_full_page_position: f32,
    },
    #[cfg(feature = "audiobooks")]
    Audiobook {
        resolved: library_backend::ResolvedBook,
        initial_target: Option<String>,
    },
}

enum ReaderState {
    Loading { stage: SharedString, download: Option<app::DownloadState> },
    Ready(AnyView),
    Error(SharedString),
}

enum ReaderLoadUpdate {
    Stage(String),
    Download(app::DownloadState),
}

/// The reader page installed directly in the application's single window.
/// It owns book loading and replaces its loading state with the
/// format-specific reader when a document is ready. Audiobooks hand their
/// playback session to the application shell instead.
pub struct ReaderView {
    title: SharedString,
    state: ReaderState,
    _load_task: Option<Task<()>>,
}

pub fn configure(preferences: ReaderPreferences, save: SaveReaderSettings, cx: &mut App) {
    cx.set_global(ReaderSettings { preferences, save });
}

fn report_stage(updates: &async_channel::Sender<ReaderLoadUpdate>, stage: impl Into<String>) {
    let _ = updates.try_send(ReaderLoadUpdate::Stage(stage.into()));
}

async fn prepare_pdf(
    locator: &BookLocator, content: library_backend::ResolvedBook, library: &Rc<LibraryClient>, initial_target: Option<String>, startup_started: Instant, updates: &async_channel::Sender<ReaderLoadUpdate>,
) -> Result<PreparedBook, String> {
    let phase_started = Instant::now();
    report_stage(updates, "Loading PDF reading position…");
    let content_hash = locator.content_hash();
    let (stored_page, stored_position) = library.pdf_reading_position(content_hash).await.map_err(|error| format!("stage=reading_position: {error}"))?.unwrap_or((0, 0.0));
    // A requested destination outranks the stored position: it is the reason
    // the book was opened. A contents row asks for `pdf-page:<n>`; anything
    // else is a target for another format and is ignored rather than guessed at.
    let (initial_page, initial_full_page_position) = match initial_target.as_deref().and_then(book_model::pdf_toc_page) {
        Some(page) => (page as u32, 0.0),
        None => (stored_page, stored_position),
    };
    startup_phase!("phase=reading_position_ready content_hash={} elapsed_ms={} phase_ms={} format=pdf", locator.content_hash(), startup_started.elapsed().as_millis(), phase_started.elapsed().as_millis());
    let document = content.into_document()?;
    Ok(PreparedBook::Pdf { page_source: document.pdf_page_source, metadata: document.pdf_metadata, reader: document.reader, initial_page: initial_page as usize, initial_full_page_position })
}

async fn prepare_book(
    locator: &BookLocator, content: library_backend::ResolvedBook, format: BookFormat, library: &Rc<LibraryClient>, executor: &gpui::BackgroundExecutor, renderer_config: html_view_core::RendererInitialConfig,
    initial_target: Option<String>, startup_started: Instant, updates: &async_channel::Sender<ReaderLoadUpdate>,
) -> Result<PreparedBook, String> {
    let phase_started = Instant::now();
    let book_format = match format {
        BookFormat::Epub => BookSourceFormat::Epub,
        BookFormat::Mobi => BookSourceFormat::Mobi,
        _ => return Err(format!("stage=prepare: {} is not a book format", format.canonical_extension())),
    };
    report_stage(updates, "Opening book…");
    let content = content.into_document()?;
    let book = executor.spawn(async move { crate::book::NativeBook::from_reader(book_format, content.reader).map_err(|error| format!("stage=book_open: {error}")) }).await?;
    startup_phase!("phase=book_opened content_hash={} elapsed_ms={} phase_ms={}", locator.content_hash(), startup_started.elapsed().as_millis(), phase_started.elapsed().as_millis());
    report_stage(updates, "Book parsed; loading reading position…");
    let content_hash = locator.content_hash();
    let nav_state = library.reading_position(content_hash).await.map_err(|error| format!("stage=reading_position: {error}"))?;
    report_stage(updates, "Preparing first document…");
    let book = Arc::new(book);
    let prepare_book = book.clone();
    let preparation = executor
        .spawn(async move {
            html_view_core::RendererPreparation::from_provider_with_nav_and_target(
                prepare_book.provider.clone(),
                prepare_book.document_uris.clone(),
                prepare_book.start_index,
                nav_state.as_deref(),
                initial_target.as_deref(),
                renderer_config,
            )
            .map_err(|error| format!("stage=document_prepare: {error}"))
        })
        .await?;
    Ok(PreparedBook::Book { book, preparation })
}

// The subscription is owned by this future: completion, failure, and dropping
// the loading task all release it without waiting for another download event.
#[cfg(not(target_arch = "wasm32"))]
async fn with_download_progress<T>(operation: impl std::future::Future<Output = T>, states: async_channel::Receiver<app::DownloadState>, updates: &async_channel::Sender<ReaderLoadUpdate>) -> T {
    use futures_util::future::{Either, select};
    let mut operation = Box::pin(operation);
    loop {
        match select(operation, Box::pin(states.recv())).await {
            Either::Left((result, _)) => return result,
            Either::Right((Ok(state), pending)) => {
                let _ = updates.try_send(ReaderLoadUpdate::Download(state));
                operation = pending;
            }
            Either::Right((Err(_), pending)) => return pending.await,
        }
    }
}

async fn load_book(
    locator: BookLocator, library: Rc<LibraryClient>, executor: gpui::BackgroundExecutor, renderer_config: html_view_core::RendererInitialConfig, initial_target: Option<String>, startup_started: Instant,
    updates: async_channel::Sender<ReaderLoadUpdate>,
) -> Result<PreparedBook, String> {
    let prepare_locator = locator;
    let phase_started = Instant::now();
    report_stage(&updates, "Preparing book; downloading it if needed…");
    let content_hash = prepare_locator.content_hash();
    #[cfg(not(target_arch = "wasm32"))]
    let content = match library.download_changes(content_hash).await {
        Ok((download_states, initial)) => {
            let _ = updates.try_send(ReaderLoadUpdate::Download(initial));
            with_download_progress(library.resolve_book(content_hash), download_states, &updates).await
        }
        Err(error) => {
            log::warn!("cannot subscribe to download progress for {content_hash}: {error}");
            library.resolve_book(content_hash).await
        }
    }
    .map_err(|error| format!("stage=book_resolve: {error}"))?;
    #[cfg(target_arch = "wasm32")]
    let content = {
        use futures_util::future::{Either, select};

        report_stage(&updates, "Sending request to backend…");
        let diagnostics = library.resolve_book_diagnostics();
        let mut resolve = Box::pin(library.resolve_book(content_hash));
        loop {
            match select(resolve, Box::pin(diagnostics.recv())).await {
                Either::Left((result, _)) => break result.map_err(|error| format!("stage=book_resolve: {error}"))?,
                Either::Right((Ok(message), pending_resolve)) => {
                    let stage = message.split_whitespace().find_map(|field| field.strip_prefix("phase=")).unwrap_or("working").replace('_', " ");
                    report_stage(&updates, format!("Backend: {stage}…"));
                    resolve = pending_resolve;
                }
                Either::Right((Err(_), pending_resolve)) => {
                    resolve = pending_resolve;
                }
            }
        }
    };
    startup_phase!("phase=content_resolved content_hash={} elapsed_ms={} phase_ms={}", prepare_locator.content_hash(), startup_started.elapsed().as_millis(), phase_started.elapsed().as_millis());
    report_stage(&updates, "Book is available; detecting format…");
    let format = content.format();

    let prepared = match format {
        BookFormat::Pdf => prepare_pdf(&prepare_locator, content, &library, initial_target, startup_started, &updates).await?,
        #[cfg(feature = "audiobooks")]
        BookFormat::M4b | BookFormat::Mp3Folder => PreparedBook::Audiobook { resolved: content, initial_target },
        #[cfg(not(feature = "audiobooks"))]
        BookFormat::M4b | BookFormat::Mp3Folder => return Err("stage=prepare: audiobook support is disabled".to_owned()),
        BookFormat::Epub | BookFormat::Mobi => prepare_book(&prepare_locator, content, format, &library, &executor, renderer_config, initial_target, startup_started, &updates).await?,
    };
    Ok(prepared)
}

impl ReaderView {
    pub fn loading_error(&self) -> Option<SharedString> {
        match &self.state {
            ReaderState::Error(error) => Some(error.clone()),
            _ => None,
        }
    }
    fn request_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(CloseRequested);
    }

    pub fn new(locator: BookLocator, title: String, initial_target: Option<String>, app: app::AppClient, library: Rc<LibraryClient>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let startup_started = Instant::now();
        startup_phase!("phase=request content_hash={} elapsed_ms=0", locator.content_hash());
        #[cfg(feature = "kobo")]
        if let Err(error) = gpui_kobo::begin_book_launch_profile(startup_started) {
            log::error!("failed to start Kobo book launch profile: {error}");
        }
        window.set_window_title(&title);
        if ui_components::uses_mobile_navigation(window) {
            cx.set_system_bars_visible(true);
        }
        let reader_view = cx.entity().downgrade();
        let close_reader: CloseReader = Rc::new(move |_, cx| {
            let _ = reader_view.update(cx, Self::request_close);
        });
        let mut reader = Self { title: title.clone().into(), state: ReaderState::Loading { stage: SharedString::from("Starting book load…"), download: None }, _load_task: None };
        reader.start_load(locator, title, initial_target, app, library, close_reader, startup_started, window, cx);
        reader
    }

    fn start_load(
        &mut self, locator: BookLocator, title: String, initial_target: Option<String>, app: app::AppClient, library: Rc<LibraryClient>, close_reader: CloseReader, startup_started: Instant, _window: &mut Window, cx: &mut Context<Self>,
    ) {
        let load_locator = locator.clone();
        let executor = cx.background_executor().clone();
        let renderer_config = crate::epub::state::renderer_initial_config(cx);
        let (load_updates, updates_rx) = async_channel::unbounded::<ReaderLoadUpdate>();
        let updates_task = cx.spawn(async move |reader_view, cx| {
            while let Ok(update) = updates_rx.recv().await {
                if reader_view
                    .update(cx, |reader_view, cx| {
                        if let ReaderState::Loading { stage, download } = &mut reader_view.state {
                            match update {
                                ReaderLoadUpdate::Stage(next) => *stage = SharedString::from(next),
                                ReaderLoadUpdate::Download(next) => *download = Some(next),
                            }
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let load_task = cx.spawn(async move |reader_view, cx| {
            let _updates_task = updates_task;
            let result = load_book(load_locator, library.clone(), executor, renderer_config, initial_target, startup_started, load_updates).await;
            match result {
                Ok(book) => {
                    let _ = reader_view.update_in(cx, move |reader_view, window, cx| {
                        reader_view.state = ReaderState::Loading { stage: SharedString::from("Building reader interface…"), download: None };
                        let view: AnyView = match book {
                            PreparedBook::Pdf { reader, metadata, page_source, initial_page, initial_full_page_position } => {
                                cx.new(|cx| PdfReaderView::new(locator, title, app, library, reader, metadata, page_source, initial_page, initial_full_page_position, close_reader, window, cx)).into()
                            }
                            PreparedBook::Book { book, preparation } => cx.new(|cx| EpubReaderView::new(locator, title, app, library, book, preparation, close_reader, window, cx)).into(),
                            #[cfg(feature = "audiobooks")]
                            PreparedBook::Audiobook { resolved, initial_target } => {
                                // Narration speed lives with the reader's other
                                // settings, so the player is handed the saved
                                // one and reports changes back the same way the
                                // document readers do.
                                let speed = crate::settings::ReaderSettings::preferences(cx).audiobook_speed;
                                let save_speed: std::rc::Rc<dyn Fn(f64, &mut gpui::App)> = std::rc::Rc::new(|speed, cx| {
                                    crate::settings::ReaderSettings::update(cx, |preferences| preferences.audiobook_speed = speed);
                                });
                                let session = cx.new(|cx| audiobook_player::PlaybackSession::open(locator, library.as_ref().clone(), resolved, initial_target, speed, save_speed, cx));
                                cx.emit(AudiobookOpened(session));
                                return;
                            }
                        };
                        startup_phase!("phase=reader_ready elapsed_ms={}", startup_started.elapsed().as_millis());
                        reader_view.state = ReaderState::Ready(view);
                        cx.notify();
                    });
                }
                Err(error) => {
                    #[cfg(target_arch = "wasm32")]
                    {
                        let _ = reader_view.update(cx, move |reader_view, cx| {
                            log::error!("cannot load reader page: {error}");
                            reader_view.state = ReaderState::Error(SharedString::from(error));
                            cx.notify();
                        });
                        return;
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        let library_id = *library.id();
                        let library_missing = match app.libraries().await {
                            Ok(libraries) => !libraries.iter().any(|entry| entry.library_id() == &library_id),
                            Err(check_error) => {
                                log::warn!("could not verify reader library {library_id} after load failure: {check_error}");
                                error.contains("library not found:")
                            }
                        };
                        if library_missing {
                            log::warn!("reader library {library_id} no longer exists; returning to the library browser");
                            let _ = reader_view.update(cx, Self::request_close);
                            return;
                        }
                        let _ = reader_view.update(cx, move |reader_view, cx| {
                            log::error!("cannot load reader page: {error}");
                            reader_view.state = ReaderState::Error(SharedString::from(error));
                            cx.notify();
                        });
                    }
                }
            }
        });
        self._load_task = Some(load_task);
        #[cfg(feature = "kobo")]
        if let Err(error) = gpui_kobo::request_experience_mode(gpui_kobo::KoboExperienceMode::ReaderQuality) {
            log::error!("failed to enter Kobo reader quality mode: {error}");
        }
    }
}

impl EventEmitter<CloseRequested> for ReaderView {}
#[cfg(feature = "audiobooks")]
impl EventEmitter<AudiobookOpened> for ReaderView {}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod lifecycle_tests {
    use super::*;
    use futures_util::FutureExt;

    #[gpui::test]
    fn missing_library_close_emits_without_reentering_the_reader(cx: &mut gpui::TestAppContext) {
        let observed = Rc::new(std::cell::Cell::new(false));
        let reader = cx.new(|_| ReaderView { title: "Book".into(), state: ReaderState::Error("library removed".into()), _load_task: None });
        let closed = observed.clone();
        let _subscription = cx.update(|cx| cx.subscribe(&reader, move |_, _: &CloseRequested, _| closed.set(true)));
        reader.update(cx, ReaderView::request_close);
        cx.run_until_parked();
        assert!(observed.get());
    }

    #[test]
    fn completed_and_failed_loads_release_download_subscription() {
        for result in [Ok(()), Err("load failed")] {
            let (states, incoming) = async_channel::unbounded();
            let (updates, _) = async_channel::unbounded();
            let actual = with_download_progress(std::future::ready(result), incoming, &updates).now_or_never();
            assert_eq!(actual, Some(result));
            assert!(states.is_closed());
        }
    }

    #[test]
    fn cancelling_load_releases_subscription_after_forwarding_progress() {
        let (states, incoming) = async_channel::unbounded();
        let (updates, received) = async_channel::unbounded();
        states.try_send(app::DownloadState::NotDownloaded).unwrap();
        let mut loading = Box::pin(with_download_progress(std::future::pending::<()>(), incoming, &updates));
        assert!(loading.as_mut().now_or_never().is_none());
        assert!(matches!(received.try_recv(), Ok(ReaderLoadUpdate::Download(app::DownloadState::NotDownloaded))));
        drop(loading);
        assert!(states.is_closed());
    }

    #[test]
    fn closed_progress_stream_does_not_prevent_load_completion() {
        let (states, incoming) = async_channel::unbounded();
        drop(states);
        let (updates, _) = async_channel::unbounded();
        let (complete, completion) = async_channel::bounded(1);
        let mut loading = Box::pin(with_download_progress(completion.recv(), incoming, &updates));
        assert!(loading.as_mut().now_or_never().is_none());
        complete.try_send(17).unwrap();
        assert_eq!(loading.now_or_never(), Some(Ok(17)));
    }
}

impl Render for ReaderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.state {
            ReaderState::Loading { stage, download } => {
                let theme = ui_components::browser_theme(cx);
                let mut loading = gpui::div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(gpui::px(ui_components::SPACE_SM))
                    .bg(theme.page_bg)
                    .text_color(theme.text)
                    .child("Loading book…")
                    .child(gpui::div().text_color(theme.text_muted).child(stage.clone()));
                if let Some(state) = download {
                    let progress = match state {
                        app::DownloadState::Queued => Some((None, SharedString::from("Waiting to download…"))),
                        app::DownloadState::Downloading(progress) => {
                            let fraction = progress.value();
                            let label = if fraction > 0.0 { format!("Downloading… {:.0}%", fraction * 100.0) } else { "Starting download…".to_owned() };
                            Some(((fraction > 0.0).then_some(fraction), SharedString::from(label)))
                        }
                        app::DownloadState::NotDownloaded | app::DownloadState::Downloaded => None,
                    };
                    if let Some((fraction, label)) = progress {
                        let indicator = match fraction {
                            Some(fraction) => ui_components::progress_fill(fraction, theme).into_any_element(),
                            None => gpui::div()
                                .h_full()
                                .w(gpui::relative(0.2))
                                .bg(theme.accent)
                                .with_animation("book-download-indeterminate", gpui::Animation::new(Duration::from_millis(900)).repeat(), |indicator, phase| indicator.ml(gpui::relative(phase * 0.8)))
                                .into_any_element(),
                        };
                        loading = loading.child(
                            gpui::div()
                                .w(gpui::px(320.0))
                                .max_w_full()
                                .flex()
                                .flex_col()
                                .gap(gpui::px(ui_components::SPACE_XS))
                                .child(gpui::div().text_color(theme.text_muted).child(label))
                                .child(ui_components::progress_track(theme).child(indicator)),
                        );
                    }
                }
                loading.into_any_element()
            }
            ReaderState::Ready(view) => view.clone().into_any_element(),
            ReaderState::Error(error) => {
                let theme = ui_components::browser_theme(cx);
                gpui::div().size_full().flex().items_center().justify_center().bg(theme.page_bg).text_color(theme.text).child(error.clone()).into_any_element()
            }
        };
        if ui_components::uses_mobile_navigation(window) && !matches!(self.state, ReaderState::Ready(_)) {
            let theme = ui_components::browser_theme(cx);
            let back = ui_components::mobile_back_button(theme).on_click(cx.listener(|reader, _, _, cx| reader.request_close(cx)));
            gpui::div()
                .relative()
                .size_full()
                .child(content)
                .child(gpui::div().absolute().top_0().left_0().right_0().pt(window.insets().safe_area.top).bg(theme.page_bg).child(ui_components::mobile_navigation_bar(self.title.clone(), Some(back), theme)))
                .into_any_element()
        } else {
            content
        }
    }
}
