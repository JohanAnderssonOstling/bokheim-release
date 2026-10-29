//! Sync and account presentation, rendered as the Account and Libraries
//! sections of Settings.
//!
//! The account leads with who is signed in, then the quota as a shape with
//! what each library holds of it; the libraries follow as a table sorted by
//! what they occupy.
//!
//! What the device is uploading and downloading is reported on the library it
//! belongs to rather than in a queue of its own: a job is only ever interesting
//! as something *a library* is doing, and a list beneath the libraries made the
//! reader match names between two lists to find that out.

use super::*;
use crate::services::format_storage_bytes;
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
use ui_components::Button;
use gpui_component::checkbox::Checkbox;
use gpui_component::Disableable;
use gpui_component::{menu::DropdownMenu as _, Icon};
#[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
use std::path::Path;

const LEGEND_LIBRARY_LIMIT: usize = 4;

/// The single character the identity avatar carries. Uppercased from the
/// address because that is the only name the account has.
fn account_initial(email: &str) -> String {
    email.chars().next().map(|first| first.to_uppercase().to_string()).unwrap_or_else(|| "?".to_owned())
}

/// The identity row has its own render boundary. Sync, storage, and transfer
/// updates can refresh the page around it without making the row flash.
pub(super) struct AccountIdentityView {
    page: Entity<SyncPage>,
    account_status: AccountStatus,
}

impl AccountIdentityView {
    pub(super) fn new(page: Entity<SyncPage>, account_status: AccountStatus) -> Self {
        Self { page, account_status }
    }

    pub(super) fn set_account_status(&mut self, account_status: AccountStatus, cx: &mut Context<Self>) {
        if self.account_status.signed_in() == account_status.signed_in() && self.account_status.email() == account_status.email() {
            return;
        }
        self.account_status = account_status;
        cx.notify();
    }
}

impl Render for AccountIdentityView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let email = self.account_status.email().unwrap_or_default().to_owned();
        let mut actions = Vec::with_capacity(3);
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            let password = self.page.clone();
            actions.push(components::link_button("account-change-password", "Change password", false).on_click(move |_, _, cx| {
                password.update(cx, |page, cx| page.open_password_change(cx));
            }));
        }
        let logout = self.page.clone();
        actions.push(components::link_button("account-log-out", "Log out", false).on_click(move |_, _, cx| {
            logout.update(cx, |page, cx| page.start_logout(cx));
        }));
        let delete = self.page.clone();
        actions.push(components::link_button("account-delete", "Delete account", false).on_click(move |_, window, cx| {
            delete.update(cx, |page, cx| page.open_account_deletion(window, cx));
        }));
        components::account_identity(account_initial(&email), email, actions, theme)
    }
}

/// The libraries, as a view of their own.
///
/// Settings draws Account, then this, as two sections — so the two halves of
/// what [`SyncPage`] knows are rendered in two places. A view of its own is
/// also the right boundary for the content: transfer progress reaches these
/// rows several times a second, and nothing above them should be rebuilt at
/// that rate.
pub(crate) struct LibrariesView {
    page: Entity<SyncPage>,
    _updates: gpui::Subscription,
}

impl LibrariesView {
    pub(crate) fn new(page: Entity<SyncPage>, cx: &mut Context<Self>) -> Self {
        let updates = cx.observe(&page, |_, _, cx| cx.notify());
        Self { page, _updates: updates }
    }
}

impl Render for LibrariesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let compact = components::WindowWidthClass::for_window(window).is_compact();
        let page = self.page.clone();
        let rows = page.read(cx).library_rows(cx);
        SyncPage::libraries_section(page.read(cx), &page, &rows, compact, theme, cx)
    }
}

/// One library as the page shows it: the sync status joined to the two
/// quantities the page reports for it.
///
/// `server_bytes` is what the account is charged for and is what the quota
/// meter is built from; `local_bytes` is what has been downloaded here. They
/// answer different questions and are never mixed.
struct LibraryRow {
    #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    local_path: Option<String>,
    name: String,
    status: LibrarySyncStatus,
    server_bytes: Option<u64>,
    local_bytes: Option<u64>,
}

impl LibraryRow {
    /// Server bytes rank the list once the server can report them; until then
    /// the local sizes are the only sizes there are.
    const fn sort_bytes(&self) -> u64 {
        match self.server_bytes {
            Some(bytes) => bytes,
            None => match self.local_bytes {
                Some(bytes) => bytes,
                None => 0,
            },
        }
    }
}

impl SyncPage {
    /// Joins the device's libraries to their sync status and local size.
    ///
    /// Libraries the backend has no status for are given the status they would
    /// have, so the verdict above the list is computed from exactly the rows the
    /// list renders rather than from a different set.
    fn library_rows(&self, cx: &gpui::App) -> Vec<LibraryRow> {
        let state = self.state.read(cx);
        // Signed out there is no verdict to wait for, so a library the backend
        // has no status for is what it plainly is — local — rather than a row
        // that says it is checking something that will never be checked.
        let unknown_state = if state.account_status.signed_in() { LibrarySyncState::Checking } else { LibrarySyncState::LocalOnly };
        let mut rows: Vec<LibraryRow> = self
            .active_library
            .read(cx)
            .entries()
            .iter()
            .map(|library| {
                let status = state.sync_statuses.iter().find(|status| status.library_id == *library.library_id()).cloned().unwrap_or_else(|| LibrarySyncStatus::placeholder(*library.library_id(), unknown_state));
                let local_bytes = state.local_storage_usage.iter().find(|usage| usage.library_id == *library.library_id()).map(|usage| usage.used_bytes);
                // A supported server that lists no share for a library is
                // reporting zero; an unsupported server reports nothing at all.
                let server_bytes = state.server_storage_usage.as_ref().map(|usage| usage.iter().find(|entry| entry.library_id == *library.library_id()).map(|entry| entry.used_bytes).unwrap_or_default());
                LibraryRow {
                    name: library.library_name().to_owned(),
                    status,
                    server_bytes,
                    local_bytes,
                    #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
                    local_path: Some(library.storage_locator().to_owned()),
                }
            })
            .collect();
        // Sorted by the quantity the page leads with, falling back to the local
        // sizes while no server breakdown is available.
        rows.sort_by(|left, right| right.sort_bytes().cmp(&left.sort_bytes()).then_with(|| left.name.cmp(&right.name)));
        rows
    }

    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn access_action(&self, entity: &Entity<Self>, id: &'static str, label: &'static str, icon: IconName, action: fn(&mut Self, &mut Context<Self>)) -> Button {
        let page = entity.clone();
        components::primary_action_button(id, label, icon, self.is_busy()).on_click(move |_, _, cx| page.update(cx, action))
    }

    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn access_link(&self, entity: &Entity<Self>, id: &'static str, label: &'static str, flow: AccountFlow) -> Button {
        let page = entity.clone();
        components::link_button(id, label, self.is_busy()).on_click(move |_, _, cx| page.update(cx, |page, cx| page.set_flow(flow, cx)))
    }

    /// The sign-in, registration, verification and password-reset forms.
    ///
    /// Only reachable while signed out, so it is kept apart from the page the
    /// account renders once there is an account to report on.
    ///
    /// Every flow has the same shape: what this screen is, the inputs it needs,
    /// one action that goes forward, and — quieter — the way to another flow.
    /// The inputs carry their own placeholders and are never labelled as well.
    ///
    /// The heading is the whole of the explanation. Text under it survives only
    /// where it says something the screen cannot show — a minimum length, or
    /// that the reset token is not the PIN.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn account_access_form(&self, entity: &Entity<Self>, feedback: Option<gpui::AnyElement>, theme: components::BrowserTheme) -> Div {
        let form = components::access_form();
        match self.flow {
            AccountFlow::SignIn => {
                let register = entity.clone();
                let mut actions = components::action_row().child(self.access_action(entity, "account-sign-in", "Sign in", IconName::Globe, Self::start_login));
                actions = actions.child(components::secondary_action_button("account-register", "Create account", IconName::User, self.is_busy()).on_click(move |_, _, cx| {
                    register.update(cx, |page, cx| page.set_flow(AccountFlow::Register, cx));
                }));
                let mut form = form
                    .child(components::section_header("Sign in", "", theme))
                    .children(feedback)
                    .child(Input::new(&self.authentication.login_email))
                    .child(Input::new(&self.authentication.password).mask_toggle())
                    .child(actions);
                // Recovery is offered once a sign-in has actually failed, so
                // the screen a returning user sees carries one action, not
                // three. It stays a link: it is the way out, not the way on.
                if self.login_failed {
                    form = form.child(self.access_link(entity, "account-request-reset", "Forgot your password?", AccountFlow::RequestPasswordReset));
                }
                form
            }
            AccountFlow::Register => form
                .child(components::section_header("Create account", "", theme))
                .children(feedback)
                .child(Input::new(&self.authentication.email))
                .child(Input::new(&self.authentication.registration_password).mask_toggle())
                .child(components::form_hint(format!("At least {} characters.", account_contract::PASSWORD_MIN_CHARACTERS), theme))
                .child(components::action_row().child(self.access_action(entity, "account-register", "Create account", IconName::User, Self::start_registration)))
                .child(self.access_link(entity, "account-sign-in", "Already have an account? Sign in", AccountFlow::SignIn)),
            AccountFlow::VerifyEmail => {
                let resend = entity.clone();
                let mut actions = components::action_row().child(self.access_action(entity, "account-verify", "Verify email", IconName::ArrowRight, Self::start_verify_email));
                actions = actions.child(components::secondary_action_button("account-resend", "Resend PIN", IconName::Redo2, self.is_busy()).on_click(move |_, _, cx| {
                    resend.update(cx, |page, cx| page.start_resend_verification(cx));
                }));
                // The PIN proves the mailbox; the password chosen at
                // registration proves this registrant, so it is asked again.
                form.child(components::section_header("Check your email", "", theme))
                    .children(feedback)
                    .child(OtpInput::new(&self.authentication.verification_pin))
                    .child(Input::new(&self.authentication.registration_password).mask_toggle())
                    .child(components::form_hint("The password you chose for this account.", theme))
                    .child(actions)
                    .child(self.access_link(entity, "account-restart-registration", "Wrong address? Start over", AccountFlow::Register))
            }
            AccountFlow::RequestPasswordReset => form
                .child(components::section_header("Reset your password", "", theme))
                .children(feedback)
                .child(Input::new(&self.authentication.email))
                .child(components::action_row().child(self.access_action(entity, "account-request-reset", "Email me a reset code", IconName::Redo2, Self::start_password_reset_request)))
                .child(self.access_link(entity, "account-sign-in", "Back to sign in", AccountFlow::SignIn)),
            AccountFlow::ResetPassword => {
                // The token is a pasted string, not the six-digit PIN, so it is
                // a plain input rather than the OTP boxes — drawing them alike
                // invites people to type six of its characters and give up.
                form.child(components::section_header("Set a new password", "Paste the full reset code from your email. This is different from the six-digit verification PIN.", theme))
                    .children(feedback)
                    .child(Input::new(&self.authentication.reset_token))
                    .child(Input::new(&self.authentication.new_password).mask_toggle())
                    .child(components::form_hint(format!("At least {} characters.", account_contract::PASSWORD_MIN_CHARACTERS), theme))
                    .child(components::action_row().child(self.access_action(entity, "account-reset-password", "Set new password", IconName::Redo2, Self::start_password_reset)))
                    .child(self.access_link(entity, "account-sign-in", "Back to sign in", AccountFlow::SignIn))
            }
        }
    }

    #[cfg(any(target_arch = "wasm32", target_os = "android"))]
    fn account_access_form(&self, entity: &Entity<Self>, feedback: Option<gpui::AnyElement>, theme: components::BrowserTheme) -> Div {
        let page = entity.clone();
        components::access_form().child(components::section_header("Bokheim account", "Sign in to sync your library across devices.", theme)).children(feedback).child(components::action_row().child(
            components::primary_action_button("account-sign-in", "Sign in or create account", IconName::Globe, self.is_busy()).on_click(move |_, _, cx| {
                #[cfg(target_arch = "wasm32")]
                page.update(cx, |page, cx| page.open_web_authentication(cx));
                #[cfg(target_os = "android")]
                page.update(cx, |page, cx| page.open_android_authentication(cx));
            }),
        ))
    }

    /// Splits the quota into the parts that explain it: what each library holds
    /// on this device, what the account holds that this device does not, and
    /// what uploads have reserved.
    ///
    /// Local copies can exceed what the server stores — books imported here but
    /// never uploaded — so library shares are taken from the used bytes in
    /// descending order and stop once those are accounted for. The remainder of
    /// the track is free space.
    fn storage_segments(&self, usage: StorageUsageResponse, theme: components::BrowserTheme, state: &SyncState) -> Vec<(SharedString, u64, gpui::Hsla)> {
        let mut segments: Vec<(SharedString, u64, gpui::Hsla)> = Vec::new();
        let mut other_bytes = 0_u64;
        for (index, (name, bytes)) in state.server_storage_usage.as_ref().map(|usage| usage.iter()).into_iter().flatten().filter(|library| library.used_bytes > 0).map(|library| (library.library_name.clone(), library.used_bytes)).enumerate()
        {
            if index < LEGEND_LIBRARY_LIMIT {
                segments.push((name.into(), bytes, components::usage_segment_color(index, theme)));
            } else {
                other_bytes += bytes;
            }
        }
        if other_bytes > 0 {
            segments.push(("Other libraries".into(), other_bytes, components::usage_segment_color(LEGEND_LIBRARY_LIMIT, theme)));
        }
        if usage.reserved_bytes > 0 {
            segments.push(("Reserved by uploads".into(), usage.reserved_bytes, theme.warning));
        }
        segments
    }
}

impl SyncPage {
    /// The account group: who is signed in, and what that account holds.
    ///
    /// Cloud storage used to be a group of its own, one gap below the identity
    /// that owns it. A quota is not a second subject: it is the answer to "what
    /// does this account get me", so it sits under the address it belongs to.
    /// Signed out there is no quota, and the group is its header, with the two
    /// ways in beside the title.
    pub(crate) fn settings_section(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = components::browser_theme(cx);
        let entity = cx.entity();
        // Whichever of the two the last attempt produced. They are never both
        // set, and one slot means a form cannot grow two lines of chrome.
        let feedback = match (&self.error, &self.message) {
            (Some(error), _) => Some(Alert::error("account-error", error.clone()).with_size(Size::Small).into_any_element()),
            (None, Some(message)) => Some(Alert::new("account-message", message.clone()).with_size(Size::Small).into_any_element()),
            (None, None) => None,
        };

        let signed_in = self.state.read(cx).account_status.signed_in();
        if signed_in && let Some(form) = self.deletion.as_ref() {
            return components::browser_settings_fieldset("Account", theme).child(components::browser_settings_block().child(self.account_deletion_form(form, &entity, feedback, theme)));
        }
        let account = if signed_in {
            components::browser_settings_fieldset("Account", theme).child(components::browser_settings_block().child(self.identity.clone()).child(self.storage_body(theme, cx)))
        } else {
            components::browser_settings_fieldset_with_actions("Account", self.account_access_actions(&entity, theme), theme)
        };

        // The form is not the group's resting state on any platform any more:
        // it appears because a header button asked for it, and it is the only
        // thing in the group while it is open.
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if self.access_open {
            // Every form here can be abandoned: the header opened it, and until
            // it is finished with there is no other way back to the account the
            // group is about.
            let close = entity.clone();
            let form = self.account_access_form(&entity, feedback, theme).child(components::link_button("account-close", "Close", self.is_busy()).on_click(move |_, _, cx| {
                close.update(cx, |page, cx| page.close_account_access(cx));
            }));
            return account.child(components::browser_settings_block().child(form));
        }

        account.children(feedback.map(|feedback| components::browser_settings_block().child(feedback)))
    }

    /// The quota as a line and a shape, and what each library holds of it.
    fn storage_body(&self, theme: components::BrowserTheme, cx: &Context<Self>) -> Div {
        let state = self.state.read(cx);
        let Some(usage) = state.storage_usage else {
            return components::storage_block().child("Loading cloud storage…");
        };
        let mut storage = components::storage_block().child(components::storage_summary(
            format_storage_bytes(usage.used_bytes),
            format!("of {} used", format_storage_bytes(usage.quota_bytes)),
            format!("{} available", format_storage_bytes(usage.available_bytes())),
            theme,
        ));
        if usage.quota_bytes > 0 {
            let segments = self.storage_segments(usage, theme, state);
            let share = |bytes: u64| bytes as f32 / usage.quota_bytes as f32;
            // Five greys and the warning cannot tell libraries apart on e-ink;
            // there the breakdown's figures stand alone.
            if !cfg!(feature = "kobo") {
                storage = storage.child(components::storage_meter(segments.iter().map(|(_, bytes, color)| (share(*bytes), *color)), theme));
            }
            storage = storage.child(components::storage_breakdown(
                segments.into_iter().map(|(label, bytes, color)| components::storage_breakdown_row(color, label, format!("{}%", (share(bytes) * 100.0).round() as u32), format_storage_bytes(bytes), theme)),
            ));
        }
        storage
    }

    /// The account group's header control while signed out: the two ways in.
    ///
    /// Signing in leads with `Log in` rather than with `Create account`: a
    /// person who opens Settings and finds themselves signed out is far more
    /// often someone whose session lapsed than someone who has never had one.
    fn account_access_actions(&self, entity: &Entity<Self>, theme: components::BrowserTheme) -> Div {
        let register = entity.clone();
        let login = entity.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(components::SPACE_SM))
            .child(components::browser_settings_header_button("account-open-register", "Create account", false, theme).on_click(move |_, _, cx| {
                register.update(cx, |page, cx| page.open_account_access(true, cx));
            }))
            .child(components::browser_settings_header_button("account-open-sign-in", "Log in", true, theme).on_click(move |_, _, cx| {
                login.update(cx, |page, cx| page.open_account_access(false, cx));
            }))
    }

    /// Reveals the form for `flow` under the account header.
    ///
    /// On the platforms that authenticate through a browser or the system
    /// account picker there is no form to reveal, so the same press opens that
    /// instead — the header reads the same everywhere, and what it opens is the
    /// platform's business.
    pub(super) fn open_account_access(&mut self, register: bool, cx: &mut Context<Self>) {
        let _ = register;
        #[cfg(target_arch = "wasm32")]
        self.open_web_authentication(cx);
        #[cfg(target_os = "android")]
        self.open_android_authentication(cx);
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            self.access_open = true;
            self.set_flow(if register { AccountFlow::Register } else { AccountFlow::SignIn }, cx);
        }
        cx.notify();
    }

    /// Opens the password-reset flow for someone who is already signed in.
    ///
    /// There is no separate "change password while signed in" exchange — the
    /// reset is the only one the server offers — so this is that flow, reached
    /// from the account menu instead of from a failed sign-in.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub(super) fn open_password_change(&mut self, cx: &mut Context<Self>) {
        self.access_open = true;
        self.set_flow(AccountFlow::RequestPasswordReset, cx);
        cx.notify();
    }

    /// Closes the form and returns the group to reporting the account.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub(super) fn close_account_access(&mut self, cx: &mut Context<Self>) {
        self.access_open = false;
        self.message = None;
        self.error = None;
        cx.notify();
    }

    /// The libraries this device holds: what each occupies, what it is doing,
    /// and where it stands with the account.
    ///
    /// The section opens with where a new library is put, because that setting
    /// is about this list — it decides where the next row in it will live — and
    /// a "Library storage" group of its own separated it from the only thing it
    /// refers to.
    fn libraries_section(&self, page: &Entity<Self>, rows: &[LibraryRow], compact: bool, theme: components::BrowserTheme, cx: &gpui::App) -> Div {
        let mut libraries = components::browser_settings_fieldset("Libraries", theme);
        let mut has_rows = false;

        #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
        if let Some(path) = self.default_library_location.clone() {
            let page = page.clone();
            let choose = components::browser_settings_path_button("default-library-location", components::library_path_label(Path::new(&path)), theme)
                .tooltip(path)
                .on_click(move |_, _, cx| page.update(cx, |page, cx| page.choose_library_location(cx)));
            libraries = libraries.child(components::browser_settings_compact_row("Default location for new libraries", choose, theme).border_t_0());
            has_rows = true;
        }

        if rows.is_empty() {
            return libraries.child(components::library_usage_empty("No local libraries", theme).when(!has_rows, |row| row.border_t_0()));
        }
        if !compact {
            libraries = libraries.child(components::library_table_header(theme).when(!has_rows, |row| row.border_t_0()));
            has_rows = true;
        }
        for row in rows {
            let local_bytes = row.local_bytes.map(format_storage_bytes).unwrap_or_else(|| if row.status.state == LibrarySyncState::Unavailable { "Unavailable".to_owned() } else { "Calculating…".to_owned() });
            let (status, status_color) = if row.status.issues.rejected != 0 {
                ("Some changes were rejected", theme.warning)
            } else if row.status.issues.untransmittable != 0 {
                ("Some changes are too large to sync", theme.warning)
            } else if row.status.issues.retryable != 0 {
                ("Waiting to retry changes", theme.warning)
            } else {
                match row.status.state {
                    LibrarySyncState::Checking => ("Loading status…", theme.text_muted),
                    LibrarySyncState::Unavailable => ("Unavailable", theme.warning),
                    LibrarySyncState::Receiving => ("Receiving library…", theme.accent),
                    LibrarySyncState::Synced => ("Up to date", theme.accent),
                    LibrarySyncState::SyncPending if row.status.last_synced_at.is_none() => ("Sync pending", theme.warning),
                    LibrarySyncState::SyncPending => ("Changes pending", theme.warning),
                    LibrarySyncState::RemoteMissing => ("Not on server", theme.warning),
                    LibrarySyncState::LocalOnly => ("On this device", theme.text_muted),
                }
            };
            let (status, status_color) = if row.status.pending_provider_uploads != 0 && !row.status.issues.has_issues() {
                let count = row.status.pending_provider_uploads;
                (format!("{count} {} waiting for upload", if count == 1 { "book" } else { "books" }), theme.warning)
            } else {
                (status.to_owned(), status_color)
            };
            let library_id = row.status.library_id;
            let (status, status_color) = if self.storage_copy_pending.contains(&library_id) { ("Copying library…".to_owned(), theme.accent) } else { (status, status_color) };
            // The folder is the button that opens it, labelled with its path.
            #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            let path = row.local_path.as_ref().map(|path| {
                let destination = std::path::PathBuf::from(path);
                components::browser_settings_path_button(SharedString::from(format!("library-path-{library_id}")), components::library_path_label(&destination), theme)
                    .max_w_full()
                    .overflow_hidden()
                    .tooltip(path.clone())
                    .on_click(move |_, _, cx| cx.open_with_system(&destination))
                    .into_any_element()
            });
            #[cfg(not(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows"))))]
            let path: Option<gpui::AnyElement> = None;
            // The column heading names the switch; stacked, with no heading,
            // the switch names itself.
            let cloud_page = page.clone();
            let cloud = if row.status.storage.external().is_some() && !app::google_drive_available() {
                gpui::div().into_any_element()
            } else if row.status.storage.external().is_some() || row.status.asset_storage_enabled {
                let provider = match row.status.storage.external().map(|root| root.scope.provider) {
                    Some(app::CloudProvider::GoogleDrive) => "Google Drive",
                    Some(app::CloudProvider::Dropbox) => "Dropbox",
                    Some(app::CloudProvider::OneDrive) => "OneDrive",
                    Some(app::CloudProvider::WebDav) => "WebDAV",
                    Some(app::CloudProvider::S3) => "S3",
                    None => "Bokheim cloud",
                };
                let bokheim = self.state.read(cx).account_status.signed_in() && row.status.storage.external().is_some();
                let trigger = components::outlined_icon_button(SharedString::from(format!("library-storage-{library_id}")), format!("{provider} · Library storage"), Icon::empty().path("icons/cloud.svg"))
                    .disabled(self.storage_copy_pending.contains(&library_id));
                trigger
                    .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                        let local = cloud_page.clone();
                        let mut menu = menu
                            .touch_friendly(compact || components::touch_ui())
                            .item(components::menu_item_with_icon("Copy to local storage", IconName::HardDrive, move |_, _, cx| local.update(cx, |page, cx| page.copy_storage(library_id, false, cx))));
                        if bokheim {
                            let cloud = cloud_page.clone();
                            menu = menu.item(components::menu_item_with_icon("Copy to Bokheim cloud", Icon::empty().path("icons/cloud.svg"), move |_, _, cx| cloud.update(cx, |page, cx| page.copy_storage(library_id, true, cx))));
                        }
                        menu
                    })
                    .into_any_element()
            } else {
                Checkbox::new(SharedString::from(format!("asset-storage-{library_id}")))
                    .when(compact, |checkbox| checkbox.label("Bokheim"))
                    .checked(false)
                    .disabled(self.asset_storage_pending.contains(&library_id))
                    .on_click(move |enabled, _, cx| cloud_page.update(cx, |page, cx| page.change_asset_storage(library_id, *enabled, cx)))
                    .into_any_element()
            };
            libraries = libraries.child(components::library_table_row(row.name.clone(), path, local_bytes, (status, status_color), cloud, compact, theme).when(!has_rows, |row| row.border_t_0()));
            has_rows = true;
        }
        libraries
    }

    /// Chooses the folder new libraries are created in.
    #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
    fn choose_library_location(&mut self, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose default library location".into()) });
        let backend = self.services.backend.clone();
        cx.spawn(async move |page, cx| {
            let Ok(Ok(Some(paths))) = selection.await else { return };
            let Some(path) = paths.first() else { return };
            let result = backend.set_default_library_save_location(path.to_string_lossy().into_owned()).await;
            let _ = page.update(cx, |page, cx| {
                match result {
                    Ok(path) => {
                        page.default_library_location = Some(path);
                        page.error = None;
                    }
                    Err(error) => page.error = Some(user_facing_error(&error, "Could not save the library location. Choose an accessible folder and try again.").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SyncPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.settings_section(window, cx)
    }
}
