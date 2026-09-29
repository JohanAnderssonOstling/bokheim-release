//! Durable import pipeline: staged execution, journals, status, and wire decoding.
//!
//! The pipeline is storage- and host-agnostic. Callers supply
//! [`staged_import::Storage`] and [`staged_import::Library`] implementations;
//! the pipeline owns checkpointing, progress cadence, and retry policy.
pub mod import_journal;
pub mod import_progress_wire;
pub mod import_status;
pub mod import_workflow;
pub mod staged_import;

pub use import_status::{ImportFailure, ImportFailureKind, ImportProgress, ImportState};
