//! Account boundary for identity, HTTP transport, and persistence.
//!
//! The implementation is grouped into modules so alternate transports and
//! persistence backends can be added without creating another composition
//! crate. The public traits in [`core`] remain the boundary consumed by the
//! synchronization server.

pub mod core;
pub mod http;
pub mod postgres;

pub use core::{AccountEmailKind, AccountEmailSender, AccountSession, AuthAttempt, AuthError, AuthenticatedUser, IssuedSession, QueuedAccountEmail, SessionRevocation};
pub use http::{auth_error, ClientAddressSource};
pub use postgres::{AccountTokenKey, PostgresAccountService};
