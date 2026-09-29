//! Shared command replies, errors, and wire encoding for client hosts.
mod error;
pub mod reply;
pub mod wire;
pub use error::{BackendError, ErrorCause, ErrorReport};
