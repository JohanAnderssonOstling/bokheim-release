mod account;
mod client;
#[cfg(all(feature = "kobo", not(target_arch = "wasm32")))]
mod device;
mod libraries;
mod preferences;
mod sync;

pub use client::AppClient;
#[cfg(all(feature = "kobo", target_os = "linux"))]
pub use client_platform_kobo::device::bluetooth::{Device as BluetoothDevice, Request as BluetoothRequest, Status as BluetoothStatus};
#[cfg(all(feature = "kobo", target_os = "linux"))]
pub use client_platform_kobo::device::wifi::{Network as WifiNetwork, Phase as WifiPhase, Request as WifiRequest, Security as WifiSecurity, Snapshot as WifiSnapshot};

#[cfg(all(feature = "kobo", not(target_arch = "wasm32")))]
pub use client_platform_kobo::device::battery::BatteryStatus;
