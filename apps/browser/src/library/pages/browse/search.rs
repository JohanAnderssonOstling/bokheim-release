//! The browse page's search box: whether it is open, what has been typed, and
//! when a pause in typing is long enough to search. The page hears one event,
//! carrying the query to run.

use std::time::Duration;

use gpui::{App, Context, EventEmitter, Subscription, Task, Window};

use super::controls::SearchField;

/// The query to search for, reported once typing has settled.
pub(super) struct SearchChanged(pub(super) String);

pub(super) struct BrowseSearch {
    /// Created the first time the box opens, since an input needs a window.
    field: Option<SearchField>,
    open: bool,
    /// What was last reported, so an edit that changes nothing reports nothing.
    query: String,
    debounce: Option<Task<()>>,
    _field_changes: Option<Subscription>,
}

impl EventEmitter<SearchChanged> for BrowseSearch {}

impl BrowseSearch {
    pub(super) fn new() -> Self {
        Self { field: None, open: false, query: String::new(), debounce: None, _field_changes: None }
    }

    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.field.as_ref().is_some_and(|field| field.is_focused(window, cx))
    }

    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.field.is_none() {
            let (field, changes) = SearchField::new(Some("Search books or authors"), |search: &mut Self, cx| search.field_changed(cx), window, cx);
            self.field = Some(field);
            self._field_changes = Some(changes);
        }
        self.open = true;
        if let Some(field) = &self.field {
            field.focus(window, cx);
        }
        cx.notify();
    }

    /// Hides the box. What was typed stays, and so does the search it ran.
    pub(super) fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    /// Empties the box without reporting it, for a page that has already
    /// dropped the query itself.
    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.debounce = None;
        if let Some(field) = &self.field {
            field.clear(window, cx);
        }
    }

    pub(super) fn render(&self) -> Option<gpui::Div> {
        self.open.then(|| self.field.as_ref().map(SearchField::render)).flatten()
    }

    /// Reports the typed query once typing pauses.
    fn field_changed(&mut self, cx: &mut Context<Self>) {
        let Some(field) = &self.field else { return };
        let query = field.query(cx);
        if query == self.query {
            return;
        }
        self.query = query.clone();
        let timer = cx.background_executor().timer(Duration::from_millis(250));
        self.debounce = Some(cx.spawn(async move |search, cx| {
            timer.await;
            let _ = search.update(cx, |search, cx| {
                search.debounce = None;
                cx.emit(SearchChanged(query));
            });
        }));
    }
}
