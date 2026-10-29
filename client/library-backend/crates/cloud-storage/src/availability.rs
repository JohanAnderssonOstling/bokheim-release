//! Device capability shared by provider connections and the application UI.

use crate::CloudError;
#[cfg(target_os = "android")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "android")]
static GOOGLE_PLAY_SERVICES_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[cfg(not(feature = "google-drive"))]
pub fn google_drive_available() -> bool {
    false
}

#[cfg(feature = "google-drive")]
pub fn google_drive_available() -> bool {
    #[cfg(target_os = "android")]
    {
        GOOGLE_PLAY_SERVICES_AVAILABLE.load(Ordering::Acquire)
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// The Android launcher publishes the SDK's availability check before opening
/// the backend, and refreshes it when the activity resumes.
#[cfg(target_os = "android")]
pub fn set_google_play_services_available(available: bool) {
    GOOGLE_PLAY_SERVICES_AVAILABLE.store(available, Ordering::Release);
}

pub fn require_google_drive() -> Result<(), CloudError> {
    if google_drive_available() { Ok(()) } else { Err(unavailable()) }
}

pub(crate) fn unavailable() -> CloudError {
    #[cfg(not(feature = "google-drive"))]
    return CloudError::InvalidRequest("Google Drive is not enabled in this build".into());
    #[cfg(feature = "google-drive")]
    CloudError::InvalidRequest("Google Drive requires enabled, up-to-date Google Play services on this device".into())
}
