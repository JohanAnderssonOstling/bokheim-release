use client_platform_runtime::connectivity::Connectivity;

pub(super) fn observe() -> Result<Connectivity, String> {
    #[cfg(all(feature = "kobo", target_os = "linux"))]
    {
        client_platform_kobo::device::wifi::subscribe_connectivity().map(Connectivity::new).map_err(|error| error.to_string())
    }
    #[cfg(not(all(feature = "kobo", target_os = "linux")))]
    Ok(Connectivity::unrestricted())
}
