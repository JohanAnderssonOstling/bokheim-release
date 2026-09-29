//! Sync and account presentation, rendered as the first section of Settings.
//!
//! The section leads with identity — who is signed in, with the sync verdict as
//! one line inside that block — then the quota as a shape, then the libraries
//! sorted by what they occupy.
//!
//! What the device is uploading and downloading is reported on the library it
//! belongs to rather than in a queue of its own: a job is only ever interesting
//! as something *a library* is doing, and a list beneath the libraries made the
//! reader match names between two lists to find that out.

use super::*;
use crate::services::format_storage_bytes;
use gpui_component::Disableable;
use gpui_component::checkbox::Checkbox;
use gpui_component::menu::DropdownMenu as _;

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
        let page = self.page.clone();
        // What can be done to an account is two things done rarely, so they
        // hang off the address rather than standing beside it: a header that
        // carried a permanent "Sign out" button made leaving the account the
        // most prominent thing about having one.
        components::account_chip("account-menu", account_initial(&email), email.clone(), theme).tooltip(email).dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
            let password = page.clone();
            let logout = page.clone();
            let menu = menu;
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            let menu = menu.item(components::menu_item("Change password…", move |_, window, cx| {
                password.update(cx, |page, cx| page.open_password_change(window, cx));
            }));
            #[cfg(any(target_arch = "wasm32", target_os = "android"))]
            let _ = password;
            menu.item(components::menu_item("Log out", move |_, _, cx| {
                logout.update(cx, |page, cx| page.start_logout(cx));
            }))
        })
    }
}

/// The libraries, as a view of their own.
///
/// Settings draws Account, then Appearance, then this — so the two halves of
/// what [`SyncPage`] knows are rendered in two places with a group of unrelated
/// settings between them. A view of its own is also the right boundary for the
/// content: transfer progress reaches these rows several times a second, and
/// nothing above them should be rebuilt at that rate.
pub(crate) struct LibrariesView {
    page: Entity<SyncPage>,
    expanded: std::collections::HashSet<sync_common::LibraryId>,
    _updates: gpui::Subscription,
}

impl LibrariesView {
    pub(crate) fn new(page: Entity<SyncPage>, cx: &mut Context<Self>) -> Self {
        let updates = cx.observe(&page, |_, _, cx| cx.notify());
        Self { page, expanded: std::collections::HashSet::new(), _updates: updates }
    }
}

impl Render for LibrariesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = components::browser_theme(cx);
        let compact = components::WindowWidthClass::for_window(window).is_compact();
        let page = self.page.clone();
        let rows = page.read(cx).library_rows(cx);
        SyncPage::libraries_section(page.read(cx), &page, &rows, &self.expanded, cx.entity(), compact, theme)
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
    thumbnail_download_coverage: Option<(u64, u64)>,
    /// What the library is doing at this moment, if anything.
    activities: Vec<LibraryActivitySummary>,
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
                let activity_jobs = self.activity_jobs.read(cx);
                // Both halves of the work a library is doing report on the
                // library itself. The uploads used to be listed under the quota
                // instead, which is where the bytes are, not where the job is.
                let activities = activity_jobs.summaries(library.library_id()).into_iter().chain(activity_jobs.server_summaries(library.library_id())).collect();
                LibraryRow {
                    name: library.library_name().to_owned(),
                    status,
                    server_bytes,
                    local_bytes,
                    thumbnail_download_coverage: activity_jobs.thumbnail_download_coverage(library.library_id()),
                    activities,
                    #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
                    local_path: library.local_path().map(str::to_owned),
                }
            })
            .collect();
        // Sorted by the quantity the page leads with, falling back to the local
        // sizes while no server breakdown is available.
        rows.sort_by(|left, right| right.sort_bytes().cmp(&left.sort_bytes()).then_with(|| left.name.cmp(&right.name)));
        rows
    }

    /// A password field whose reveal stays on until it is pressed again.
    ///
    /// The input's own `mask_toggle` unmasks only while the mouse button is
    /// held, which cannot be used to check a password as it is typed and does
    /// not survive a touch at all.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    fn password_input(&self, state: &Entity<InputState>, id: &'static str, entity: &Entity<Self>) -> Input {
        let revealed = self.password_revealed;
        let toggle = entity.clone();
        let toggled_state = state.clone();
        Input::new(state).suffix(components::base_button(id).icon(if revealed { IconName::EyeOff } else { IconName::Eye }).xsmall().ghost().tooltip(if revealed { "Hide password" } else { "Show password" }).on_click(move |_, window, cx| {
            toggled_state.update(cx, |state, cx| state.set_masked(revealed, window, cx));
            toggle.update(cx, |page, cx| {
                page.password_revealed = !revealed;
                cx.notify();
            });
        }))
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
                let login = entity.clone();
                let register = entity.clone();
                let reset = entity.clone();
                let mut actions = components::action_row().child(components::primary_action_button("account-sign-in", "Sign in", IconName::Globe, self.is_busy()).on_click(move |_, _, cx| {
                    login.update(cx, |page, cx| page.start_login(cx));
                }));
                actions = actions.child(components::secondary_action_button("account-register", "Create account", IconName::User, self.is_busy()).on_click(move |_, window, cx| {
                    register.update(cx, |page, cx| page.set_flow(AccountFlow::Register, window, cx));
                }));
                let mut form = form
                    .child(components::section_header("Sign in", "", theme))
                    .children(feedback)
                    .child(Input::new(&self.authentication.login_email))
                    .child(self.password_input(&self.authentication.password, "toggle-sign-in-password", entity))
                    .child(actions);
                // Recovery is offered once a sign-in has actually failed, so
                // the screen a returning user sees carries one action, not
                // three. It stays a link: it is the way out, not the way on.
                if self.login_failed {
                    form = form.child(components::link_button("account-request-reset", "Forgot your password?", theme, self.is_busy()).on_click(move |_, window, cx| {
                        reset.update(cx, |page, cx| page.set_flow(AccountFlow::RequestPasswordReset, window, cx));
                    }));
                }
                form
            }
            AccountFlow::Register => {
                let register = entity.clone();
                let sign_in = entity.clone();
                form.child(components::section_header("Create account", "", theme))
                    .children(feedback)
                    .child(Input::new(&self.authentication.email))
                    .child(self.password_input(&self.authentication.registration_password, "toggle-registration-password", entity))
                    .child(components::form_hint(format!("At least {} characters.", account_contract::PASSWORD_MIN_CHARACTERS), theme))
                    .child(components::action_row().child(components::primary_action_button("account-register", "Create account", IconName::User, self.is_busy()).on_click(move |_, _, cx| {
                        register.update(cx, |page, cx| page.start_registration(cx));
                    })))
                    .child(components::link_button("account-sign-in", "Already have an account? Sign in", theme, self.is_busy()).on_click(move |_, window, cx| {
                        sign_in.update(cx, |page, cx| page.set_flow(AccountFlow::SignIn, window, cx));
                    }))
            }
            AccountFlow::VerifyEmail => {
                let verify = entity.clone();
                let resend = entity.clone();
                let start_over = entity.clone();
                let mut actions = components::action_row().child(components::primary_action_button("account-verify", "Verify email", IconName::ArrowRight, self.is_busy()).on_click(move |_, _, cx| {
                    verify.update(cx, |page, cx| page.start_verify_email(cx));
                }));
                actions = actions.child(components::secondary_action_button("account-resend", "Resend PIN", IconName::Redo2, self.is_busy()).on_click(move |_, _, cx| {
                    resend.update(cx, |page, cx| page.start_resend_verification(cx));
                }));
                form.child(components::section_header("Check your email", "", theme)).children(feedback).child(OtpInput::new(&self.authentication.verification_pin)).child(actions).child(
                    components::link_button("account-restart-registration", "Wrong address? Start over", theme, self.is_busy()).on_click(move |_, window, cx| {
                        start_over.update(cx, |page, cx| page.set_flow(AccountFlow::Register, window, cx));
                    }),
                )
            }
            AccountFlow::RequestPasswordReset => {
                let request = entity.clone();
                let sign_in = entity.clone();
                form.child(components::section_header("Reset your password", "", theme))
                    .children(feedback)
                    .child(Input::new(&self.authentication.email))
                    .child(components::action_row().child(components::primary_action_button("account-request-reset", "Email me a reset token", IconName::Redo2, self.is_busy()).on_click(move |_, _, cx| {
                        request.update(cx, |page, cx| page.start_password_reset_request(cx));
                    })))
                    .child(components::link_button("account-sign-in", "Back to sign in", theme, self.is_busy()).on_click(move |_, window, cx| {
                        sign_in.update(cx, |page, cx| page.set_flow(AccountFlow::SignIn, window, cx));
                    }))
            }
            AccountFlow::ResetPassword => {
                let reset = entity.clone();
                let sign_in = entity.clone();
                // The token is a pasted string, not the six-digit PIN, so it is
                // a plain input rather than the OTP boxes — drawing them alike
                // invites people to type six of its characters and give up.
                form.child(components::section_header("Set a new password", "Paste the token from the email. It is not the six-digit PIN.", theme))
                    .children(feedback)
                    .child(Input::new(&self.authentication.reset_token))
                    .child(self.password_input(&self.authentication.new_password, "toggle-new-password", entity))
                    .child(components::form_hint(format!("At least {} characters.", account_contract::PASSWORD_MIN_CHARACTERS), theme))
                    .child(components::action_row().child(components::primary_action_button("account-reset-password", "Set new password", IconName::Redo2, self.is_busy()).on_click(move |_, _, cx| {
                        reset.update(cx, |page, cx| page.start_password_reset(cx));
                    })))
                    .child(components::link_button("account-sign-in", "Back to sign in", theme, self.is_busy()).on_click(move |_, window, cx| {
                        sign_in.update(cx, |page, cx| page.set_flow(AccountFlow::SignIn, window, cx));
                    }))
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
    fn storage_segments(&self, usage: AccountStorageUsage, theme: components::BrowserTheme, state: &SyncState) -> Vec<(SharedString, u64, gpui::Hsla)> {
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
    /// The account group: who is signed in — in the header, as the control the
    /// group is about — and what that account holds.
    ///
    /// Cloud storage used to be a group of its own, one gap below the identity
    /// that owns it. A quota is not a second subject: it is the answer to "what
    /// does this account get me", so it is the body of the group whose header
    /// asks the question. Signed out there is no quota, and the group is its
    /// header alone.
    pub(crate) fn settings_section(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = components::browser_theme(cx);
        let entity = cx.entity();
        // Whichever of the two the last attempt produced. They are never both
        // set, and one slot means a form cannot grow two lines of chrome.
        let feedback = match (&self.error, &self.message) {
            (Some(error), _) => Some(Alert::error("account-error", error.clone()).with_size(Size::Small).into_any_element()),
            (None, Some(message)) => Some(Alert::info("account-message", message.clone()).with_size(Size::Small).into_any_element()),
            (None, None) => None,
        };

        let signed_in = self.state.read(cx).account_status.signed_in();
        let mut account = components::browser_settings_fieldset_with_actions("Account", self.account_header_actions(&entity, signed_in, theme), theme);

        if signed_in {
            account = account.child(self.storage_body(theme, cx).px(px(components::SPACE_MD)).py(px(components::SPACE_SM)));
        }

        // The form is not the group's resting state on any platform any more:
        // it appears because a header button asked for it, and it is the only
        // thing in the group while it is open.
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        if self.access_open {
            // Every form here can be abandoned: the header opened it, and until
            // it is finished with there is no other way back to the account the
            // group is about.
            let close = entity.clone();
            let form = self.account_access_form(&entity, feedback, theme).child(components::link_button("account-close", "Close", theme, self.is_busy()).on_click(move |_, _, cx| {
                close.update(cx, |page, cx| page.close_account_access(cx));
            }));
            return account.child(form.px(px(components::SPACE_MD)).py(px(components::SPACE_SM)));
        }

        account.children(feedback.map(|feedback| div().px(px(components::SPACE_MD)).py(px(components::SPACE_SM)).child(feedback)))
    }

    /// The quota as a figure and a shape.
    fn storage_body(&self, theme: components::BrowserTheme, cx: &Context<Self>) -> Div {
        let state = self.state.read(cx);
        let Some(usage) = state.storage_usage else {
            return components::storage_block().child("Loading cloud storage…");
        };
        let mut storage = components::storage_block().child(components::storage_figures(
            format_storage_bytes(usage.used_bytes),
            format!("of {} used", format_storage_bytes(usage.quota_bytes)),
            format!("{} available", format_storage_bytes(usage.available_bytes())),
            theme,
        ));
        if usage.quota_bytes > 0 {
            let segments = self.storage_segments(usage, theme, &state).into_iter().map(|(label, bytes, color)| (bytes as f32 / usage.quota_bytes as f32, color, SharedString::from(format!("{label} {}", format_storage_bytes(bytes)))));
            storage = storage.child(components::storage_meter(segments, theme));
        }
        storage
    }

    /// The control in the account group's header: the two ways in, or who came
    /// in that way.
    ///
    /// Signing in leads with `Log in` rather than with `Create account`: a
    /// person who opens Settings and finds themselves signed out is far more
    /// often someone whose session lapsed than someone who has never had one.
    fn account_header_actions(&self, entity: &Entity<Self>, signed_in: bool, theme: components::BrowserTheme) -> gpui::AnyElement {
        if signed_in {
            return self.identity.clone().into_any_element();
        }
        let register = entity.clone();
        let login = entity.clone();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(components::SPACE_SM))
            .child(components::browser_settings_header_button("account-open-register", "Create account", false, theme).on_click(move |_, window, cx| {
                register.update(cx, |page, cx| page.open_account_access(true, window, cx));
            }))
            .child(components::browser_settings_header_button("account-open-sign-in", "Log in", true, theme).on_click(move |_, window, cx| {
                login.update(cx, |page, cx| page.open_account_access(false, window, cx));
            }))
            .into_any_element()
    }

    /// Reveals the form for `flow` under the account header.
    ///
    /// On the platforms that authenticate through a browser or the system
    /// account picker there is no form to reveal, so the same press opens that
    /// instead — the header reads the same everywhere, and what it opens is the
    /// platform's business.
    pub(super) fn open_account_access(&mut self, register: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = (register, &window);
        #[cfg(target_arch = "wasm32")]
        self.open_web_authentication(cx);
        #[cfg(target_os = "android")]
        self.open_android_authentication(cx);
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        {
            self.access_open = true;
            self.set_flow(if register { AccountFlow::Register } else { AccountFlow::SignIn }, window, cx);
        }
        cx.notify();
    }

    /// Opens the password-reset flow for someone who is already signed in.
    ///
    /// There is no separate "change password while signed in" exchange — the
    /// reset is the only one the server offers — so this is that flow, reached
    /// from the account menu instead of from a failed sign-in.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub(super) fn open_password_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.access_open = true;
        self.set_flow(AccountFlow::RequestPasswordReset, window, cx);
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
    fn libraries_section(&self, page: &Entity<Self>, rows: &[LibraryRow], expanded: &std::collections::HashSet<sync_common::LibraryId>, view: Entity<LibrariesView>, compact: bool, theme: components::BrowserTheme) -> Div {
        let mut libraries = components::browser_settings_fieldset("Libraries", theme);

        // The list is ruled off from the setting above it. Without that the
        // first library reads as the setting's own second line.
        #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
        let ruled = self.default_library_location.is_some();
        #[cfg(not(all(feature = "filesystem-libraries", not(target_arch = "wasm32"))))]
        let ruled = false;
        #[cfg(all(feature = "filesystem-libraries", not(target_arch = "wasm32")))]
        if let Some(path) = self.default_library_location.clone() {
            let page = page.clone();
            let choose = components::browser_settings_path_button("default-library-location", components::library_path_label(std::path::Path::new(&path)), theme)
                .tooltip(path)
                .on_click(move |_, _, cx| page.update(cx, |page, cx| page.choose_library_location(cx)));
            libraries = libraries.child(components::browser_settings_compact_row("Default location for new libraries", choose, theme));
        }

        if rows.is_empty() {
            return libraries.child(components::library_usage_empty("No local libraries", theme).when(ruled, |empty| empty.border_t_1().border_color(theme.rule)));
        }
        let mut usage_rows = Vec::with_capacity(rows.len());
        for row in rows {
            let activity = row.activities.iter().map(|activity| {
                let color = match activity.state {
                    ActivityState::Running => theme.accent,
                    ActivityState::Waiting => theme.warning,
                };
                components::library_activity_note(color, activity.text.clone(), activity.count.clone().map(SharedString::from), activity.progress, theme)
            });
            let local_bytes = row.local_bytes.map(format_storage_bytes).unwrap_or_else(|| if row.status.state == LibrarySyncState::Unavailable { "Unavailable".to_owned() } else { "Calculating…".to_owned() });
            let (status, status_color) = match row.status.state {
                LibrarySyncState::Checking => ("Loading status…", theme.text_muted),
                LibrarySyncState::Unavailable => ("Unavailable", theme.warning),
                LibrarySyncState::Receiving => ("Receiving library…", theme.accent),
                LibrarySyncState::Synced => ("Up to date", theme.accent),
                LibrarySyncState::SyncPending if row.status.last_synced_at.is_none() => ("Sync pending", theme.warning),
                LibrarySyncState::SyncPending => ("Changes pending", theme.warning),
                LibrarySyncState::RemoteMissing => ("Not on server", theme.warning),
                LibrarySyncState::LocalOnly => ("On this device", theme.text_muted),
            };
            let library_id = row.status.library_id;
            let is_expanded = expanded.contains(&library_id);
            let toggle_view = view.clone();
            let toggle = components::outlined_icon_button(SharedString::from(format!("library-details-{library_id}")), if is_expanded { "Hide library details" } else { "Show library details" }, IconName::Ellipsis, theme)
                .on_click(move |_, _, cx| toggle_view.update(cx, |view, cx| {
                    if !view.expanded.insert(library_id) {
                        view.expanded.remove(&library_id);
                    }
                    cx.notify();
                }));
            #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
            let path = row.local_path.as_ref().map(|path| components::library_path_label(std::path::Path::new(path)));
            #[cfg(not(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows"))))]
            let path: Option<String> = None;
            let title = div().min_w_0().flex_1().flex().flex_col()
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(gpui::FontWeight::SEMIBOLD).text_size(gpui::rems(components::TEXT_SM)).text_color(theme.text).child(row.name.clone()))
                .children(path.map(|path| div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(gpui::rems(components::TEXT_XS)).text_color(theme.text_muted).child(path)));
            let size = div().flex_none().text_size(gpui::rems(components::TEXT_XS)).text_color(theme.text_muted).child(local_bytes.clone());
            let state = div().min_w_0().flex().items_center().gap(px(6.0)).text_size(gpui::rems(components::TEXT_XS)).text_color(theme.text)
                .child(div().w(px(8.0)).h(px(8.0)).rounded(px(4.0)).bg(status_color))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(status));
            let meta = if compact { div().w_full().flex().items_center().gap(px(12.0)).child(size).child(state.flex_1()).child(toggle) }
                else { div().flex_none().flex().items_center().gap(px(12.0)).child(size).child(state).child(toggle) };
            let heading = if compact { div().w_full().flex().flex_col().gap(px(6.0)).child(title).child(meta) }
                else { div().w_full().flex().items_center().gap(px(14.0)).child(title).child(meta) };
            let mut summary = div().w_full().px(px(components::SPACE_MD)).py(px(12.0)).flex().flex_col().gap(px(8.0)).child(heading).children(activity);
            if is_expanded {
                let mut facts: Vec<(SharedString, gpui::AnyElement)> = Vec::new();
                if let Some((ready, available)) = row.thumbnail_download_coverage.filter(|(_, available)| *available > 0) {
                    facts.push(("Thumbnails on this device".into(), SharedString::from(format!("{ready} / {available}")).into_any_element()));
                }

                #[cfg(all(feature = "filesystem-libraries", any(target_os = "linux", target_os = "macos", target_os = "windows")))]
                if let Some(path) = row.local_path.as_ref() {
                    let destination = std::path::PathBuf::from(path);
                    facts.push((
                        "Folder".into(),
                        components::browser_settings_path_button(SharedString::from(format!("library-path-{}", row.status.library_id)), components::library_path_label(&destination), theme)
                            .max_w_full()
                            .overflow_hidden()
                            .tooltip(path.clone())
                            .on_click(move |_, _, cx| cx.open_with_system(&destination))
                            .into_any_element(),
                    ));
                }

                let page = page.clone();
                facts.push((
                    SharedString::default(),
                    Checkbox::new(SharedString::from(format!("asset-storage-{library_id}")))
                        .label("Store on cloud")
                        .checked(row.status.asset_storage_enabled)
                        .disabled(self.asset_storage_pending.contains(&library_id))
                        .on_click(move |enabled, _, cx| page.update(cx, |page, cx| page.change_asset_storage(library_id, *enabled, cx)))
                        .into_any_element(),
                ));

                summary = summary.child(components::library_usage_details(facts, compact, theme).pt(px(8.0)).border_t_1().border_color(theme.rule));
            }
            usage_rows.push(summary);
        }
        libraries.child(components::library_usage_list(usage_rows, theme).when(ruled, |list| list.border_t_1().border_color(theme.rule)))
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
                    Err(error) => page.error = Some(error.into()),
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
