//! Transfer execution: the worker snapshots, drains, commits.
mod authentication;
mod coordination;
mod executor;
mod worker;

use authentication::RefreshScope;
pub use worker::TransferWorker;

type TransferRunResult = Result<(), crate::TransferError>;

#[cfg(test)]
mod tests;
