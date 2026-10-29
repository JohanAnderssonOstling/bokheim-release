//! The catalog's OPDS feed commands: adding a feed by its address, signing
//! in to one, and removing one. Feeds belong to the account, so adding and
//! removing reach every device; what is typed to sign in stays on this one.
//! Also opens the browser and the catalog's saved stores. A store is kept
//! on this device only, where its sign-in is.

use ui_components::user_facing_error;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, EventEmitter, Focusable, SharedString, Subscription, Window, div, px, rems};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable, IconName};
use library_model::{AddOpdsFeedResult, BrowseLocation, OpdsCredentials, OpdsSourceId, StoreId};
use ui_components as components;

use crate::library::services::LibraryContext;
use crate::library::widgets::SelectionAction;
use crate::services::{notify_error, notify_info};

/// A feed or store was added or removed, or credentials saved for a feed:
/// what the page lists from them is stale.
pub(crate) struct FeedsChanged;

#[derive(Clone, Copy)]
enum CatalogSource {
    Feed(OpdsSourceId),
    Store(StoreId),
}

struct FeedDialog {
    /// The feed being signed in to, or `None` when adding one.
    sign_in: Option<(OpdsSourceId, String)>,
    /// The address that requested credentials while adding a feed.
    authentication_url: Option<String>,
    url: Entity<InputState>,
    name: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    error: Option<SharedString>,
    /// The feed is being fetched or the credentials saved, which the dialog
    /// has to stay up for.
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl FeedDialog {
    fn requires_credentials(&self, cx: &App) -> bool {
        self.sign_in.is_some() || self.authentication_url.as_deref().is_some_and(|url| url == self.url.read(cx).value().trim())
    }

    /// Credentials as typed; none when the username is left empty.
    fn credentials(&self, cx: &App) -> Option<OpdsCredentials> {
        if !self.requires_credentials(cx) {
            return None;
        }
        let username = self.username.read(cx).value().trim().to_owned();
        (!username.is_empty()).then(|| OpdsCredentials { username, password: self.password.read(cx).value().to_string() })
    }

    /// Whether there is enough typed to submit.
    fn ready(&self, cx: &App) -> bool {
        (self.sign_in.is_some() || !self.url.read(cx).value().trim().is_empty()) && (!self.requires_credentials(cx) || self.credentials(cx).is_some())
    }
}

pub(crate) struct FeedActions {
    ui: LibraryContext,
    dialog: Option<FeedDialog>,
    /// Whether this system can open a store window; stores are not offered
    /// until it is known that it can.
    stores_available: bool,
}

impl EventEmitter<FeedsChanged> for FeedActions {}

impl FeedActions {
    pub(crate) fn new(ui: LibraryContext, cx: &mut Context<Self>) -> Self {
        let app = ui.app().clone();
        cx.spawn(async move |actions, cx| {
            let available = app.stores_available().await.unwrap_or(false);
            let _ = actions.update(cx, |actions, cx| {
                actions.stores_available = available;
                cx.notify();
            });
        })
        .detach();
        Self { ui, dialog: None, stores_available: false }
    }

    pub(crate) fn has_modal(&self) -> bool {
        self.dialog.is_some()
    }

    pub(crate) fn dismiss_modal(&mut self, cx: &mut Context<Self>) {
        if self.dialog.as_ref().is_some_and(|dialog| dialog.busy) {
            return;
        }
        self.dialog = None;
        cx.notify();
    }

    pub(crate) fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open(None, window, cx);
    }

    pub(crate) fn open_sign_in(&mut self, source: OpdsSourceId, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Some((source, name)), window, cx);
    }

    fn open(&mut self, sign_in: Option<(OpdsSourceId, String)>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://example.org/opds"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name (optional)"));
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Username"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
        let submit_on_enter = |actions: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                actions.submit(window, cx);
            } else if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        };
        let subscriptions = [&url, &name, &username, &password].into_iter().map(|input| cx.subscribe_in(input, window, submit_on_enter)).collect();
        let first = if sign_in.is_some() { &username } else { &url };
        first.read(cx).focus_handle(cx).focus(window, cx);
        self.dialog = Some(FeedDialog { sign_in, authentication_url: None, url, name, username, password, error: None, busy: false, _subscriptions: subscriptions });
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.as_ref() else { return };
        if dialog.busy || !dialog.ready(cx) {
            return;
        }
        let credentials = dialog.credentials(cx);
        let submitted_url = dialog.url.read(cx).value().trim().to_owned();
        let app = self.ui.app().clone();
        let name = Some(dialog.name.read(cx).value().trim().to_owned()).filter(|name| !name.is_empty());
        let request: futures_util::future::LocalBoxFuture<'static, Result<AddOpdsFeedResult, app::BackendError>> = match (&dialog.sign_in, credentials) {
            (Some((source, _)), Some(credentials)) => {
                let source = *source;
                Box::pin(async move { app.set_opds_credentials(source, credentials).await.map(|_| AddOpdsFeedResult::Added) })
            }
            (Some(_), None) => return,
            (None, credentials) => {
                let url = submitted_url.clone();
                Box::pin(async move { app.add_opds_feed(url, name, credentials).await })
            }
        };
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.busy = true;
            dialog.error = None;
        }
        cx.notify();
        cx.spawn_in(window, async move |actions, cx| {
            let result = request.await;
            let _ = actions.update_in(cx, |actions, window, cx| {
                match result {
                    Ok(AddOpdsFeedResult::Added) => {
                        actions.dialog = None;
                        cx.emit(FeedsChanged);
                    }
                    Ok(AddOpdsFeedResult::SignInRequired) => {
                        if let Some(dialog) = actions.dialog.as_mut() {
                            dialog.busy = false;
                            dialog.authentication_url = Some(submitted_url);
                            if dialog.requires_credentials(cx) {
                                dialog.username.read(cx).focus_handle(cx).focus(window, cx);
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(dialog) = actions.dialog.as_mut() {
                            dialog.busy = false;
                            dialog.error = Some(user_facing_error(&error, "Could not add or sign in to the catalog. Check its address and sign-in details, then try again.").into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Only a feed's root chip or a saved store can remove its source.
    pub(super) fn removal_action(actions: Entity<Self>, location: BrowseLocation, name: String) -> Option<SelectionAction> {
        let (source, label) = match location {
            BrowseLocation::Opds { source, page: None } => (CatalogSource::Feed(source), "Remove feed"),
            BrowseLocation::Store(id) => (CatalogSource::Store(id), "Remove store"),
            _ => return None,
        };
        Some(SelectionAction::new("remove-source", label, components::navigation_icon(components::NavigationIcon::Trash2), move |window, cx| actions.update(cx, |actions, cx| actions.remove(source, name.clone(), window, cx))))
    }

    fn remove(&mut self, source: CatalogSource, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.ui.app().clone();
        cx.spawn_in(window, async move |actions, cx| {
            let (result, success_id, error_id) = match source {
                CatalogSource::Feed(id) => (app.remove_opds_feed(id).await, "feed-removed", "feed-remove-error"),
                CatalogSource::Store(id) => (app.remove_store(id).await, "store-removed", "store-remove-error"),
            };
            let _ = actions.update_in(cx, |_, window, cx| match result {
                Ok(()) => {
                    notify_info(success_id, format!("Removed “{name}”"), window, cx);
                    cx.emit(FeedsChanged);
                }
                Err(error) => notify_error(error_id, format!("Could not remove “{name}”. {}", user_facing_error(&error, "Please try again.")), window, cx),
            });
        })
        .detach();
    }

    /// Opens the store's window, where what is bought goes into this
    /// library's root.
    pub(crate) fn open_store(&mut self, id: StoreId, window: &mut Window, cx: &mut Context<Self>) {
        let (app, library_id) = (self.ui.app().clone(), *self.ui.backend().id());
        cx.spawn_in(window, async move |actions, cx| {
            if let Err(error) = app.open_store(id, library_id).await {
                let _ = actions.update_in(cx, |_, window, cx| notify_error("store-open-error", format!("Could not open the store. {}", user_facing_error(&error, "Please try again.")), window, cx));
            }
        })
        .detach();
    }

    fn open_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (app, library_id) = (self.ui.app().clone(), *self.ui.backend().id());
        cx.spawn_in(window, async move |actions, cx| {
            if let Err(error) = app.open_browser(library_id).await {
                let _ = actions.update_in(cx, |_, window, cx| notify_error("browser-open-error", format!("Could not open the browser. {}", user_facing_error(&error, "Please try again.")), window, cx));
            }
        })
        .detach();
    }

    pub(crate) fn render_trigger(actions: Entity<Self>) -> AnyElement {
        components::outlined_icon_button("catalog-add-feed", "Add OPDS feed", IconName::Plus).on_click(move |_, window, cx| actions.update(cx, |actions, cx| actions.open_add(window, cx))).into_any_element()
    }

    /// Opens the browser directly where a browser window is available.
    pub(crate) fn render_store_trigger(actions: Entity<Self>, cx: &App) -> Option<AnyElement> {
        actions.read(cx).stores_available.then(||
            components::outlined_icon_button("catalog-open-browser", "Open browser", IconName::Globe)
                .on_click(move |_, window, cx| actions.update(cx, |actions, cx| actions.open_browser(window, cx)))
                .into_any_element()
        )
    }

    /// What a feed page that refused this device shows in place of its
    /// listing: a way to sign in.
    pub(crate) fn render_sign_in(actions: Entity<Self>, source: OpdsSourceId, name: String, cx: &App) -> AnyElement {
        let theme = components::browser_theme(cx);
        let button = components::base_button("feed-sign-in").primary().label("Sign in").on_click(move |_, window, cx| actions.update(cx, |actions, cx| actions.open_sign_in(source, name.clone(), window, cx)));
        components::absolute_full_size()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(components::SPACE_MD))
            .child(div().text_color(theme.text).child("This feed asks for a username and password."))
            .child(button)
            .into_any_element()
    }

    pub(crate) fn render_overlay(actions: Entity<Self>, window: &Window, cx: &mut App) -> Option<AnyElement> {
        let (title, adding, authenticating, inputs, error, busy, ready) = {
            let dialog = actions.read(cx).dialog.as_ref()?;
            let title = match &dialog.sign_in {
                Some((_, name)) => format!("Sign in to {name}"),
                None => "Add OPDS feed".to_owned(),
            };
            let inputs = [dialog.url.clone(), dialog.name.clone(), dialog.username.clone(), dialog.password.clone()];
            (title, dialog.sign_in.is_none(), dialog.requires_credentials(cx), inputs, dialog.error.clone(), dialog.busy, dialog.ready(cx))
        };
        let [url, name, username, password] = inputs;
        let theme = components::browser_theme(cx);
        let (cancel_actions, dismiss_actions, submit_actions) = (actions.clone(), actions.clone(), actions);
        let cancel = components::dialog_cancel_button("cancel-feed-dialog", "Cancel").disabled(busy).on_click(move |_, _, cx| cancel_actions.update(cx, |actions, cx| actions.dismiss_modal(cx)));
        let (idle, working) = if adding { ("Add", "Adding…") } else { ("Sign in", "Signing in…") };
        let submit = components::base_button("confirm-feed-dialog")
            .primary()
            .label(if busy { working } else { idle })
            .tooltip(components::shortcut_tooltip(idle, "enter"))
            .disabled(busy || !ready)
            .on_click(move |_, window, cx| submit_actions.update(cx, |actions, cx| actions.submit(window, cx)));
        let input_height = rems(components::topbar_action_height_rem());
        let panel = components::modal_form_surface(theme)
            .w(px(420.0))
            .child(div().text_size(rems(1.125)).text_color(theme.text).child(title))
            .when(adding, |panel| panel.child(Input::new(&url).h(input_height)).child(Input::new(&name).h(input_height)))
            .when(authenticating, |panel| {
                panel
                    .when(adding, |panel| panel.child(div().text_color(theme.text_muted).text_size(rems(0.875)).child("This feed requires a username and password.")))
                    .child(Input::new(&username).h(input_height))
                    .child(Input::new(&password).mask_toggle().h(input_height))
                    .child(div().text_color(theme.text_muted).text_size(rems(0.875)).child("Kept on this device only."))
            })
            .children(error.map(|error| components::error_text(theme).child(error)))
            .child(div().flex().items_center().justify_end().gap(px(components::SPACE_SM)).child(cancel).child(submit));
        let scrim = components::modal_scrim("feed-dialog-scrim", theme).on_click(move |_, _, cx| dismiss_actions.update(cx, |actions, cx| actions.dismiss_modal(cx)));
        Some(gpui::deferred(components::centered_modal(scrim, panel, window)).with_priority(2).into_any_element())
    }
}

#[cfg(all(test, not(target_arch = "wasm32"), not(feature = "kobo")))]
mod tests {
    use super::*;
    use crate::navigation::LibrarySession;
    use gpui::{Render, TestAppContext};
    use gpui_component::Root;
    use uuid::Uuid;

    struct FeedFixture(Entity<FeedActions>);

    impl Render for FeedFixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().children(FeedActions::render_overlay(self.0.clone(), window, cx))
        }
    }

    #[gpui::test]
    async fn removal_actions_refresh_the_catalog_and_keep_source_feedback_separate(cx: &mut TestAppContext) {
        let _backend = crate::test_support::BACKEND.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_directory, _session, library) = LibrarySession::test_context(cx).await;
        let actions = cx.new(|cx| FeedActions::new(library, cx));
        let (sent, received) = async_channel::unbounded();
        let _subscription = cx.update(|cx| cx.subscribe(&actions, move |_, _: &FeedsChanged, _| sent.try_send(()).unwrap()));
        let (root, visual) = cx.add_window_view(|window, cx| {
            let content = cx.new(|_| FeedFixture(actions.clone()));
            Root::new(content, window, cx)
        });
        let id = Uuid::new_v4();
        // Deletion is idempotent for already-removed sources. Use one ID in
        // both namespaces and repeat the feed command to check toast replacement.
        let feed = BrowseLocation::Opds { source: id, page: None };
        for (location, notifications) in [(feed, 1), (BrowseLocation::Store(id), 2), (feed, 2)] {
            let action = FeedActions::removal_action(actions.clone(), location, "Source".into()).unwrap();
            visual.update(|window, cx| action.run(window, cx));
            received.recv().await.unwrap();
            root.read_with(visual, |root, cx| assert_eq!(root.notification.read(cx).notifications().len(), notifications));
            assert!(received.is_empty(), "one completed removal must request one catalog refresh");
        }
    }
}
