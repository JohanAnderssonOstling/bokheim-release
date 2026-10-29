use super::{AccountActivity, SyncPage};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, Entity, Focusable, Window};
use gpui_component::input::{Input, InputState};
use ui_components as components;

pub(super) struct DeleteAccountForm {
    pub(super) email: String,
    password: Entity<InputState>,
    confirmation: Entity<InputState>,
}

impl SyncPage {
    pub(super) fn open_account_deletion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() || self.deletion.is_some() {
            return;
        }
        let Some(email) = self.state.read(cx).account_status.email().map(str::to_owned) else { return };
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Current password").masked(true));
        let confirmation = cx.new(|cx| InputState::new(window, cx).placeholder("Type DELETE to confirm"));
        password.read(cx).focus_handle(cx).focus(window, cx);
        self.deletion = Some(DeleteAccountForm { email, password, confirmation });
        self.error = None;
        self.message = None;
        cx.notify();
    }

    pub(super) fn account_deletion_form(&self, form: &DeleteAccountForm, entity: &Entity<Self>, feedback: Option<AnyElement>, theme: components::BrowserTheme) -> Div {
        let cancel = entity.clone();
        let submit = entity.clone();
        components::access_form()
            .child(components::section_header("Delete your account", format!("Permanently delete {}? This cannot be undone.", form.email), theme))
            .child(components::form_hint(
                "Your account, synced library records, reading progress and bookmarks will be deleted, and all sessions revoked. Books saved on your devices stay there. Uploaded book and cover files may remain on the server.",
                theme,
            ))
            .children(feedback)
            .child(Input::new(&form.password))
            .child(Input::new(&form.confirmation))
            .child(
                components::action_row()
                    .child(components::base_button("confirm-delete-account").label("Permanently delete account").danger().on_click(move |_, window, cx| {
                        submit.update(cx, |page, cx| page.start_account_deletion(window, cx));
                    }))
                    .child(components::dialog_cancel_button("cancel-delete-account", "Cancel").on_click(move |_, _, cx| {
                        cancel.update(cx, |page, cx| {
                            page.deletion = None;
                            page.error = None;
                            cx.notify();
                        });
                    })),
            )
    }

    fn start_account_deletion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let Some(form) = self.deletion.as_ref() else { return };
        if form.confirmation.read(cx).value().as_str() != "DELETE" || form.password.read(cx).value().is_empty() {
            self.error = Some("Enter your current password and type DELETE exactly to confirm.".into());
            cx.notify();
            return;
        }
        let email = form.email.clone();
        if self.state.read(cx).account_status.email() != Some(email.as_str()) {
            self.deletion = None;
            self.error = Some("Your account changed. Reopen account deletion and try again.".into());
            cx.notify();
            return;
        }
        let password = form.password.read(cx).value().to_string();
        form.password.update(cx, |input, cx| input.set_value("", window, cx));
        self.deletion = None;
        self.activity = AccountActivity::DeletingAccount;
        self.error = None;
        self.message = Some("Deleting your account…".into());
        cx.notify();
        #[cfg(target_arch = "wasm32")]
        self.send_event(super::SyncEvent::DeleteAccountRequested { email, password }, cx);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let backend = self.services.backend.clone();
            // Keep the irreversible operation alive if Settings is closed.
            cx.spawn(async move |page, cx| {
                let result = backend.delete_account(email, password).await;
                let _ = page.update(cx, |page, cx| match result {
                    Ok(snapshot) => page.apply_snapshot(snapshot, Some("Your account has been deleted. Books saved on this device have been kept."), cx),
                    Err(error) => page.apply_error(error, cx),
                });
            })
            .detach();
        }
    }
}
