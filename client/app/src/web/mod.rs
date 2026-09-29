//! Browser adapters for backend services. Application worker topology lives in
//! apps/web-backend-worker; reusable browser mechanisms live in client-platform-web.
#![cfg(target_arch = "wasm32")]

mod background;
#[cfg(feature = "web-runtime-tests")]
pub use client_platform_web::transport::bridge;
#[cfg(not(feature = "web-runtime-tests"))]
pub(crate) use client_platform_web::transport::bridge;
pub(crate) mod requests;
mod tab_host;
pub use tab_host::{WebConfiguration, WebConnection};
mod import_queue;
pub mod import_worker;
use client_platform_web::transport::cpu;
pub(crate) use client_platform_web::transport::cpu_client;
pub use client_platform_web::transport::{BytesFuture, BytesOperation};
#[cfg(feature = "web-runtime-tests")]
pub use cpu_client::request as cpu_request;
#[cfg(not(feature = "web-runtime-tests"))]
pub(crate) use cpu_client::request as cpu_request;
pub mod book_source;
pub use client_platform_web::transport::network_client;
pub use client_platform_web::transport::network_server;
mod sync;
use client_platform_web::transport::transfer;
pub use import_queue::ImportQueue;
pub use {
    background::BackgroundService,
    cpu::CpuService,
    sync::{SyncOptions, SyncService},
    transfer::TransferService,
};

#[cfg(feature = "web-runtime-tests")]
pub use client_platform_web::transport::ClientRelay;
use client_platform_web::transport::{byte_reply, error_message, field, Port};

#[cfg(feature = "web-runtime-tests")]
pub use import_queue::contract as import_queue_contract;

#[cfg(feature = "web-runtime-tests")]
pub use client_platform_web::transport::port_lifecycle_contract;

#[cfg(feature = "web-runtime-tests")]
pub use sync::cancellation_contract as sync_cancellation_contract;

pub(crate) use client_platform_web::transport::object;

#[cfg(feature = "web-runtime-tests")]
pub use requests::subscription_contract;
