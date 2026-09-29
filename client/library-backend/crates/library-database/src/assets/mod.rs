//! Asset transfer planning, transfer commits, and operation status.

mod candidates;
mod downloads;
mod operations;
mod planning;
mod presence;
mod transfer;
mod uploads;

pub use operations::{OperationActivity, OperationStatus};
pub use planning::{AssetPlanningPage, BookUploadCompletion, BookUploadSnapshot, RejectedAsset};
pub use transfer::{AssetTransferState, PlacementTransferEntry, TransferOutcome, TransferSnapshot, TransferWrite};

use include_sqlite_sql::include_sql;

use crate::DatabaseError;

pub(super) fn unix_secs() -> Result<i64, DatabaseError> {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|duration| duration.as_secs() as i64).map_err(DatabaseError::operation)
}

include_sql!("src/assets/sql/schema.sql");
include_sql!("src/assets/sql/transfers.sql");
include_sql!("src/assets/sql/operations.sql");
