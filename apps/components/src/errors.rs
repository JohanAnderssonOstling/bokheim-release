//! Wording for failures crossing the backend/UI boundary. Errors arrive
//! classified by where they arose; their text is only logged.

use operation_failure::{Classified, Failure, WifiFailure};

/// A known recovery step for `error`, or the caller's action-specific
/// fallback. Never shows the error's own text.
pub fn user_facing_error(error: &(impl Classified + ?Sized), fallback: &'static str) -> &'static str {
    log::warn!("{fallback} Diagnostic: {error}");
    error.failure().and_then(recovery).unwrap_or(fallback)
}

fn recovery(failure: Failure) -> Option<&'static str> {
    Some(match failure {
        Failure::StorageFull => "There isn't enough free space. Free up space on the device or drive that holds your library, then try again.",
        Failure::CloudStorageFull => "Your cloud storage account is full. Free up space in that account, then retry.",
        Failure::ReadOnlyStorage => "Bokheim can't save changes here. Allow changes to the library folder or choose a writable library location, then try again.",
        Failure::PermissionDenied => "Bokheim can't access the files it needs. Check that you have permission to use the folder, then try again.",
        Failure::StorageBusy => "Another Bokheim tab is using browser storage. Close your other Bokheim tabs, then reload this page.",
        Failure::WebViewMissing => "The system browser component is missing. Install WebKitGTK 4.1, then reopen Bokheim and try again.",
        Failure::InvalidAddress => "Enter a complete web address starting with https:// or http://.",
        Failure::NotACatalog => "This address doesn't provide a supported book catalog. Check the catalog's feed address and try again.",
        Failure::CatalogCredentialsRejected => "The catalog didn't accept those sign-in details. Check your username and password, then try again.",
        Failure::Unauthorized | Failure::SignInRequired => "Sign in again, then retry the action.",
        Failure::Forbidden => "Access was refused. Check that you're signed in with an account that has access, then try again.",
        Failure::NotFound => "This item is no longer available. Refresh the page or choose another item.",
        Failure::RateLimited => "Too many requests were made. Please wait a few minutes, then try again.",
        Failure::ServiceUnavailable => "The service is temporarily unavailable. Please try again later.",
        Failure::TimedOut => "The request took too long. Check your connection, then try again.",
        Failure::Offline => "Bokheim couldn't connect. Check your internet connection, then try again.",
        Failure::LibraryFolderUnavailable => "The library folder isn't available. Reconnect its drive or choose an available library, then try again.",
        Failure::AccountChanged => "Your account changed while this was in progress. Please try again.",
        Failure::DrmProtected => "This file is not supported. Bokheim cannot read DRM-protected books.",
        Failure::UnreadableBook => "This book's file couldn't be read. Try downloading it again or choose another edition.",
        Failure::Wifi(failure) => wifi_message(failure),
        Failure::Rejected => return None,
    })
}

/// Wi-Fi failures need recovery steps that work while the device is offline.
pub fn wifi_error_message(error: &(impl Classified + ?Sized)) -> &'static str {
    log::warn!("Wi-Fi request failed: {error}");
    match error.failure() {
        Some(Failure::Wifi(failure)) => wifi_message(failure),
        _ => "Could not update Wi-Fi. Try again, or restart Bokheim if the problem continues.",
    }
}

fn wifi_message(failure: WifiFailure) -> &'static str {
    match failure {
        WifiFailure::InvalidPassword => "Enter the network password using 8 to 63 standard keyboard characters.",
        WifiFailure::AuthenticationFailed => "The Wi-Fi password wasn't accepted. Check the network password and try again.",
        WifiFailure::UnsupportedSecurity => "This network uses a security type Bokheim doesn't support. Try another Wi-Fi network.",
        WifiFailure::NetworkGone => "This Wi-Fi network is no longer available. Tap Refresh and choose an available network.",
        WifiFailure::InvalidNetworkName => "Enter a network name. If you've already entered one, try a shorter name.",
        WifiFailure::ScanTimedOut => "The Wi-Fi scan took too long. Tap Refresh to try again.",
        WifiFailure::NoAddress => "Could not get a network address. Try connecting again, or check your router.",
        WifiFailure::ConnectTimedOut => "Could not connect to Wi-Fi in time. Check the network password and move closer to the router, then try again.",
        WifiFailure::Sleeping => "Wi-Fi is unavailable while your Kobo is going to sleep. Wake the device before trying again.",
        WifiFailure::PowerControlUnavailable => "Bokheim can't switch Wi-Fi on or off on this device. Use the device's Wi-Fi controls instead.",
    }
}

/// The account operations whose failures the forms explain, shared by the
/// desktop, Android, and browser account screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountAction {
    Login,
    Register,
    Verify,
    Resend,
    RequestReset,
    Reset,
    DeleteAccount,
}

pub fn authentication_error(error: &(impl Classified + ?Sized), action: AccountAction) -> &'static str {
    log::warn!("Account action {action:?} failed: {error}");
    let failure = error.failure();
    // The server refused these credentials or this request as made.
    let refused = matches!(failure, Some(Failure::Unauthorized | Failure::Rejected));
    let explained = match action {
        AccountAction::DeleteAccount => match failure {
            Some(Failure::Unauthorized) => Some("The password is incorrect. Check your current password and try again."),
            Some(Failure::NotFound) => Some("Account deletion is not available on the server yet. Please try again later."),
            Some(Failure::AccountChanged) => Some("Your account changed. Reopen account deletion and try again."),
            _ => None,
        },
        AccountAction::Login if refused => Some("Could not sign in. Check your email and password, then try again. You can reset your password if you've forgotten it."),
        AccountAction::Verify if refused => Some("Could not verify your email. Check the PIN from your email and the password you chose, or request a new PIN and try again."),
        AccountAction::Reset if refused => Some("Could not reset your password. Request a new reset code and use a password that meets the length requirement."),
        AccountAction::Register if refused => Some("Could not create the account. Check your email and password. If you already have an account, sign in or reset your password."),
        _ => None,
    };
    let fallback = match action {
        AccountAction::DeleteAccount => "Could not confirm account deletion. Check your connection and account status before trying again.",
        AccountAction::Login => "Could not sign in. Please try again.",
        AccountAction::Verify => "Could not verify your email. Please try again.",
        AccountAction::Reset => "Could not reset your password. Please try again.",
        AccountAction::Register => "Could not create the account. Please try again.",
        AccountAction::Resend => "Could not send a verification PIN. Check your email address and try again.",
        AccountAction::RequestReset => "Could not send a password reset email. Check your email address and try again.",
    };
    explained.or_else(|| failure.and_then(recovery)).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::{authentication_error, user_facing_error, wifi_error_message, AccountAction};
    use operation_failure::{Classified, Failure, WifiFailure};
    use std::fmt;

    struct Failed(Option<Failure>);
    impl fmt::Display for Failed {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("private diagnostic /home/reader")
        }
    }
    impl Classified for Failed {
        fn failure(&self) -> Option<Failure> {
            self.0
        }
    }

    #[test]
    fn deletion_errors_explain_password_and_endpoint_failures() {
        assert!(authentication_error(&Failed(Some(Failure::Unauthorized)), AccountAction::DeleteAccount).contains("password is incorrect"));
        assert!(authentication_error(&Failed(Some(Failure::NotFound)), AccountAction::DeleteAccount).contains("not available on the server"));
        assert!(authentication_error(&Failed(Some(Failure::AccountChanged)), AccountAction::DeleteAccount).contains("Reopen account deletion"));
    }

    #[test]
    fn unclassified_errors_show_the_fallback_never_their_text() {
        let fallback = "Could not save this change. Please try again.";
        assert_eq!(user_facing_error(&Failed(None), fallback), fallback);
        assert_eq!(user_facing_error("No space left on device (os error 28)", fallback), fallback);
        assert_eq!(user_facing_error(&Failed(Some(Failure::Rejected)), fallback), fallback);
        assert!(!authentication_error(&Failed(None), AccountAction::Login).contains("private"));
    }

    #[test]
    fn authentication_recovery_depends_on_the_action() {
        let refused = Failed(Some(Failure::Unauthorized));
        assert!(authentication_error(&refused, AccountAction::Login).contains("Check your email and password"));
        assert!(authentication_error(&refused, AccountAction::Verify).contains("request a new PIN"));
        assert!(authentication_error(&refused, AccountAction::Reset).contains("new reset code"));
        assert!(authentication_error(&Failed(Some(Failure::RateLimited)), AccountAction::Login).contains("wait a few minutes"));
        assert!(authentication_error(&Failed(Some(Failure::ServiceUnavailable)), AccountAction::Register).contains("try again later"));
    }

    #[test]
    fn wifi_recovery_does_not_require_an_existing_internet_connection() {
        assert!(wifi_error_message(&Failed(Some(Failure::Wifi(WifiFailure::AuthenticationFailed)))).contains("Check the network password"));
        assert!(wifi_error_message(&Failed(Some(Failure::Wifi(WifiFailure::ScanTimedOut)))).contains("Tap Refresh"));
        let message = wifi_error_message(&Failed(None));
        assert!(!message.contains("private"));
        assert!(message.contains("restart Bokheim"));
    }
}
