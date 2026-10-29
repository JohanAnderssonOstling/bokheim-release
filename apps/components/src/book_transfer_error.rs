//! Actionable messages for book downloads and imports, worded from how the
//! transfer failed. Diagnostics stay in the logs where they arose.

use operation_failure::Failure;

pub fn book_transfer_error_message(failure: Option<Failure>) -> &'static str {
    match failure {
        Some(Failure::DrmProtected) => "This file is not supported. Bokheim cannot read DRM-protected downloads.",
        Some(Failure::StorageFull) => "There isn't enough free space to save this book. Free up some space on the device or drive that holds your library, then try again.",
        Some(Failure::CloudStorageFull) => "Your cloud storage account is full. Free up space in that account, then retry. Your local books have been kept.",
        Some(Failure::ReadOnlyStorage) => "Your library is read-only. Choose a writable library location or allow changes to it, then try again.",
        Some(Failure::PermissionDenied) => "Bokheim can't save files in your library. Check the folder's permissions or choose another library location, then try again.",
        Some(Failure::NotFound) => "This book is no longer available at its download link. Try another edition or check the source website.",
        Some(Failure::Unauthorized | Failure::SignInRequired | Failure::CatalogCredentialsRejected) => "The source requires you to sign in. Sign in to the catalog, then try again.",
        Some(Failure::Forbidden) => "The source is blocking this download. Try downloading from its website or choose another edition.",
        Some(Failure::RateLimited | Failure::ServiceUnavailable) => "The download service is temporarily unavailable. Please try again later.",
        Some(Failure::TimedOut) => "The download took too long. Check your internet connection, then try again.",
        Some(Failure::Offline) => "The download couldn't finish. Check your internet connection, then try again.",
        Some(Failure::UnreadableBook | Failure::NotACatalog) => "This book's file couldn't be opened. Try downloading it again or choose another edition.",
        Some(Failure::LibraryFolderUnavailable) => "The library folder isn't available. Reconnect its drive or choose an available library, then try again.",
        _ => "The book couldn't be downloaded or saved. Please try again. If it keeps failing, try another edition.",
    }
}
