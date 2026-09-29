use std::{cell::RefCell, rc::Rc, time::Duration};

use futures_util::{FutureExt, select_biased};
use gpui::{App, IntoElement, ParentElement, Styled, Window, div, px};
use gpui_component::{WindowExt, notification::Notification, progress::Progress};
use library_backend::LibraryClient;
use library_model::{DirectoryImportProgress, ImportFileStage};

/// The import owns this guard; completion, failure, and cancellation all close
/// its toast, even if the browse page that started the import has been closed.
pub(crate) struct ImportProgressToast(async_channel::Sender<()>);

impl Drop for ImportProgressToast {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

impl ImportProgressToast {
    pub(crate) fn start(library: LibraryClient, window: &mut Window, cx: &mut App) -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (library, window, cx);
            return Self(async_channel::bounded(1).0);
        }
        #[cfg(not(target_arch = "wasm32"))]
        Self::start_with_reader(
            move || {
                let library = library.clone();
                async move { library.directory_import_progress().await }
            },
            window,
            cx,
        )
    }

    /// Watches one native import activity so simultaneous imports do not
    /// borrow the progress of whichever activity started first.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn start_for_activity(library: LibraryClient, activity: uuid::Uuid, window: &mut Window, cx: &mut App) -> Self {
        Self::start_with_reader(
            move || {
                let library = library.clone();
                async move { library.directory_import_progress_for(activity).await }
            },
            window,
            cx,
        )
    }

    fn start_with_reader<F: std::future::Future<Output = Result<Option<DirectoryImportProgress>, String>> + 'static>(mut read_progress: impl FnMut() -> F + 'static, window: &mut Window, cx: &mut App) -> Self {
        let id = gpui::SharedString::from(uuid::Uuid::new_v4().to_string());
        let progress = Rc::new(RefCell::new(presentation(None)));
        let content_progress = progress.clone();
        window.push_notification(
            Notification::new().id1::<Self>(id.clone()).autohide(false).title("Importing books").content(move |_, _, _| {
                let status = content_progress.borrow().clone();
                let mut content = div().w_full().flex().flex_col().gap(px(8.)).child(status.label);
                if let Some(name) = status.book {
                    content = content.child(div().w_full().child(name));
                }
                if let Some(stage) = status.stage {
                    content = content.child(stage);
                }
                content.child(Progress::new("import-progress").loading(status.percent.is_none()).value(status.percent.unwrap_or_default())).into_any_element()
            }),
            cx,
        );
        let notification = window.notifications(cx).last().expect("import toast was just added").downgrade();
        let (done, finished) = async_channel::bounded::<()>(1);
        window
            .spawn(cx, async move |cx| {
                let completed = finished.recv().fuse();
                futures_util::pin_mut!(completed);
                loop {
                    // Completion wins even when a progress request is slow or queued
                    // behind the final database commit.
                    let update = read_progress().fuse();
                    futures_util::pin_mut!(update);
                    select_biased! {
                        _ = completed => break,
                        result = update => {
                            if let Ok(current) = result {
                                let current = presentation(current);
                                if *progress.borrow() != current {
                                    *progress.borrow_mut() = current;
                                    if notification.update(cx, |_, cx| cx.notify()).is_err() { break; }
                                }
                            }
                        }
                    }
                    let delay = cx.background_executor().timer(Duration::from_millis(250)).fuse();
                    futures_util::pin_mut!(delay);
                    select_biased! { _ = completed => break, _ = delay => {} }
                }
                let _ = cx.update(|window, cx| window.remove_notification1::<ImportProgressToast>(id, cx));
            })
            .detach();
        Self(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Context, Render};

    struct Host;
    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn completion_closes_only_its_own_toast_even_with_a_pending_progress_read(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| Host);
            gpui_component::Root::new(host, window, cx)
        });
        let first = cx.update(|window, cx| ImportProgressToast::start_with_reader(|| std::future::pending(), window, cx));
        let second = cx.update(|window, cx| ImportProgressToast::start_with_reader(|| std::future::ready(Ok(Some(DirectoryImportProgress { total: 10, succeeded: 3, failed: 1, current: None }))), window, cx));
        cx.run_until_parked();
        cx.update(|window, cx| assert_eq!(window.notifications(cx).len(), 2));
        drop(first);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        cx.update(|window, cx| assert_eq!(window.notifications(cx).len(), 1));
        drop(second);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        cx.update(|window, cx| assert!(window.notifications(cx).is_empty()));
    }

    #[test]
    fn progress_counts_failed_books_as_processed_and_handles_unknown_totals() {
        assert_eq!(presentation(None).percent, None);
        assert_eq!(presentation(Some(DirectoryImportProgress::default())).percent, None);
        let status = presentation(Some(DirectoryImportProgress { total: 10, succeeded: 3, failed: 1, current: None }));
        assert_eq!(status.percent, Some(40.));
        assert!(status.label.contains("4 of 10"));
        assert!(status.label.contains("1 failed"));
    }

    #[test]
    fn current_book_has_copy_progress_but_later_stages_are_indeterminate() {
        let mut progress = DirectoryImportProgress {
            total: 3,
            succeeded: 1,
            failed: 0,
            current: Some(library_model::ImportFileProgress { name: "History.m4b".into(), total_bytes: Some(200_000_000), stage: ImportFileStage::Copying { copied_bytes: 50_000_000 } }),
        };
        let status = presentation(Some(progress.clone()));
        assert_eq!(status.label, "Book 2 of 3");
        assert_eq!(status.book.as_deref(), Some("History.m4b"));
        assert_eq!(status.stage.as_deref(), Some("Copying book · 50.0 / 200.0 MB"));
        assert_eq!(status.percent, Some(25.));
        progress.current.as_mut().unwrap().total_bytes = None;
        assert_eq!(presentation(Some(progress.clone())).percent, None);
        progress.current.as_mut().unwrap().stage = ImportFileStage::Identifying;
        let status = presentation(Some(progress));
        assert_eq!(status.percent, None);
        assert_eq!(status.stage.as_deref(), Some("Identifying book…"));
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Presentation {
    label: String,
    book: Option<String>,
    stage: Option<String>,
    percent: Option<f32>,
}

fn presentation(progress: Option<DirectoryImportProgress>) -> Presentation {
    let Some(progress) = progress.filter(|p| p.total > 0) else {
        return Presentation { label: "Preparing import…".into(), book: None, stage: None, percent: None };
    };
    let completed = progress.succeeded.saturating_add(progress.failed).min(progress.total);
    let failures = if progress.failed > 0 { format!(" · {} failed", progress.failed) } else { String::new() };
    let Some(file) = progress.current else {
        return Presentation { label: format!("{completed} of {} books{failures}", progress.total), book: None, stage: None, percent: Some(100. * completed as f32 / progress.total as f32) };
    };
    let (stage, percent) = match file.stage {
        ImportFileStage::Preparing => ("Preparing book…".into(), None),
        ImportFileStage::Copying { copied_bytes } => match file.total_bytes.filter(|total| *total > 0) {
            Some(total) => (format!("Copying book · {:.1} / {:.1} MB", copied_bytes as f64 / 1_000_000., total as f64 / 1_000_000.), Some((100. * copied_bytes as f64 / total as f64).clamp(0., 100.) as f32)),
            None => (format!("Copying book · {:.1} MB copied", copied_bytes as f64 / 1_000_000.), None),
        },
        ImportFileStage::Saving => ("Saving book to disk…".into(), None),
        ImportFileStage::Identifying => ("Identifying book…".into(), None),
        ImportFileStage::ReadingMetadata => ("Reading title, author and other metadata…".into(), None),
        ImportFileStage::Publishing => ("Placing book in library folder…".into(), None),
        ImportFileStage::AddingToLibrary => ("Adding book to library…".into(), None),
    };
    Presentation { label: format!("Book {} of {}{failures}", (completed + 1).min(progress.total), progress.total), book: Some(file.name), stage: Some(stage), percent }
}

/// One styled notification per coordinator job, including recovered imports.
#[cfg(target_arch = "wasm32")]
pub(crate) fn watch_browser_imports(backend: Rc<app::AppClient>, sync_events: async_channel::Sender<crate::SyncEvent>, window: &mut Window, cx: &mut App) -> gpui::Task<()> {
    use super::import_presentation::browser_presentation;
    use app::ImportProgress;
    use std::collections::BTreeMap;
    struct BrowserImport;
    let mut updates = backend.import_updates();
    window.spawn(cx, async move |cx| {
        let mut shown: BTreeMap<String, (Rc<RefCell<ImportProgress>>, gpui::WeakEntity<Notification>)> = BTreeMap::new();
        let mut last_activity_refresh = None;
        loop {
            let snapshot = updates.borrow_and_update().clone();
            let mut wake_activity = false;
            for (id, current) in snapshot {
                let active = matches!(&current.state, app::ImportState::Queued | app::ImportState::Copying { .. } | app::ImportState::Adding { .. });
                if active {
                    let changed_phase = shown.get(&id).is_none_or(|(previous, _)| std::mem::discriminant(&previous.borrow().state) != std::mem::discriminant(&current.state));
                    let refresh_due = last_activity_refresh.is_none_or(|last: web_time::Instant| last.elapsed() >= std::time::Duration::from_secs(1));
                    wake_activity |= changed_phase || refresh_due;
                }
                if let Some((value, notification)) = shown.get(&id) {
                    if std::mem::discriminant(&value.borrow().state) == std::mem::discriminant(&current.state) {
                        if *value.borrow() != current {
                            *value.borrow_mut() = current;
                            let _ = notification.update(cx, |_, cx| cx.notify());
                        }
                        continue;
                    }
                }
                let retry = matches!(&current.state, app::ImportState::Paused { .. });
                let title = current.name.clone();
                let value = Rc::new(RefCell::new(current));
                let content = value.clone();
                let backend = backend.clone();
                let retry_id = id.clone();
                let result = cx.update(|window, cx| {
                    let mut toast = Notification::new().id1::<BrowserImport>(gpui::SharedString::from(id.clone())).title(title).autohide(false).content(move |_, _, _| {
                        let state = content.borrow();
                        let (text, percent) = browser_presentation(&state.state);
                        let label = div().w_full().flex().flex_col().gap(px(8.)).child(text);
                        if active { label.child(Progress::new("browser-import-progress").loading(percent.is_none()).value(percent.unwrap_or_default())).into_any_element() } else { label.into_any_element() }
                    });
                    if retry {
                        toast = toast.action(move |_, _, _| {
                            let backend = backend.clone();
                            let id = retry_id.clone();
                            ui_components::base_button("retry-import").label("Retry").on_click(move |_, _, _| backend.retry_import(&id))
                        });
                    }
                    window.push_notification(toast, cx);
                    window.notifications(cx).last().unwrap().downgrade()
                });
                match result {
                    Ok(notification) => {
                        shown.insert(id, (value, notification));
                    }
                    Err(_) => return,
                }
            }
            if wake_activity {
                last_activity_refresh = Some(web_time::Instant::now());
                let _ = sync_events.try_send(crate::SyncEvent::TransferRefreshWake);
            }
            if updates.changed().await.is_err() {
                break;
            }
        }
    })
}
