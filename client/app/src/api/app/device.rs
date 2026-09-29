use super::AppClient;

impl AppClient {
    #[cfg(all(feature = "kobo", target_os = "linux"))]
    pub async fn kobo_bluetooth(&self, request: client_platform_kobo::device::bluetooth::Request) -> Result<client_platform_kobo::device::bluetooth::Status, String> {
        self.transport
            .run(move |_| async move { crate::executor::run_blocking(move || client_platform_kobo::device::bluetooth::request(request).map_err(crate::BackendError::operation)).await.map_err(crate::BackendError::operation)? })
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn kobo_natural_light(&self) -> Result<Option<u8>, String> {
        self.transport.run_ordered(|_| client_platform_kobo::device::natural_light::read().map_err(crate::BackendError::operation)).await.map_err(|error| error.to_string())
    }
    pub async fn set_kobo_natural_light(&self, percent: u8) -> Result<u8, String> {
        self.transport.run_ordered(move |_| client_platform_kobo::device::natural_light::set(percent).map_err(crate::BackendError::operation)).await.map_err(|error| error.to_string())
    }

    pub async fn kobo_battery(&self) -> Result<client_platform_kobo::device::battery::BatteryStatus, String> {
        self.transport.run_ordered(|_| client_platform_kobo::device::battery::read().map_err(crate::BackendError::operation)).await.map_err(|error| error.to_string())
    }

    /// Runs serialized radio work on its own worker, independent of library jobs.
    #[cfg(target_os = "linux")]
    pub async fn kobo_wifi(&self, request: client_platform_kobo::device::wifi::Request) -> Result<client_platform_kobo::device::wifi::Snapshot, String> {
        client_platform_kobo::device::wifi::request(request).await
    }
    /// Reads the Kobo frontlight percentage without performing device I/O on the UI thread.
    pub async fn kobo_brightness(&self) -> Result<u8, String> {
        self.transport.run_ordered(|_| client_platform_kobo::device::brightness::read().map_err(crate::BackendError::operation)).await.map_err(|error| error.to_string())
    }

    /// Applies frontlight intensity in worker receive order and returns the driver readback.
    /// Zero switches the light off. Color temperature remains unchanged.
    pub async fn set_kobo_brightness(&self, percent: u8) -> Result<u8, String> {
        self.transport.run_ordered(move |_| client_platform_kobo::device::brightness::set(percent).map_err(crate::BackendError::operation)).await.map_err(|error| error.to_string())
    }
}
