//! Owns authentication inputs and sign-in, registration, verification, and reset flows.

use gpui::{AppContext, Context, Entity, Window};
use gpui_component::input::{InputState, OtpState};

use crate::model::AccountFlow;
use crate::services::UiFuture;

use super::SyncPage;

pub(super) struct AuthenticationInputs {
    pub(super) login_email: Entity<InputState>,
    pub(super) password: Entity<InputState>,
    pub(super) email: Entity<InputState>,
    pub(super) registration_password: Entity<InputState>,
    pub(super) verification_pin: Entity<OtpState>,
    pub(super) reset_token: Entity<InputState>,
    pub(super) new_password: Entity<InputState>,
}

impl AuthenticationInputs {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SyncPage>) -> Self {
        let verification_pin = cx.new(|cx| OtpState::new(6, window, cx));
        let mut input = |placeholder: &'static str, masked: bool, cx: &mut Context<SyncPage>| cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked));
        Self {
            login_email: input("Email", false, cx),
            password: input("Password", true, cx),
            email: input("Email", false, cx),
            registration_password: input("Password", true, cx),
            verification_pin,
            reset_token: input("Reset token", false, cx),
            new_password: input("New password", true, cx),
        }
    }
}

impl SyncPage {
    /// Leaving a flow re-masks every password, so a field revealed on one
    /// screen is never left in clear text behind another.
    pub(super) fn set_flow(&mut self, flow: AccountFlow, window: &mut Window, cx: &mut Context<Self>) {
        if self.flow == flow && self.error.is_none() && self.message.is_none() {
            return;
        }
        self.password_revealed = false;
        for state in [&self.authentication.password, &self.authentication.registration_password, &self.authentication.new_password] {
            state.update(cx, |state, cx| state.set_masked(true, window, cx));
        }
        self.flow = flow;
        self.login_failed = false;
        self.error = None;
        self.message = None;
        cx.notify();
    }

    fn start_account_request(&mut self, progress: &'static str, success: &'static str, next_flow: AccountFlow, operation: UiFuture<()>, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.activity = super::AccountActivity::AccountRequest;
        self.error = None;
        self.message = Some(progress.into());
        cx.notify();
        self.task = Some(cx.spawn(async move |page, cx| {
            let result = operation.await;
            let _ = page.update(cx, |page, cx| {
                page.activity = super::AccountActivity::Idle;
                match result {
                    Ok(()) => {
                        page.password_revealed = false;
                        page.flow = next_flow;
                        page.message = Some(success.into());
                    }
                    Err(error) => {
                        page.message = None;
                        page.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn start_login(&mut self, cx: &mut Context<Self>) {
        let email = self.authentication.login_email.read(cx).value().trim().to_owned();
        let password = self.authentication.password.read(cx).value().to_string();
        if email.is_empty() || password.is_empty() {
            self.error = Some("Email and password are required".into());
            cx.notify();
            return;
        }
        let operation = super::account::login(&self.services.backend, email, password);
        self.start_snapshot_operation("Signing in…", "Signed in", true, operation, cx);
    }

    pub(super) fn start_registration(&mut self, cx: &mut Context<Self>) {
        let email = self.authentication.email.read(cx).value().trim().to_owned();
        let password = self.authentication.registration_password.read(cx).value().to_string();
        if email.is_empty() || password.chars().count() < account_contract::PASSWORD_MIN_CHARACTERS {
            self.error = Some("Check the email and password length".into());
            cx.notify();
            return;
        }
        let backend = self.services.backend.clone();
        let operation = Box::pin(async move { backend.request_public_registration(email, password).await });
        self.start_account_request("Creating account…", "Check your email for a six-digit verification PIN", AccountFlow::VerifyEmail, operation, cx);
    }

    pub(super) fn start_resend_verification(&mut self, cx: &mut Context<Self>) {
        let email = self.authentication.email.read(cx).value().trim().to_owned();
        if email.is_empty() {
            self.error = Some("Email is required".into());
            cx.notify();
            return;
        }
        let backend = self.services.backend.clone();
        let operation = Box::pin(async move { backend.resend_verification(email).await });
        self.start_account_request("Resending PIN…", "Verification PIN sent if the address exists", AccountFlow::VerifyEmail, operation, cx);
    }

    pub(super) fn start_verify_email(&mut self, cx: &mut Context<Self>) {
        let email = self.authentication.email.read(cx).value().trim().to_owned();
        let pin = self.authentication.verification_pin.read(cx).value().trim().to_owned();
        let pin_digits = account_contract::EMAIL_VERIFICATION_PIN_DIGITS;
        if email.is_empty() || pin.len() != pin_digits || !pin.bytes().all(|byte| byte.is_ascii_digit()) {
            self.error = Some(format!("Enter the {pin_digits}-digit verification PIN").into());
            cx.notify();
            return;
        }
        let operation = super::account::verify_email(&self.services.backend, email, pin);
        self.start_snapshot_operation("Verifying email…", "Email verified", false, operation, cx);
    }

    pub(super) fn start_password_reset_request(&mut self, cx: &mut Context<Self>) {
        let email = self.authentication.email.read(cx).value().trim().to_owned();
        if email.is_empty() {
            self.error = Some("Email is required".into());
            cx.notify();
            return;
        }
        let backend = self.services.backend.clone();
        let operation = Box::pin(async move { backend.request_password_reset(email).await });
        self.start_account_request("Requesting reset…", "Reset token sent if the address exists", AccountFlow::ResetPassword, operation, cx);
    }

    pub(super) fn start_password_reset(&mut self, cx: &mut Context<Self>) {
        let token = self.authentication.reset_token.read(cx).value().trim().to_owned();
        let password = self.authentication.new_password.read(cx).value().to_string();
        if token.is_empty() || password.chars().count() < account_contract::PASSWORD_MIN_CHARACTERS {
            self.error = Some("Reset token and a long enough password are required".into());
            cx.notify();
            return;
        }
        let backend = self.services.backend.clone();
        let operation = Box::pin(async move { backend.reset_password(token, password).await });
        self.start_account_request("Updating password…", "Password updated. Sign in with your new password.", AccountFlow::SignIn, operation, cx);
    }
}
