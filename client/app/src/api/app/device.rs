use crate::BackendError;
use super::AppClient;

impl AppClient {
    #[cfg(all(feature = "kobo", target_os = "linux"))]
    pub async fn kobo_bluetooth(&self, request: client_platform_kobo::device::bluetooth::Request) -> Result<client_platform_kobo::device::bluetooth::Status, BackendError> {
        self.transport
            .run(move |_| async move { crate::executor::run_blocking(move || client_platform_kobo::device::bluetooth::request(request).map_err(BackendError::operation)).await.map_err(BackendError::operation)? })
            .await
    }

    pub async fn kobo_natural_light(&self) -> Result<Option<u8>, BackendError> {
        self.transport.run_ordered(|_| client_platform_kobo::device::natural_light::read().map_err(BackendError::operation)).await
    }
    pub async fn set_kobo_natural_light(&self, percent: u8) -> Result<u8, BackendError> {
        self.transport.run_ordered(move |_| client_platform_kobo::device::natural_light::set(percent).map_err(BackendError::operation)).await
    }

    pub async fn kobo_battery(&self) -> Result<client_platform_kobo::device::battery::BatteryStatus, BackendError> {
        self.transport.run_ordered(|_| client_platform_kobo::device::battery::read().map_err(BackendError::operation)).await
    }

    pub fn subscribe_kobo_battery(&self) -> tokio::sync::watch::Receiver<Option<super::BatteryReading>> {
        client_platform_kobo::device::battery::subscribe()
    }

    /// Observes serialized radio work without starting a UI polling loop.
    #[cfg(target_os = "linux")]
    pub fn subscribe_kobo_wifi(&self) -> Result<tokio::sync::watch::Receiver<client_platform_kobo::device::wifi::Snapshot>, BackendError> {
        client_platform_kobo::device::wifi::subscribe().map_err(BackendError::operation)
    }

    /// Runs serialized radio work on its own worker, independent of library jobs.
    #[cfg(target_os = "linux")]
    pub async fn kobo_wifi(&self, request: client_platform_kobo::device::wifi::Request) -> Result<client_platform_kobo::device::wifi::Snapshot, BackendError> {
        client_platform_kobo::device::wifi::request(request).await.map_err(|error| match error.failure {
            Some(failure) => BackendError::classified(crate::Failure::Wifi(failure), error.message),
            None => BackendError::message(error.message),
        })
    }
    /// Reads the Kobo frontlight percentage without performing device I/O on the UI thread.
    pub async fn kobo_brightness(&self) -> Result<u8, BackendError> {
        self.transport.run_ordered(|_| client_platform_kobo::device::brightness::read().map_err(BackendError::operation)).await
    }

    /// Applies frontlight intensity in worker receive order and returns the driver readback.
    /// Zero switches the light off. Color temperature remains unchanged.
    pub async fn set_kobo_brightness(&self, percent: u8) -> Result<u8, BackendError> {
        self.transport.run_ordered(move |_| client_platform_kobo::device::brightness::set(percent).map_err(BackendError::operation)).await
    }
}
